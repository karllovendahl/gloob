use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::{
    cache,
    config::Action,
    preview::{self, Animation},
    provider::{Gif, GifSnap, Page},
};

pub const COLUMNS: usize = 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Retry {
    Search(Option<String>),
    Copy,
}

#[derive(Default)]
pub struct State {
    pub generation: u64,
    pub query: String,
    pub items: Vec<Gif>,
    pub selected: usize,
    pub cursor: Option<String>,
    pub loading: bool,
    pub copying: bool,
    pub error: Option<String>,
    pub retry: Option<Retry>,
    pub requested_cursor: Option<String>,
    automatic_more: bool,
}

impl State {
    pub fn change_query(&mut self, query: String) -> u64 {
        self.generation += 1;
        self.query = query;
        self.items.clear();
        self.selected = 0;
        self.cursor = None;
        self.loading = true;
        self.error = None;
        self.retry = None;
        self.requested_cursor = None;
        self.automatic_more = false;
        self.generation
    }

    pub fn begin_search(&mut self, cursor: Option<String>) -> bool {
        if self.copying || (self.loading && cursor.is_some()) {
            return false;
        }
        if cursor.is_none() {
            self.generation += 1;
            self.items.clear();
            self.selected = 0;
            self.cursor = None;
            self.automatic_more = false;
        }
        self.loading = true;
        self.requested_cursor = cursor;
        self.error = None;
        self.retry = None;
        true
    }

    pub fn accept_page(&mut self, generation: u64, result: Result<Page>) -> bool {
        if generation != self.generation || !self.loading {
            return false;
        }
        self.loading = false;
        // a late page must not replace a failed copy's message or retry action.
        let copy_error = self.retry == Some(Retry::Copy);
        match result {
            Ok(page) => {
                let mut ids: std::collections::HashSet<_> =
                    self.items.iter().map(|item| item.id.clone()).collect();
                let before = self.items.len();
                self.items.extend(
                    page.items
                        .into_iter()
                        .filter(|item| ids.insert(item.id.clone())),
                );
                self.cursor = page.cursor;
                self.automatic_more = self.items.len() > before;
                if !copy_error {
                    self.error = None;
                    self.retry = None;
                }
            }
            Err(error) => {
                self.automatic_more = false;
                if !copy_error {
                    self.error = Some(format!("{error:#}"));
                    self.retry = Some(Retry::Search(self.requested_cursor.clone()));
                }
            }
        }
        true
    }

    pub fn automatic_page_cursor(&self) -> Option<String> {
        if !self.automatic_more || self.retry.is_some() || self.items.is_empty() {
            return None;
        }
        self.next_page_cursor()
    }

    pub fn navigate(&mut self, action: Action) {
        if self.items.is_empty() || self.copying {
            return;
        }
        let last = self.items.len() - 1;
        self.selected = match action {
            Action::MoveLeft => {
                if self.selected.is_multiple_of(COLUMNS) {
                    self.selected
                } else {
                    self.selected - 1
                }
            }
            Action::MoveRight => {
                if self.selected % COLUMNS == COLUMNS - 1 {
                    self.selected
                } else {
                    (self.selected + 1).min(last)
                }
            }
            Action::MoveUp => self.selected.saturating_sub(COLUMNS),
            Action::MoveDown => (self.selected + COLUMNS).min(last),
            _ => self.selected,
        };
    }

    pub fn next_page_cursor(&self) -> Option<String> {
        if self.loading || self.copying {
            return None;
        }
        self.cursor.clone()
    }

    pub fn fail_copy(&mut self, error: anyhow::Error) {
        self.copying = false;
        self.error = Some(format!("{error:#}"));
        self.retry = Some(Retry::Copy);
    }

    pub fn begin_copy(&mut self) -> Option<Gif> {
        if self.copying {
            return None;
        }
        let gif = self.items.get(self.selected)?.clone();
        self.copying = true;
        self.error = None;
        self.retry = None;
        Some(gif)
    }
}

pub enum Message {
    Page(u64, Result<Page>),
    Copied(Result<Option<String>>),
}

pub struct PreviewMessage {
    pub generation: u64,
    pub index: usize,
    pub result: Result<Animation>,
}

// keep decoded deliveries bounded separately from lightweight search/copy messages.
const READY_PREVIEWS: usize = 4;

enum Job {
    Search(u64, String, Option<String>),
    Preview(u64, usize, Box<Gif>),
}

pub struct Workers {
    jobs: mpsc::SyncSender<Job>,
    copy: mpsc::SyncSender<Gif>,
    pub messages: mpsc::Receiver<Message>,
    pub preview_messages: mpsc::Receiver<PreviewMessage>,
    generation: Arc<AtomicU64>,
    api_times: Mutex<Vec<Instant>>,
}

