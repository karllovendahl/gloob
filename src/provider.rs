use std::io::Read;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use reqwest::blocking::{Client, Response};
use serde::Deserialize;
use url::Url;

const API_LIMIT_BYTES: u64 = 2 * 1024 * 1024;
const MEDIA_LIMIT_BYTES: u64 = 25 * 1024 * 1024;
const PREVIEW_LIMIT_BYTES: u64 = 512 * 1024;
const PAGE_SIZE: usize = 24;

#[derive(Clone, Debug)]
pub struct Gif {
    pub id: String,
    pub title: String,
    pub page_url: Url,
    pub preview_url: Option<Url>,
    pub download_url: Url,
    pub source: Option<String>,
    pub content_id: Option<String>,
}

#[derive(Debug)]
pub struct Page {
    pub items: Vec<Gif>,
    pub cursor: Option<String>,
}

#[derive(Clone)]
pub struct GifSnap {
    client: Client,
    base: Url,
}

impl GifSnap {
    pub fn new() -> Result<Self> {
        Self::with_base(Url::parse("https://gifsnap.com/")?)
    }

    fn with_base(base: Url) -> Result<Self> {
        let client = Client::builder()
            .user_agent("gloob/0.1")
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("could not create the gifsnap client")?;
        Ok(Self { client, base })
    }

    pub fn search(&self, query: &str, cursor: Option<&str>) -> Result<Page> {
        let query = query.trim();
        let page = match cursor {
            Some(cursor) => cursor.parse::<u32>().context("invalid gifsnap page")?,
            None => 1,
        };
        ensure!((1..=1_000_000).contains(&page), "invalid gifsnap page");
        let endpoint = if query.is_empty() {
            "gifs/trending"
        } else {
            "gifs/search"
        };
        let mut url = self.base.join(&format!("api/v1/{endpoint}"))?;
        {
            let mut params = url.query_pairs_mut();
            params.append_pair("limit", &PAGE_SIZE.to_string());
            params.append_pair("page", &page.to_string());
            if !query.is_empty() {
                params.append_pair("q", query);
            }
        }
        let response = self
            .client
            .get(url)
            .send()
            .context("could not reach gifsnap")?;
        check_status(&response)?;
        let bytes = read_limited(response, API_LIMIT_BYTES)?;
        let wire: WirePage =
            serde_json::from_slice(&bytes).context("gifsnap returned an invalid response")?;
        ensure!(
            wire.data.len() <= PAGE_SIZE,
            "gifsnap returned too many results"
        );
        ensure!(
            wire.pagination.page == page,
            "gifsnap returned an unexpected page"
        );
        let cursor = if wire.pagination.has_next {
            let next = wire
                .pagination
                .next_page
                .context("gifsnap omitted its next page")?;
            ensure!(
                next > page && next <= 1_000_000,
                "gifsnap returned an invalid next page"
            );
            Some(next.to_string())
        } else {
            None
        };
        let mut items = Vec::with_capacity(wire.data.len());
        for item in wire.data {
            ensure!(!item.id.is_empty(), "gifsnap returned an empty item id");
            let download_url = media_url(&item.url)?;
            // static preview_url is deliberately not used as an animation.
            let preview_url = item
                .animated_preview
                .filter(|preview| {
                    preview.animated
                        && matches!(preview.mime_type.as_str(), "image/gif" | "image/webp")
                        && (1..=480).contains(&preview.width)
                        && (1..=480).contains(&preview.height)
                        && preview
                            .byte_size
                            .is_none_or(|size| (1..=PREVIEW_LIMIT_BYTES).contains(&size))
                })
                .and_then(|preview| media_url(&preview.url).ok());
            let mut page_url = self.base.join("api/v1/gifs/")?;
            page_url
                .path_segments_mut()
                .unwrap()
                .pop_if_empty()
                .push(&item.id);
            items.push(Gif {
                id: item.id,
                title: if item.title.trim().is_empty() {
                    "untitled gif".to_owned()
                } else {
                    item.title
                },
                page_url,
                preview_url,
                download_url,
                source: item.source,
                content_id: item.content_id,
            });
        }
        Ok(Page { items, cursor })
    }

    pub fn download(&self, gif: &Gif) -> Result<Vec<u8>> {
        self.media(&gif.download_url, MEDIA_LIMIT_BYTES)
    }

    pub fn preview(&self, gif: &Gif) -> Result<Vec<u8>> {
        self.media(
            gif.preview_url
                .as_ref()
                .context("no small animated preview")?,
            PREVIEW_LIMIT_BYTES,
        )
    }

