use std::io::Read;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use reqwest::blocking::{Client, Response};
use serde::Deserialize;
use url::Url;

const API_LIMIT_BYTES: u64 = 2 * 1024 * 1024;
const GIF_LIMIT_BYTES: u64 = 25 * 1024 * 1024;
const PREVIEW_LIMIT_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Gif {
    pub id: String,
    pub title: String,
    pub page_url: Url,
    pub preview_url: Url,
    pub download_url: Url,
}

#[derive(Debug)]
pub struct Page {
    pub items: Vec<Gif>,
    pub cursor: Option<String>,
}

#[derive(Clone)]
pub struct GifJif {
    client: Client,
    base: Url,
}

impl GifJif {
    pub fn new() -> Result<Self> {
        Self::with_base(Url::parse("https://gifjif.com/")?)
    }

    fn with_base(base: Url) -> Result<Self> {
        let client = Client::builder()
            .user_agent("gloob/0.1")
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("could not create the gifjif client")?;
        Ok(Self { client, base })
    }

    pub fn search(&self, query: &str, cursor: Option<&str>) -> Result<Page> {
        let query = query.trim();
        let endpoint = if query.is_empty() {
            "api/feed"
        } else {
            "api/search"
        };
        let mut url = self.base.join(endpoint)?;
        {
            let mut params = url.query_pairs_mut();
            params.append_pair("limit", "24");
            if !query.is_empty() {
                params.append_pair("q", query);
                params.append_pair("sort", "top");
            }
            if let Some(cursor) = cursor {
                params.append_pair("cursor", cursor);
            }
        }
        let response = self
            .client
            .get(url)
            .send()
            .context("could not reach gifjif")?;
        check_status(&response)?;
        let bytes = read_limited(response, API_LIMIT_BYTES)?;
        let wire: WirePage =
            serde_json::from_slice(&bytes).context("gifjif returned an invalid response")?;
        ensure!(wire.ok, "gifjif reported an unsuccessful request");
        ensure!(wire.items.len() <= 30, "gifjif returned too many results");
        let mut items = Vec::with_capacity(wire.items.len());
        for item in wire.items {
            ensure!(!item.id.is_empty(), "gifjif returned an empty item id");
            items.push(Gif {
                id: item.id,
                title: item.title,
                page_url: media_url(&item.url)?,
                preview_url: media_url(item.media.tinygif.as_deref().unwrap_or(&item.media.gif))?,
                download_url: media_url(&item.media.gif)?,
            });
        }
        let cursor = if wire.has_more {
            Some(
                wire.cursor
                    .filter(|value| !value.is_empty())
                    .context("gifjif omitted its next-page cursor")?,
            )
        } else {
            None
        };
        Ok(Page { items, cursor })
    }

    pub fn download(&self, gif: &Gif) -> Result<Vec<u8>> {
        self.media(&gif.download_url, GIF_LIMIT_BYTES)
    }

    pub fn preview(&self, gif: &Gif) -> Result<Vec<u8>> {
        self.media(&gif.preview_url, PREVIEW_LIMIT_BYTES)
    }

    fn media(&self, url: &Url, limit: u64) -> Result<Vec<u8>> {
        let response = self
            .client
            .get(url.clone())
            .send()
            .context("could not download the gif")?;
        check_status(&response)?;
        let bytes = read_limited(response, limit)?;
        ensure!(
            bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
            "gifjif returned a non-gif asset"
        );
        Ok(bytes)
    }
}

fn media_url(value: &str) -> Result<Url> {
    let url = Url::parse(value).context("gifjif returned an invalid media url")?;
    let host = url.host_str().unwrap_or_default();
    ensure!(
        url.scheme() == "https" && (host == "gifjif.com" || host.ends_with(".gifjif.com")),
        "gifjif returned an unexpected media host"
    );
    ensure!(
        url.username().is_empty() && url.password().is_none() && url.port().is_none(),
        "gifjif returned an unsupported media url"
    );
    Ok(url)
}

fn check_status(response: &Response) -> Result<()> {
    if response.status().as_u16() == 429 {
        let seconds = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());
        if let Some(seconds) = seconds {
            bail!("gifjif rate limit reached; try again after {seconds} seconds");
        }
        bail!("gifjif rate limit reached; try again later");
    }
    ensure!(
        response.status().is_success(),
        "gifjif request failed (http {})",
        response.status().as_u16()
    );
    Ok(())
}

fn read_limited(response: Response, limit: u64) -> Result<Vec<u8>> {
    ensure!(
        response
            .content_length()
            .is_none_or(|length| length <= limit),
        "gifjif response exceeds the size limit"
    );
    let mut bytes = Vec::new();
    response
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .context("could not read the gifjif response")?;
    ensure!(
        bytes.len() as u64 <= limit,
        "gifjif response exceeds the size limit"
    );
    Ok(bytes)
}