impl Workers {
    pub fn new() -> Result<Self> {
        Self::with_api(GifSnap::new()?)
    }

    fn with_api(api: GifSnap) -> Result<Self> {
        let (send, messages) = mpsc::channel();
        let (previews, preview_messages) = mpsc::sync_channel(READY_PREVIEWS);
        let (jobs, receive) = mpsc::sync_channel::<Job>(32);
        let receive = Arc::new(Mutex::new(receive));
        let generation = Arc::new(AtomicU64::new(0));
        for _ in 0..4 {
            let receive = receive.clone();
            let generation = generation.clone();
            let send = send.clone();
            let api = api.clone();
            let previews = previews.clone();
            std::thread::spawn(move || {
                loop {
                    let job = receive.lock().unwrap().recv();
                    let Ok(job) = job else {
                        break;
                    };
                    let expected = match &job {
                        Job::Search(id, _, _) | Job::Preview(id, _, _) => *id,
                    };
                    if generation.load(Ordering::Relaxed) != expected {
                        continue;
                    }
                    let message = match job {
                        Job::Search(id, query, cursor) => {
                            Message::Page(id, api.search(&query, cursor.as_deref()))
                        }
                        Job::Preview(id, index, gif) => {
                            let result = load_preview(&api, &gif);
                            if generation.load(Ordering::Relaxed) == id
                                && previews
                                    .send(PreviewMessage {
                                        generation: id,
                                        index,
                                        result,
                                    })
                                    .is_err()
                            {
                                break;
                            }
                            continue;
                        }
                    };
                    if generation.load(Ordering::Relaxed) == expected && send.send(message).is_err()
                    {
                        break;
                    }
                }
            });
        }
        let (copy, copy_receive) = mpsc::sync_channel::<Gif>(1);
        std::thread::spawn(move || {
            while let Ok(gif) = copy_receive.recv() {
                let result = api
                    .download(&gif)
                    .and_then(|bytes| cache::copy_bytes(&bytes));
                if send.send(Message::Copied(result)).is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            jobs,
            copy,
            messages,
            preview_messages,
            generation,
            api_times: Mutex::new(Vec::new()),
        })
    }

    pub fn invalidate(&self, generation: u64) {
        self.generation.store(generation, Ordering::Relaxed);
    }

    pub fn search(&self, generation: u64, query: String, cursor: Option<String>) -> Result<()> {
        // this is our per-process safety policy, not a promised gifsnap quota. do not silently queue retries.
        let mut times = self.api_times.lock().unwrap();
        let now = Instant::now();
        times.retain(|time| now.duration_since(*time) < Duration::from_secs(60));
        anyhow::ensure!(
            times.len() < 40,
            "search limit reached; wait a minute and retry"
        );
        self.jobs
            .try_send(Job::Search(generation, query, cursor))
            .map_err(|_| anyhow::anyhow!("network workers are busy; retry shortly"))?;
        times.push(now);
        Ok(())
    }

    pub fn preview(&self, generation: u64, index: usize, gif: Gif) -> bool {
        self.jobs
            .try_send(Job::Preview(generation, index, Box::new(gif)))
            .is_ok()
    }

    pub fn copy(&self, gif: Gif) -> Result<()> {
        self.copy
            .try_send(gif)
            .map_err(|_| anyhow::anyhow!("copy worker is busy"))
    }
}

pub(crate) fn load_preview(api: &GifSnap, gif: &Gif) -> Result<Animation> {
    if gif.preview_url.is_some()
        && let Ok(animation) = api.preview(gif).and_then(|bytes| preview::decode(&bytes))
    {
        return Ok(animation);
    }
    api.download(gif).and_then(|bytes| preview::decode(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use url::Url;

    fn gif(index: usize) -> Gif {
        let url = Url::parse("https://media.gifsnap.com/a.gif").unwrap();
        Gif {
            id: index.to_string(),
            title: String::new(),
            page_url: url.clone(),
            preview_url: None,
            download_url: url,
            source: None,
            content_id: None,
        }
    }

    #[test]
    fn saturated_preview_delivery_stays_bounded_without_blocking_copy_results() {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpListener;
        use std::sync::atomic::AtomicBool;
        let listener = TcpListener::bind("localhost:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let done = stop.clone();
        let (requests, observed) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let bytes = crate::test_media::animation(2, 1, 2);
            while !done.load(Ordering::Relaxed) {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("{error}"),
                };
                let mut reader = BufReader::new(&stream);
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                }
                let copy = first.contains("/copy ");
                let body = if copy {
                    b"invalid media".as_slice()
                } else {
                    &bytes
                };
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(body).unwrap();
                if !copy {
                    requests.send(()).unwrap();
                }
            }
        });
        let workers = Workers::with_api(GifSnap::with_base(base.clone()).unwrap()).unwrap();
        workers.invalidate(1);
        let mut asset = gif(0);
        asset.download_url = base.join("preview").unwrap();
        for index in 0..20 {
            assert!(workers.preview(1, index, asset.clone()));
        }
        // four ready deliveries plus four blocked workers: no unbounded decoded-message queue.
        for _ in 0..8 {
            observed.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        assert!(observed.recv_timeout(Duration::from_millis(100)).is_err());
        asset.download_url = base.join("copy").unwrap();
        workers.copy(asset).unwrap();
        // invalid bytes fail before cache/clipboard access, but the independent copy result arrives.
        assert!(matches!(
            workers
                .messages
                .recv_timeout(Duration::from_secs(5))
                .unwrap(),
            Message::Copied(Err(_))
        ));
        assert!(observed.try_recv().is_err());
        drop(workers);
        stop.store(true, Ordering::Relaxed);
        server.join().unwrap();
    }

    #[test]
    fn stale_search_cannot_replace_newer_items_or_errors() {
        let mut state = State::default();
        let old = state.change_query("old".into());
        let new = state.change_query("new".into());
        assert!(!state.accept_page(
            old,
            Ok(Page {
                items: vec![gif(1)],
                cursor: None
            })
        ));
        assert!(state.accept_page(
            new,
            Ok(Page {
                items: vec![gif(2)],
                cursor: None
            })
        ));
        assert!(!state.accept_page(old, Err(anyhow::anyhow!("late error"))));
        assert_eq!(state.items[0].id, "2");
        assert!(state.error.is_none());
    }

    #[test]
    fn next_page_requires_a_cursor_and_an_idle_picker() {
        let mut state = State::default();
        assert!(state.next_page_cursor().is_none());
        state.cursor = Some("page-two".into());
        assert_eq!(state.next_page_cursor().as_deref(), Some("page-two"));
        state.loading = true;
        assert!(state.next_page_cursor().is_none());
        state.loading = false;
        state.copying = true;
        assert!(state.next_page_cursor().is_none());
        state.copying = false;
        state.change_query("different query".into());
        assert!(state.next_page_cursor().is_none());
    }

    #[test]
    fn failed_pagination_keeps_retry_cursor_until_query_changes() {
        let mut state = State {
            generation: 1,
            requested_cursor: Some("later+page".into()),
            loading: true,
            ..State::default()
        };
        state.accept_page(1, Err(anyhow::anyhow!("temporary failure")));
        assert_eq!(state.retry, Some(Retry::Search(Some("later+page".into()))));
        state.change_query("new query".into());
        assert!(state.retry.is_none());
        assert!(state.requested_cursor.is_none());
    }

    #[test]
    fn navigation_stays_in_bounds_and_does_not_wrap_rows() {
        let mut state = State {
            items: (0..6).map(gif).collect(),
            ..State::default()
        };
        state.navigate(Action::MoveLeft);
        assert_eq!(state.selected, 0);
        state.navigate(Action::MoveDown);
        assert_eq!(state.selected, 4);
        state.navigate(Action::MoveRight);
        assert_eq!(state.selected, 5);
        state.navigate(Action::MoveDown);
        assert_eq!(state.selected, 5);
        state.navigate(Action::MoveUp);
        assert_eq!(state.selected, 1);
        state.selected = 3;
        state.navigate(Action::MoveRight);
        assert_eq!(state.selected, 3);
    }

    fn first_page() -> State {
        let mut state = State::default();
        assert!(state.begin_search(None));
        assert!(state.accept_page(
            state.generation,
            Ok(Page {
                items: (0..24).map(gif).collect(),
                cursor: Some("2".into())
            })
        ));
        state
    }

    #[test]
    fn appending_preserves_selection_order_and_generation_without_duplicates() {
        let mut state = first_page();
        state.selected = 20;
        let generation = state.generation;
        assert!(state.begin_search(state.next_page_cursor()));
        assert!(!state.begin_search(Some("2".into())));
        assert!(state.automatic_page_cursor().is_none());
        state.navigate(Action::MoveRight);
        assert!(state.accept_page(
            generation,
            Ok(Page {
                items: vec![gif(23), gif(24), gif(24), gif(25)],
                cursor: Some("3".into())
            })
        ));
        assert_eq!(state.generation, generation);
        assert_eq!(state.selected, 21);
        assert_eq!(
            state
                .items
                .iter()
                .map(|gif| gif.id.as_str())
                .collect::<Vec<_>>(),
            (0..26).map(|index| index.to_string()).collect::<Vec<_>>()
        );
        assert_eq!(state.automatic_page_cursor().as_deref(), Some("3"));
    }

    #[test]
    fn failed_more_keeps_results_and_blocks_automatic_retries_until_explicit_success() {
        let mut state = first_page();
        state.selected = 21;
        state.begin_search(Some("2".into()));
        state.accept_page(state.generation, Err(anyhow::anyhow!("429 wait")));
        assert_eq!(state.items.len(), 24);
        assert_eq!(state.selected, 21);
        assert!(state.automatic_page_cursor().is_none());
        assert_eq!(state.retry, Some(Retry::Search(Some("2".into()))));
        let Some(Retry::Search(cursor)) = state.retry.clone() else {
            panic!()
        };
        assert!(state.begin_search(cursor));
        state.accept_page(
            state.generation,
            Ok(Page {
                items: vec![gif(24)],
                cursor: None,
            }),
        );
        assert_eq!(state.items.len(), 25);
        assert!(state.retry.is_none() && state.next_page_cursor().is_none());
    }

    #[test]
    fn empty_and_duplicate_only_pages_stop_automatic_fetching_but_allow_manual_more() {
        for items in [vec![], vec![gif(0), gif(1)]] {
            let mut state = first_page();
            state.begin_search(Some("2".into()));
            state.accept_page(
                state.generation,
                Ok(Page {
                    items,
                    cursor: Some("3".into()),
                }),
            );
            assert_eq!(state.items.len(), 24);
            assert!(state.automatic_page_cursor().is_none());
            assert_eq!(state.next_page_cursor().as_deref(), Some("3"));
        }
    }

    #[test]
    fn new_query_clears_appended_results_and_rejects_old_completions() {
        let mut state = first_page();
        let old = state.generation;
        state.begin_search(Some("2".into()));
        state.accept_page(
            old,
            Ok(Page {
                items: vec![gif(24)],
                cursor: Some("3".into()),
            }),
        );
        state.begin_search(Some("3".into()));
        state.change_query("different".into());
        assert!(state.items.is_empty() && state.cursor.is_none());
        assert!(!state.accept_page(
            old,
            Ok(Page {
                items: vec![gif(25)],
                cursor: None
            })
        ));
        assert!(!state.accept_page(old, Err(anyhow::anyhow!("late error"))));
        assert!(state.items.is_empty() && state.error.is_none());
    }

    #[test]
    fn late_more_during_copy_or_after_failure_does_not_break_selection_or_retry() {
        for fail_before_page in [true, false] {
            let mut state = first_page();
            state.selected = 5;
            state.begin_search(Some("2".into()));
            assert_eq!(state.begin_copy().unwrap().id, "5");
            if fail_before_page {
                state.fail_copy(anyhow::anyhow!("copy failed"));
            }
            assert!(state.accept_page(
                state.generation,
                Ok(Page {
                    items: vec![gif(24)],
                    cursor: Some("3".into())
                })
            ));
            assert!(!state.loading);
            assert_eq!(state.items.len(), 25);
            assert_eq!(state.selected, 5);
            if !fail_before_page {
                state.fail_copy(anyhow::anyhow!("copy failed"));
            }
            assert_eq!(state.retry, Some(Retry::Copy));
            assert_eq!(state.error.as_deref(), Some("copy failed"));
            assert!(state.automatic_page_cursor().is_none());
            assert_eq!(state.begin_copy().unwrap().id, "5");
        }
        let mut state = first_page();
        state.begin_search(Some("2".into()));
        state.begin_copy();
        state.fail_copy(anyhow::anyhow!("copy failed"));
        state.accept_page(state.generation, Err(anyhow::anyhow!("page failed")));
        assert!(!state.loading);
        assert_eq!(state.retry, Some(Retry::Copy));
        assert_eq!(state.error.as_deref(), Some("copy failed"));
        assert_eq!(state.items.len(), 24);
    }

    #[test]
    fn copying_snapshots_selection_and_blocks_duplicate_activation() {
        let mut state = State {
            items: vec![gif(1), gif(2)],
            ..State::default()
        };
        assert_eq!(state.begin_copy().unwrap().id, "1");
        state.navigate(Action::MoveRight);
        assert_eq!(state.selected, 0);
        assert!(state.begin_copy().is_none());
        assert!(!state.accept_page(
            0,
            Ok(Page {
                items: vec![gif(3)],
                cursor: None
            })
        ));
        state.fail_copy(anyhow::anyhow!("download failed"));
        assert_eq!(state.retry, Some(Retry::Copy));
        assert_eq!(state.begin_copy().unwrap().id, "1");
    }
}