    fn media(&self, url: &Url, limit: u64) -> Result<Vec<u8>> {
        let response = self
            .client
            .get(url.clone())
            .send()
            .context("could not download the media")?;
        check_status(&response)?;
        let bytes = read_limited(response, limit)?;
        crate::media::Format::detect(&bytes).context("gifsnap returned unsupported media")?;
        Ok(bytes)
    }
}

fn media_url(value: &str) -> Result<Url> {
    let url = Url::parse(value).context("gifsnap returned an invalid media url")?;
    let host = url.host_str().unwrap_or_default();
    ensure!(
        url.scheme() == "https"
            && (host == "gifsnap.com"
                || host.ends_with(".gifsnap.com")
                // gifsnap also returns direct upstream assets, not only mirrored media.
                || host == "static.klipy.com"),
        "gifsnap returned an unexpected media host"
    );
    ensure!(
        url.username().is_empty()
            && url.password().is_none()
            && url.port().is_none()
            && url.fragment().is_none(),
        "gifsnap returned an unsupported media url"
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
            bail!("gifsnap rate limit reached; try again after {seconds} seconds");
        }
        bail!("gifsnap rate limit reached; try again later");
    }
    ensure!(
        response.status().is_success(),
        "gifsnap request failed (http {})",
        response.status().as_u16()
    );
    Ok(())
}

fn read_limited(response: Response, limit: u64) -> Result<Vec<u8>> {
    ensure!(
        response
            .content_length()
            .is_none_or(|length| length <= limit),
        "gifsnap response exceeds the size limit"
    );
    let mut bytes = Vec::new();
    response
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .context("could not read the gifsnap response")?;
    ensure!(
        bytes.len() as u64 <= limit,
        "gifsnap response exceeds the size limit"
    );
    Ok(bytes)
}

#[derive(Deserialize)]
struct WirePage {
    data: Vec<WireGif>,
    pagination: WirePagination,
}

#[derive(Deserialize)]
struct WirePagination {
    page: u32,
    has_next: bool,
    next_page: Option<u32>,
}

#[derive(Deserialize)]
struct WireGif {
    id: String,
    title: String,
    url: String,
    source: Option<String>,
    content_id: Option<String>,
    animated_preview: Option<WirePreview>,
}

#[derive(Deserialize)]
struct WirePreview {
    url: String,
    mime_type: String,
    width: u32,
    height: u32,
    byte_size: Option<u64>,
    animated: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    fn server(status: &str, headers: &str, body: Vec<u8>) -> (GifSnap, mpsc::Receiver<String>) {
        response_server(status, headers, body, true)
    }

