//! eBay Browse API client — OAuth2 client-credentials (app-token) flow, in
//! the house `SpotifyClient` style (see `capabilities/music/src/web_api.rs`)
//! but simpler: no user consent step, no rotating refresh token.

use serde::Deserialize;
use serde_json::Value;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

const TOKEN_URL: &str = "https://api.ebay.com/identity/v1/oauth2/token";
const API_BASE: &str = "https://api.ebay.com/buy/browse/v1";
const OAUTH_SCOPE: &str = "https://api.ebay.com/oauth/api_scope";

/// Errors from the Browse API, pre-phrased for humans.
#[derive(Debug)]
pub enum EbayError {
    NotConfigured,
    Unauthorized,
    RateLimited,
    Other(String),
}

impl std::fmt::Display for EbayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EbayError::NotConfigured => write!(f, "eBay client_id/client_secret not configured"),
            EbayError::Unauthorized => write!(f, "eBay sign-in failed (check client_id/secret)"),
            EbayError::RateLimited => write!(f, "eBay API rate limited"),
            EbayError::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for EbayError {}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Listing {
    pub item_id: String,
    pub title: String,
    pub price_minor: Option<i64>,
    pub currency: Option<String>,
    pub image_url: Option<String>,
    pub item_web_url: String,
    /// e.g. "New", "Used", "For parts or not working" — carried through to
    /// the bargain-verdict prompt so a suspiciously-cheap "for parts"
    /// listing doesn't get flagged as a steal.
    pub condition: Option<String>,
}

/// The pasted-URL lookup result, used to seed the term-generation prompt.
#[derive(Debug, Clone)]
pub struct ItemDetail {
    pub item_id: String,
    pub title: String,
    pub category: Option<String>,
    pub price_minor: Option<i64>,
    pub currency: Option<String>,
    pub condition: Option<String>,
}

struct CachedToken {
    access_token: String,
    expires_at: Instant,
}

pub struct EbayClient {
    http: reqwest::Client,
    client_id: String,
    client_secret: String,
    token: Mutex<Option<CachedToken>>,
}

impl EbayClient {
    pub fn new(client_id: String, client_secret: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            client_id,
            client_secret,
            token: Mutex::new(None),
        }
    }

    async fn access_token(&self) -> Result<String, EbayError> {
        if self.client_id.is_empty() || self.client_secret.is_empty() {
            return Err(EbayError::NotConfigured);
        }
        let mut guard = self.token.lock().await;
        if let Some(t) = guard.as_ref()
            && t.expires_at > Instant::now()
        {
            return Ok(t.access_token.clone());
        }

        let resp = self
            .http
            .post(TOKEN_URL)
            .basic_auth(&self.client_id, Some(&self.client_secret))
            .form(&[("grant_type", "client_credentials"), ("scope", OAUTH_SCOPE)])
            .send()
            .await
            .map_err(|e| EbayError::Other(format!("could not reach eBay: {e}")))?;

        let status = resp.status();
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            return Err(if status.as_u16() == 401 || status.as_u16() == 403 {
                EbayError::Unauthorized
            } else {
                EbayError::Other(format!(
                    "eBay token request failed: HTTP {} {}",
                    status,
                    body["error_description"].as_str().unwrap_or("")
                ))
            });
        }
        let Some(access_token) = body["access_token"].as_str() else {
            return Err(EbayError::Other("eBay token response had no token".into()));
        };
        let expires_in = body["expires_in"].as_u64().unwrap_or(7200);
        *guard = Some(CachedToken {
            access_token: access_token.to_string(),
            expires_at: Instant::now() + Duration::from_secs(expires_in.saturating_sub(60)),
        });
        Ok(access_token.to_string())
    }

    /// `GET item_summary/search` for `terms` (joined with " OR " so any one
    /// matching term surfaces a listing), scoped to `marketplace` (e.g.
    /// "EBAY_GB").
    pub async fn search(
        &self,
        terms: &[String],
        marketplace: &str,
    ) -> Result<Vec<Listing>, EbayError> {
        let token = self.access_token().await?;
        let query = terms.join(" OR ");
        let resp = self
            .http
            .get(format!("{API_BASE}/item_summary/search"))
            .bearer_auth(token)
            .header("X-EBAY-C-MARKETPLACE-ID", marketplace)
            .query(&[("q", query.as_str()), ("limit", "50")])
            .send()
            .await
            .map_err(|e| EbayError::Other(format!("could not reach eBay: {e}")))?;

        let status = resp.status();
        if status.as_u16() == 429 {
            return Err(EbayError::RateLimited);
        }
        if !status.is_success() {
            return Err(EbayError::Other(format!(
                "eBay search failed: HTTP {status}"
            )));
        }
        let body: SearchResponse = resp
            .json()
            .await
            .map_err(|e| EbayError::Other(format!("unexpected eBay search response: {e}")))?;
        Ok(body
            .item_summaries
            .into_iter()
            .map(|i| Listing {
                item_id: i.item_id,
                title: i.title,
                price_minor: i.price.as_ref().and_then(price_to_minor),
                currency: i.price.map(|p| p.currency),
                image_url: i.image.map(|img| img.image_url),
                item_web_url: i.item_web_url,
                condition: i.condition,
            })
            .collect())
    }

    /// Look up full detail for a legacy item id parsed out of a pasted eBay
    /// URL (see [`parse_legacy_item_id`]).
    pub async fn lookup_item(&self, url: &str) -> Result<ItemDetail, EbayError> {
        let legacy_id = parse_legacy_item_id(url)
            .ok_or_else(|| EbayError::Other("could not find an eBay item id in that URL".into()))?;
        let token = self.access_token().await?;
        let resp = self
            .http
            .get(format!("{API_BASE}/item/get_item_by_legacy_id"))
            .bearer_auth(token)
            .header("X-EBAY-C-MARKETPLACE-ID", "EBAY_GB")
            .query(&[("legacy_item_id", legacy_id.as_str())])
            .send()
            .await
            .map_err(|e| EbayError::Other(format!("could not reach eBay: {e}")))?;

        let status = resp.status();
        if status.as_u16() == 429 {
            return Err(EbayError::RateLimited);
        }
        if !status.is_success() {
            return Err(EbayError::Other(format!(
                "eBay item lookup failed: HTTP {status}"
            )));
        }
        let body: ItemResponse = resp
            .json()
            .await
            .map_err(|e| EbayError::Other(format!("unexpected eBay item response: {e}")))?;
        Ok(ItemDetail {
            item_id: legacy_id,
            title: body.title,
            category: body.category_path,
            price_minor: body.price.as_ref().and_then(price_to_minor),
            currency: body.price.map(|p| p.currency),
            condition: body.condition,
        })
    }
}

