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
    provider::{Gif, GifJif, Page},
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
        self.generation
    }

    pub fn accept_page(&mut self, generation: u64, result: Result<Page>) -> bool {
        if generation != self.generation || self.copying {
            return false;
        }
        self.loading = false;
        match result {
            Ok(page) => {
                self.items = page.items;
                self.cursor = page.cursor;
                self.selected = 0;
                self.error = None;
                self.retry = None;
            }
            Err(error) => {
                self.error = Some(format!("{error:#}"));
                self.retry = Some(Retry::Search(self.requested_cursor.clone()));
            }
        }
        true
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
    Preview(u64, usize, Result<Animation>),
    Copied(Result<Option<String>>),
}

enum Job {
    Search(u64, String, Option<String>),
    Preview(u64, usize, Box<Gif>),
}

pub struct Workers {
    jobs: mpsc::SyncSender<Job>,
    copy: mpsc::SyncSender<Gif>,
    pub messages: mpsc::Receiver<Message>,
    generation: Arc<AtomicU64>,
    api_times: Mutex<Vec<Instant>>,
}

impl Workers {
    pub fn new() -> Result<Self> {
        let api = GifJif::new()?;
        let (send, messages) = mpsc::channel();
        let (jobs, receive) = mpsc::sync_channel::<Job>(32);
        let receive = Arc::new(Mutex::new(receive));
        let generation = Arc::new(AtomicU64::new(0));
        for _ in 0..4 {
            let receive = receive.clone();
            let generation = generation.clone();
            let send = send.clone();
            let api = api.clone();
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
                        Job::Preview(id, index, gif) => Message::Preview(
                            id,
                            index,
                            api.preview(&gif).and_then(|bytes| preview::decode(&bytes)),
                        ),
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
            generation,
            api_times: Mutex::new(Vec::new()),
        })
    }

    pub fn invalidate(&self, generation: u64) {
        self.generation.store(generation, Ordering::Relaxed);
    }

    pub fn search(&self, generation: u64, query: String, cursor: Option<String>) -> Result<()> {
        // keep a margin under the shared per-ip allowance; do not retry or silently queue rate-limited searches.
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

#[cfg(test)]
mod tests {
    use super::*;
    use url::Url;

    fn gif(index: usize) -> Gif {
        let url = Url::parse("https://gifjif.com/a.gif").unwrap();
        Gif {
            id: index.to_string(),
            title: String::new(),
            page_url: url.clone(),
            preview_url: url.clone(),
            download_url: url,
        }
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
    fn failed_pagination_keeps_retry_cursor_until_query_changes() {
        let mut state = State {
            generation: 1,
            requested_cursor: Some("later+page".into()),
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