#[derive(Deserialize)]
struct WirePage {
    ok: bool,
    items: Vec<WireGif>,
    cursor: Option<String>,
    #[serde(rename = "hasMore")]
    has_more: bool,
}

#[derive(Deserialize)]
struct WireGif {
    id: String,
    title: String,
    url: String,
    media: WireMedia,
}

#[derive(Deserialize)]
struct WireMedia {
    gif: String,
    tinygif: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    fn server(status: &str, headers: &str, body: Vec<u8>) -> (GifJif, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("localhost:0").unwrap();
        let base = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let (send, receive) = mpsc::channel();
        let status = status.to_owned();
        let headers = headers.to_owned();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            let mut reader = BufReader::new(&stream);
            reader.read_line(&mut request).unwrap();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
            }
            let _ = send.send(request);
            write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n",
                body.len()
            )
            .unwrap();
            let _ = stream.write_all(&body);
        });
        (GifJif::with_base(base).unwrap(), receive)
    }

    fn page() -> Vec<u8> {
        br#"{"ok":true,"items":[{"id":"one","title":"hello","url":"https://gifjif.com/hello","media":{"gif":"https://gifjif.com/a.gif","tinygif":"https://gifjif.com/a.tiny.gif"}}],"cursor":"next+page","hasMore":true}"#.to_vec()
    }

    #[test]
    fn encodes_search_and_cursor_without_losing_ranking() {
        let (api, request) = server("200 OK", "", page());
        let page = api.search(" hello & ü ", Some("next+page")).unwrap();
        assert_eq!(page.items[0].id, "one");
        assert_eq!(page.cursor.as_deref(), Some("next+page"));
        assert!(page.items[0].preview_url.as_str().ends_with("tiny.gif"));
        let request = request.recv().unwrap();
        let url = Url::parse(&format!(
            "http://test{}",
            request.split_whitespace().nth(1).unwrap()
        ))
        .unwrap();
        let params: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(url.path(), "/api/search");
        assert_eq!(params["q"], "hello & ü");
        assert_eq!(params["sort"], "top");
        assert_eq!(params["cursor"], "next+page");
    }

    #[test]
    fn empty_query_loads_feed_and_empty_results_are_successful() {
        let (api, request) = server(
            "200 OK",
            "",
            br#"{"ok":true,"items":[],"hasMore":false}"#.to_vec(),
        );
        let page = api.search("", None).unwrap();
        assert!(page.items.is_empty());
        assert!(page.cursor.is_none());
        assert!(request.recv().unwrap().contains("/api/feed?"));
    }

    #[test]
    fn rejects_failed_malformed_and_untrusted_responses() {
        for body in [
            b"not json".to_vec(),
            br#"{"ok":false,"items":[],"hasMore":false}"#.to_vec(),
            br#"{"ok":true,"items":[],"hasMore":true}"#.to_vec(),
        ] {
            let (api, _request) = server("200 OK", "", body);
            assert!(api.search("hello", None).is_err());
        }
        for url in [
            "http://gifjif.com/a.gif",
            "https://evil.test/a.gif",
            "https://gifjif.com:444/a.gif",
        ] {
            assert!(media_url(url).is_err());
        }
    }

    #[test]
    fn reports_rate_limit_and_http_errors_without_retrying() {
        let (api, _request) = server("429 Too Many Requests", "Retry-After: 17\r\n", Vec::new());
        assert!(
            api.search("hello", None)
                .unwrap_err()
                .to_string()
                .contains("17 seconds")
        );
        let (api, _request) = server("503 Service Unavailable", "", Vec::new());
        assert!(
            api.search("hello", None)
                .unwrap_err()
                .to_string()
                .contains("503")
        );
    }

    #[test]
    fn bounds_response_bytes_and_checks_media_format() {
        let (api, _request) = server("200 OK", "", vec![0; API_LIMIT_BYTES as usize + 1]);
        assert!(api.search("hello", None).is_err());
        let (api, _request) = server("200 OK", "", b"GIF89aabc".to_vec());
        assert!(api.media(&api.base.join("asset").unwrap(), 8).is_err());
        let (api, _request) = server("200 OK", "", b"<html>".to_vec());
        assert!(api.media(&api.base.join("asset").unwrap(), 100).is_err());
        let (api, _request) = server("200 OK", "", b"GIF89aabc".to_vec());
        assert_eq!(
            api.media(&api.base.join("asset").unwrap(), 100).unwrap(),
            b"GIF89aabc"
        );
    }
}