fn price_to_minor(p: &ApiPrice) -> Option<i64> {
    p.value
        .parse::<f64>()
        .ok()
        .map(|v| (v * 100.0).round() as i64)
}

#[derive(Deserialize)]
struct SearchResponse {
    #[serde(default, rename = "itemSummaries")]
    item_summaries: Vec<ItemSummary>,
}

#[derive(Deserialize)]
struct ItemSummary {
    #[serde(rename = "itemId")]
    item_id: String,
    title: String,
    #[serde(default)]
    price: Option<ApiPrice>,
    #[serde(default)]
    image: Option<ApiImage>,
    #[serde(rename = "itemWebUrl")]
    item_web_url: String,
    #[serde(default)]
    condition: Option<String>,
}

#[derive(Deserialize)]
struct ApiPrice {
    value: String,
    currency: String,
}

#[derive(Deserialize)]
struct ApiImage {
    #[serde(rename = "imageUrl")]
    image_url: String,
}

#[derive(Deserialize)]
struct ItemResponse {
    title: String,
    #[serde(default)]
    price: Option<ApiPrice>,
    #[serde(default)]
    condition: Option<String>,
    #[serde(default, rename = "categoryPath")]
    category_path: Option<String>,
}

/// Parse an eBay legacy item id out of a pasted listing URL. Handles:
/// - `.../itm/<title-slug>/<id>`
/// - `.../itm/<id>`
/// - `...?hash=item<id>:g:<rest>` (eBay's other common share-link form)
///
/// Returns `None` (never falls back to scraping) if nothing recognisable is
/// found, so callers can surface a clean 400 instead of guessing.
pub fn parse_legacy_item_id(url: &str) -> Option<String> {
    if let Some(idx) = url.find("/itm/") {
        let after = &url[idx + "/itm/".len()..];
        let after = after.split(['?', '#']).next().unwrap_or(after);
        let segments: Vec<&str> = after.split('/').filter(|s| !s.is_empty()).collect();
        if let Some(&last) = segments.last()
            && last.chars().all(|c| c.is_ascii_digit())
            && !last.is_empty()
        {
            return Some(last.to_string());
        }
    }
    if let Some(idx) = url.find("item") {
        let after = &url[idx + "item".len()..];
        let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() {
            return Some(digits);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_slug_and_id_path() {
        assert_eq!(
            parse_legacy_item_id("https://www.ebay.co.uk/itm/Fender-Strat/123456789012"),
            Some("123456789012".to_string())
        );
    }

    #[test]
    fn parses_bare_id_path() {
        assert_eq!(
            parse_legacy_item_id("https://www.ebay.co.uk/itm/123456789012"),
            Some("123456789012".to_string())
        );
    }

    #[test]
    fn parses_query_string_hash_form() {
        assert_eq!(
            parse_legacy_item_id(
                "https://www.ebay.co.uk/sch/i.html?_nkw=strat&hash=item123456789012:g:abcAAOSw"
            ),
            Some("123456789012".to_string())
        );
    }

    #[test]
    fn returns_none_for_malformed_url() {
        assert_eq!(
            parse_legacy_item_id("https://www.ebay.co.uk/sch/i.html?_nkw=strat"),
            None
        );
    }

    #[test]
    fn returns_none_for_non_numeric_slug_tail() {
        assert_eq!(
            parse_legacy_item_id("https://www.ebay.co.uk/itm/Fender-Strat/"),
            None
        );
    }
}