    fn response_server(
        status: &str,
        headers: &str,
        body: Vec<u8>,
        known_length: bool,
    ) -> (GifSnap, mpsc::Receiver<String>) {
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
                "HTTP/1.1 {status}\r\n{}Connection: close\r\n{headers}\r\n",
                if known_length {
                    format!("Content-Length: {}\r\n", body.len())
                } else {
                    String::new()
                }
            )
            .unwrap();
            let _ = stream.write_all(&body);
        });
        (GifSnap::with_base(base).unwrap(), receive)
    }

    fn page() -> Vec<u8> {
        br#"{"data":[{"id":"one","title":"hello","url":"https://media.gifsnap.com/a.webp?token=keep%2Bme","preview_url":"https://media.gifsnap.com/static.webp","source":"tenor","content_id":"verified:one","animated_preview":{"url":"https://media.gifsnap.com/tiny.gif","mime_type":"image/gif","width":160,"height":120,"animated":true}}],"pagination":{"page":1,"next_page":2,"has_next":true}}"#.to_vec()
    }

    #[test]
    fn encodes_search_and_pages_and_preserves_media_metadata() {
        let (api, request) = server("200 OK", "", page());
        let page = api.search(" hello & ü ", None).unwrap();
        assert_eq!(page.items[0].id, "one");
        assert_eq!(page.items[0].page_url.path(), "/api/v1/gifs/one");
        assert_eq!(page.cursor.as_deref(), Some("2"));
        assert_eq!(
            page.items[0].download_url.as_str(),
            "https://media.gifsnap.com/a.webp?token=keep%2Bme"
        );
        assert!(
            page.items[0]
                .preview_url
                .as_ref()
                .unwrap()
                .as_str()
                .ends_with("tiny.gif")
        );
        assert_eq!(page.items[0].source.as_deref(), Some("tenor"));
        assert_eq!(page.items[0].content_id.as_deref(), Some("verified:one"));
        let request = request.recv().unwrap();
        let url = Url::parse(&format!(
            "http://test{}",
            request.split_whitespace().nth(1).unwrap()
        ))
        .unwrap();
        let params: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(url.path(), "/api/v1/gifs/search");
        assert_eq!(params["q"], "hello & ü");
        assert_eq!(params["limit"], "24");
        assert_eq!(params["page"], "1");
    }

    #[test]
    fn accepts_upstream_klipy_media_in_gifsnap_search_results() {
        // gifsnap's aura text results point directly at this upstream cdn.
        let url = "https://static.klipy.com/ii/4493325008d34b7bf8cd6813cd5c1619/2e/4e/ytAVDGdO1XP5rv2IU8.webp?token=keep%2Bme";
        let mut body: serde_json::Value = serde_json::from_slice(&page()).unwrap();
        body["data"][0]["url"] = url.into();
        body["data"][0]["animated_preview"]["url"] = url.into();
        body["data"][0]["animated_preview"]["mime_type"] = "image/webp".into();
        let (api, _) = server("200 OK", "", serde_json::to_vec(&body).unwrap());
        let item = api.search("aura text", None).unwrap().items.remove(0);
        assert_eq!(item.download_url.as_str(), url);
        assert_eq!(item.preview_url.unwrap().as_str(), url);
        assert_eq!(item.source.as_deref(), Some("tenor"));
    }

    #[test]
    fn trending_and_last_page_are_distinct_from_search_failures() {
        let (api, request) = server(
            "200 OK",
            "",
            br#"{"data":[],"pagination":{"page":2,"has_next":false,"next_page":null}}"#.to_vec(),
        );
        let page = api.search("", Some("2")).unwrap();
        assert!(page.items.is_empty() && page.cursor.is_none());
        let request = request.recv().unwrap();
        assert!(request.contains("/api/v1/gifs/trending?") && request.contains("page=2"));
    }

    #[test]
    fn skips_static_and_unsupported_previews_without_dropping_selected_media() {
        for preview in [
            serde_json::Value::Null,
            serde_json::json!({"url":"https://media.gifsnap.com/a.mp4","mime_type":"video/mp4","width":160,"height":120,"animated":true}),
            serde_json::json!({"url":"https://evil.test/a.gif","mime_type":"image/gif","width":160,"height":120,"animated":true}),
            serde_json::json!({"url":"https://media.gifsnap.com/a.gif","mime_type":"image/gif","width":160,"height":120,"animated":false}),
        ] {
            let mut body: serde_json::Value = serde_json::from_slice(&page()).unwrap();
            body["data"][0]["animated_preview"] = preview;
            let (api, _) = server("200 OK", "", serde_json::to_vec(&body).unwrap());
            let item = api.search("hello", None).unwrap().items.remove(0);
            assert!(item.preview_url.is_none());
            assert_eq!(
                item.download_url.as_str(),
                "https://media.gifsnap.com/a.webp?token=keep%2Bme"
            );
        }
    }

    #[test]
    fn rejects_invalid_pagination_and_untrusted_media() {
        for pagination in [
            serde_json::json!({"page":1,"has_next":true}),
            serde_json::json!({"page":1,"has_next":true,"next_page":1}),
            serde_json::json!({"page":2,"has_next":false}),
        ] {
            let mut body: serde_json::Value = serde_json::from_slice(&page()).unwrap();
            body["pagination"] = pagination;
            let (api, _) = server("200 OK", "", serde_json::to_vec(&body).unwrap());
            assert!(api.search("hello", None).is_err());
        }
        for cursor in ["0", "-1", "1000001", "not a page"] {
            let api = GifSnap::new().unwrap();
            assert!(api.search("hello", Some(cursor)).is_err());
        }
        for url in [
            "http://gifsnap.com/a.gif",
            "https://evil.test/a.gif",
            "https://gifsnap.com.evil.test/a.gif",
            "https://static.klipy.com.evil.test/a.webp",
            "https://evil.static.klipy.com/a.webp",
            "https://klipy.com/a.webp",
            "http://static.klipy.com/a.webp",
            "https://static.klipy.com:444/a.webp",
            "https://user@static.klipy.com/a.webp",
            "https://static.klipy.com/a.webp#fragment",
            "https://gifsnap.com:444/a.gif",
            "https://user@gifsnap.com/a.gif",
            "https://127.0.0.1/a.gif",
        ] {
            let mut body: serde_json::Value = serde_json::from_slice(&page()).unwrap();
            body["data"][0]["url"] = url.into();
            let (api, _) = server("200 OK", "", serde_json::to_vec(&body).unwrap());
            assert!(api.search("hello", None).is_err());
        }
    }

    #[test]
    fn reports_rate_limit_http_and_malformed_errors_without_retrying() {
        let (api, _) = server("429 Too Many Requests", "Retry-After: 17\r\n", Vec::new());
        assert!(
            api.search("hello", None)
                .unwrap_err()
                .to_string()
                .contains("17 seconds")
        );
        let (api, _) = server("429 Too Many Requests", "", Vec::new());
        assert!(
            api.search("hello", None)
                .unwrap_err()
                .to_string()
                .contains("try again later")
        );
        let (api, _) = server("503 Service Unavailable", "", Vec::new());
        assert!(
            api.search("hello", None)
                .unwrap_err()
                .to_string()
                .contains("503")
        );
        let (api, _) = server("302 Found", "Location: https://evil.test/\r\n", Vec::new());
        assert!(
            api.search("hello", None)
                .unwrap_err()
                .to_string()
                .contains("302")
        );
        for bytes in [b"not json".to_vec(), br#"{"error":"failure"}"#.to_vec()] {
            let (api, _) = server("200 OK", "", bytes);
            assert!(api.search("hello", None).is_err());
        }
    }

    #[test]
    fn bounds_response_and_media_bytes_even_without_content_length() {
        for known in [true, false] {
            let (api, _) =
                response_server("200 OK", "", vec![0; API_LIMIT_BYTES as usize + 1], known);
            assert!(
                api.search("hello", None)
                    .unwrap_err()
                    .to_string()
                    .contains("size limit")
            );
            let (api, _) = response_server("200 OK", "", vec![0; 101], known);
            assert!(api.media(&api.base.join("asset").unwrap(), 100).is_err());
        }
        for bytes in [b"GIF89aabc".to_vec(), crate::test_media::animation(2, 1, 2)] {
            let (api, _) = server("200 OK", "", bytes.clone());
            assert_eq!(
                api.media(&api.base.join("asset").unwrap(), 10000).unwrap(),
                bytes
            );
        }
        let (api, _) = server("200 OK", "", b"<html>".to_vec());
        assert!(api.media(&api.base.join("asset").unwrap(), 100).is_err());
    }

    #[test]
    fn successful_small_animation_does_not_download_full_asset() {
        let bytes = crate::test_media::animation(2, 1, 2);
        let (api, request) = server("200 OK", "", bytes);
        let gif = Gif {
            id: "one".into(),
            title: "hello".into(),
            page_url: api.base.clone(),
            preview_url: Some(api.base.join("small").unwrap()),
            download_url: api.base.join("full").unwrap(),
            source: None,
            content_id: None,
        };
        let animation = crate::app::load_preview(&api, &gif).unwrap();
        assert_eq!(animation.frames.len(), 2);
        assert_eq!(animation.frames[0].rgba, [255, 0, 0, 255].repeat(2));
        // this one-response server closes its listener: an unwanted /full fetch cannot succeed.
        assert!(request.recv().unwrap().starts_with("GET /small "));
        assert!(request.try_recv().is_err());
    }

    #[test]
    fn failed_or_corrupt_small_animation_falls_back_to_full_asset() {
        for small in [b"GIF89a;".to_vec(), b"<html>".to_vec()] {
            let listener = TcpListener::bind("localhost:0").unwrap();
            let base = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
            let original = crate::test_media::animation(2, 1, 2);
            let original_copy = original.clone();
            let worker = std::thread::spawn(move || {
                for (path, bytes) in [("/small", small), ("/full", original_copy)] {
                    let (mut stream, _) = listener.accept().unwrap();
                    let mut reader = BufReader::new(&stream);
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    assert!(line.starts_with(&format!("GET {path} ")));
                    loop {
                        line.clear();
                        reader.read_line(&mut line).unwrap();
                        if line == "\r\n" {
                            break;
                        }
                    }
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        bytes.len()
                    )
                    .unwrap();
                    stream.write_all(&bytes).unwrap();
                }
            });
            let api = GifSnap::with_base(base.clone()).unwrap();
            let gif = Gif {
                id: "one".into(),
                title: "hello".into(),
                page_url: base.clone(),
                preview_url: Some(base.join("small").unwrap()),
                download_url: base.join("full").unwrap(),
                source: None,
                content_id: None,
            };
            let animation = crate::app::load_preview(&api, &gif).unwrap();
            assert_eq!(animation.frames.len(), 2);
            assert_eq!(animation.frames[1].rgba, [0, 255, 0, 128].repeat(2));
            worker.join().unwrap();
        }
    }
}
