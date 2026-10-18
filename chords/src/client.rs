//! Fetching Ultimate Guitar pages. One request per thing the user asked for —
//! no crawling, no background polling.

use crate::ug::{self, SearchHit, Tab, UgError};
use std::time::Duration;

const SITE: &str = "https://www.ultimate-guitar.com";
/// UG answers reqwest's default agent with a bot check, as Wikimedia does
/// (see `capabilities/art`), so ask the way a browser does.
const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) \
                          Chrome/130.0 Safari/537.36";
/// Search pages fetched per query. Popular songs have dozens of versions, and
/// page two is where the second-best chords version often is.
const SEARCH_PAGES: u32 = 2;

pub struct UgClient {
    http: reqwest::Client,
    /// Where search pages come from. Only a test points it elsewhere.
    base: String,
}

impl Default for UgClient {
    fn default() -> Self {
        Self::new()
    }
}

impl UgClient {
    pub fn new() -> Self {
        Self::with_base_url(SITE)
    }

    /// A client whose search goes to `base` and which will also fetch tab
    /// pages under `base` — for tests against a local server.
    pub fn with_base_url(base: &str) -> Self {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::ACCEPT_LANGUAGE,
            reqwest::header::HeaderValue::from_static("en-GB,en;q=0.9"),
        );
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .user_agent(USER_AGENT)
            .default_headers(headers)
            .build()
            .unwrap_or_default();
        Self {
            http,
            base: base.trim_end_matches('/').to_string(),
        }
    }

    /// Every free sheet version UG has for `query` (a title, an artist, or both).
    pub async fn search(&self, query: &str) -> Result<Vec<SearchHit>, UgError> {
        let mut hits = Vec::new();
        for page in 1..=SEARCH_PAGES {
            let url = format!("{}/search.php", self.base);
            let page_str = page.to_string();
            let req = self.http.get(&url).query(&[
                ("search_type", "title"),
                ("value", query),
                ("page", page_str.as_str()),
            ]);
            match self.get_store(req).await {
                Ok(store) => {
                    let found = ug::parse_search(&store);
                    let last = store["store"]["page"]["data"]["pagination"]["total"]
                        .as_i64()
                        .is_some_and(|total| total <= page as i64);
                    let empty = found.is_empty();
                    hits.extend(found);
                    if empty || last {
                        break;
                    }
                }
                // UG answers a page past the end with a 404; a later page
                // failing for any reason shouldn't cost the first page's results.
                Err(_) if page > 1 => break,
                Err(UgError::NotFound) => break,
                Err(e) => return Err(e),
            }
        }
        Ok(hits)
    }

    /// One version's sheet. Refuses anything but a UG tab page.
    pub async fn fetch_tab(&self, url: &str) -> Result<Tab, UgError> {
        let url = url.trim();
        let local = self.base != SITE && url.starts_with(&format!("{}/", self.base));
        if !ug::is_ug_url(url) && !local {
            return Err(UgError::Other(
                "only Ultimate Guitar tab links (https://tabs.ultimate-guitar.com/tab/…) can be fetched"
                    .into(),
            ));
        }
        let store = self.get_store(self.http.get(url)).await?;
        let mut tab = ug::parse_tab(&store)?;
        if tab.url.is_none() {
            tab.url = Some(url.to_string());
        }
        Ok(tab)
    }

    async fn get_store(&self, req: reqwest::RequestBuilder) -> Result<serde_json::Value, UgError> {
        let resp = req
            .send()
            .await
            .map_err(|e| UgError::Unreachable(e.to_string()))?;
        match resp.status().as_u16() {
            200..=299 => {}
            404 | 410 => return Err(UgError::NotFound),
            403 | 429 | 503 => return Err(UgError::Blocked),
            s => return Err(UgError::Other(format!("Ultimate Guitar returned HTTP {s}"))),
        }
        let html = resp
            .text()
            .await
            .map_err(|e| UgError::Unreachable(e.to_string()))?;
        ug::extract_store(&html)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A one-request HTTP server answering with `status` and `body`; returns
    /// its base URL.
    async fn serve(status: &'static str, body: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = vec![0u8; 8192];
                let _ = sock.read(&mut buf).await;
                let resp = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            }
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn fetches_a_tab_from_a_page() {
        let base = serve("200 OK", include_str!("../tests/fixtures/tab.html")).await;
        let client = UgClient::with_base_url(&base);
        let tab = client.fetch_tab(&format!("{base}/tab/x")).await.unwrap();
        assert_eq!(tab.song_name, "Pompeii");
        assert_eq!(tab.versions.len(), 2);
    }

    #[tokio::test]
    async fn search_reads_results_and_stops_at_the_last_page() {
        let base = serve("200 OK", include_str!("../tests/fixtures/search.html")).await;
        let hits = UgClient::with_base_url(&base)
            .search("pompeii")
            .await
            .unwrap();
        // The fixture says it is the only page, so it is read once, not twice.
        assert_eq!(hits.len(), 5);
    }

    #[tokio::test]
    async fn a_bot_check_is_reported_as_blocked() {
        let base = serve("200 OK", include_str!("../tests/fixtures/blocked.html")).await;
        let err = UgClient::with_base_url(&base)
            .search("x")
            .await
            .unwrap_err();
        assert_eq!(err, UgError::Blocked);

        let base = serve("403 Forbidden", "").await;
        let err = UgClient::with_base_url(&base)
            .fetch_tab(&format!("{base}/tab/x"))
            .await
            .unwrap_err();
        assert_eq!(err, UgError::Blocked);
    }

    #[tokio::test]
    async fn the_real_client_refuses_other_hosts() {
        let err = UgClient::new()
            .fetch_tab("http://127.0.0.1:1/tab/x")
            .await
            .unwrap_err();
        assert!(matches!(err, UgError::Other(_)));
    }
}
