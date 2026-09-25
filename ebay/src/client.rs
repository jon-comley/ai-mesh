//! eBay Browse API client — OAuth2 client-credentials (app-token) flow, in
//! the house `SpotifyClient` style (see `capabilities/music/src/web_api.rs`)
//! but simpler: no user consent step, no rotating refresh token.

use crate::HuntFilter;
use serde::Deserialize;
use serde_json::Value;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

const TOKEN_URL: &str = "https://api.ebay.com/identity/v1/oauth2/token";
const API_BASE: &str = "https://api.ebay.com/buy/browse/v1";
const TAXONOMY_BASE: &str = "https://api.ebay.com/commerce/taxonomy/v1";
const OAUTH_SCOPE: &str = "https://api.ebay.com/oauth/api_scope";
/// Listings fetched per search cycle. See `Client::search` for why this is the
/// ceiling on what a hunt can notice at all. eBay's Browse API allows up to 200.
const SEARCH_LIMIT: &str = "100";

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
    /// The marketplace's own id for that category, so a hunt made from this
    /// listing can search inside it instead of everywhere.
    pub category_id: Option<String>,
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
    ///
    /// **`SEARCH_LIMIT` is how many listings a cycle can see at all.** Anything
    /// past it is not "shown later", it is never fetched, so a hunt whose terms
    /// match more than this silently only ever considers the first page eBay
    /// chooses to return. Raised 50 -> 100 on 2026-09-14 at Jon's request; the
    /// Browse API caps it at 200, so there is headroom if a hunt outgrows this
    /// too. The cost is one request either way — the limit does not change the
    /// number of API calls, only the size of the one response, and dedupe
    /// against `ebay_seen_listings` means a bigger page does not mean more LLM
    /// verdicts on repeat cycles.
    ///
    /// Not to be confused with `default_finds_limit` in `http::api::ebay`,
    /// which caps how many already-stored finds the dashboard asks for.
    pub async fn search(
        &self,
        terms: &[String],
        marketplace: &str,
        filter: &HuntFilter,
    ) -> Result<Vec<Listing>, EbayError> {
        let token = self.access_token().await?;
        let query = terms.join(" OR ");
        let params = search_params(&query, marketplace, filter);
        let resp = self
            .http
            .get(format!("{API_BASE}/item_summary/search"))
            .bearer_auth(token)
            .header("X-EBAY-C-MARKETPLACE-ID", marketplace)
            .query(&params)
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

    /// The marketplace category a set of search terms agree on, from the Taxonomy
    /// API's suggestions for each (see `vote_category`). `Ok(None)` when they do
    /// not agree, eBay has nothing to say, or the marketplace is one this does not
    /// know. An error only when no term could be looked up at all.
    pub async fn suggest_category_for_terms(
        &self,
        terms: &[String],
        marketplace: &str,
    ) -> Result<Option<(String, String)>, EbayError> {
        let Some(tree) = category_tree_id(marketplace) else {
            return Ok(None);
        };
        let token = self.access_token().await?;

        let mut chains: Vec<CategoryChain> = Vec::new();
        let mut last_error = None;
        let mut asked: Vec<String> = Vec::new();

        for term in terms {
            let term = term.trim();
            if term.is_empty() || asked.iter().any(|t| t.eq_ignore_ascii_case(term)) {
                continue;
            }
            if asked.len() == CATEGORY_VOTE_TERMS {
                break;
            }
            asked.push(term.to_string());

            match self.category_chain(&token, tree, term).await {
                Ok(Some(chain)) => chains.push(chain),
                Ok(None) => {}
                Err(EbayError::RateLimited) => return Err(EbayError::RateLimited),
                // One term that cannot be looked up just does not vote.
                Err(e) => last_error = Some(e),
            }
        }

        match (chains.is_empty(), last_error) {
            (true, Some(e)) => Err(e),
            _ => Ok(vote_category(&chains)),
        }
    }

    async fn category_chain(
        &self,
        token: &str,
        tree: &str,
        query: &str,
    ) -> Result<Option<CategoryChain>, EbayError> {
        let resp = self
            .http
            .get(format!(
                "{TAXONOMY_BASE}/category_tree/{tree}/get_category_suggestions"
            ))
            .bearer_auth(token)
            .query(&[("q", query)])
            .send()
            .await
            .map_err(|e| EbayError::Other(format!("could not reach eBay: {e}")))?;

        let status = resp.status();
        if status.as_u16() == 429 {
            return Err(EbayError::RateLimited);
        }
        if !status.is_success() {
            return Err(EbayError::Other(format!(
                "eBay category lookup failed: HTTP {status}"
            )));
        }
        let body: Value = resp
            .json()
            .await
            .map_err(|e| EbayError::Other(format!("unexpected eBay category response: {e}")))?;
        Ok(parse_category_chain(&body))
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
            category_id: body.category_id,
            price_minor: body.price.as_ref().and_then(price_to_minor),
            currency: body.price.map(|p| p.currency),
            condition: body.condition,
        })
    }
}

/// The currency a marketplace prices in, for the price filter. `None` for one
/// this does not know, in which case no price ceiling is sent rather than a
/// guessed one.
pub fn marketplace_currency(marketplace: &str) -> Option<&'static str> {
    match marketplace {
        "EBAY_GB" => Some("GBP"),
        "EBAY_US" => Some("USD"),
        "EBAY_DE" | "EBAY_FR" | "EBAY_IT" | "EBAY_ES" | "EBAY_IE" => Some("EUR"),
        "EBAY_AU" => Some("AUD"),
        "EBAY_CA" => Some("CAD"),
        _ => None,
    }
}

/// The query string for `item_summary/search`. Pure, so it can be tested without eBay.
///
/// `category_ids` confines the search to one category. The price ceiling goes in
/// the `filter` parameter as `price:[..MAX]` and needs its currency alongside.
fn search_params(query: &str, marketplace: &str, filter: &HuntFilter) -> Vec<(&'static str, String)> {
    let mut params = vec![("q", query.to_string()), ("limit", SEARCH_LIMIT.to_string())];

    if let Some(category) = filter.category_id.as_deref().filter(|c| !c.is_empty()) {
        params.push(("category_ids", category.to_string()));
    }

    if let (Some(max), Some(currency)) = (filter.max_price_minor, marketplace_currency(marketplace))
        && max > 0
    {
        params.push((
            "filter",
            format!("price:[..{}.{:02}],priceCurrency:{currency}", max / 100, max % 100),
        ));
    }

    params
}

/// The taxonomy tree a marketplace's categories live in.
fn category_tree_id(marketplace: &str) -> Option<&'static str> {
    match marketplace {
        "EBAY_GB" => Some("3"),
        "EBAY_US" => Some("0"),
        "EBAY_DE" => Some("77"),
        "EBAY_AU" => Some("15"),
        "EBAY_CA" => Some("2"),
        "EBAY_FR" => Some("71"),
        "EBAY_IT" => Some("101"),
        "EBAY_ES" => Some("186"),
        "EBAY_IE" => Some("205"),
        _ => None,
    }
}

/// A category and every one above it, root first, each as (id, name).
type CategoryChain = Vec<(String, String)>;

/// How many of a hunt's terms are looked up to agree on a category. Each is a
/// request, so this is a cost cap as much as a quality one.
const CATEGORY_VOTE_TERMS: usize = 5;

/// The top suggestion in a `get_category_suggestions` reply, as the chain from
/// the root down to it.
///
/// Only the first suggestion is read. The API is built for item titles and its
/// later suggestions are often unrelated ("cars" also offers toy cars), so what
/// they add is noise; agreement between terms is worked out in `vote_category`.
fn parse_category_chain(body: &Value) -> Option<CategoryChain> {
    let first = body["categorySuggestions"].get(0)?;
    let leaf = category_node(first.get("category")?)?;

    let mut ancestors: Vec<(Option<u64>, (String, String))> = first["categoryTreeNodeAncestors"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|a| Some((a["categoryTreeNodeLevel"].as_u64(), category_node(a)?)))
                .collect()
        })
        .unwrap_or_default();

    // By level when every ancestor has one; otherwise the API lists them nearest
    // parent first, so the reverse is root first.
    if ancestors.iter().all(|(level, _)| level.is_some()) {
        ancestors.sort_by_key(|(level, _)| *level);
    } else {
        ancestors.reverse();
    }

    let mut chain: CategoryChain = ancestors.into_iter().map(|(_, node)| node).collect();
    chain.push(leaf);
    Some(chain)
}

fn category_node(v: &Value) -> Option<(String, String)> {
    let id = v["categoryId"].as_str()?.trim();
    let name = v["categoryName"].as_str()?.trim();

    (!id.is_empty() && !name.is_empty()).then(|| (id.to_string(), name.to_string()))
}

/// The deepest category that MORE THAN HALF of the chains pass through.
///
/// A hunt for "ford fiesta", "vauxhall corsa" and "toyota yaris" lands in three
/// different make categories, which share only their parent, "Cars": that is the
/// answer, and it is wider than any one term's own category on purpose. One term
/// that lands somewhere silly (a fuel filter) is outvoted rather than trusted.
/// `None` when nothing has a majority, so a hunt is left unfiltered rather than
/// confined to a guess.
fn vote_category(chains: &[CategoryChain]) -> Option<(String, String)> {
    use std::collections::{HashMap, HashSet};

    // id -> (chains through it, deepest position it was seen at, name)
    let mut tally: HashMap<&str, (usize, usize, &str)> = HashMap::new();

    for chain in chains {
        let mut seen: HashSet<&str> = HashSet::new();
        for (depth, (id, name)) in chain.iter().enumerate() {
            if seen.insert(id.as_str()) {
                let entry = tally.entry(id.as_str()).or_insert((0, 0, name.as_str()));
                entry.0 += 1;
                entry.1 = entry.1.max(depth);
            }
        }
    }

    tally
        .into_iter()
        .filter(|(_, (count, _, _))| count * 2 > chains.len())
        .max_by(|a, b| (a.1.1, a.1.0, a.0).cmp(&(b.1.1, b.1.0, b.0)))
        .map(|(id, (_, _, name))| (id.to_string(), name.to_string()))
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
    #[serde(default, rename = "categoryId")]
    category_id: Option<String>,
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

    fn filter(category: Option<&str>, max: Option<i64>) -> HuntFilter {
        HuntFilter {
            category_id: category.map(str::to_string),
            category_name: None,
            max_price_minor: max,
        }
    }

    fn param<'a>(params: &'a [(&'static str, String)], key: &str) -> Option<&'a str> {
        params.iter().find(|(k, _)| *k == key).map(|(_, v)| v.as_str())
    }

    #[test]
    fn search_with_no_filter_sends_only_the_query_and_limit() {
        let params = search_params("fiesta", "EBAY_GB", &HuntFilter::default());

        assert_eq!(param(&params, "q"), Some("fiesta"));
        assert_eq!(param(&params, "limit"), Some(SEARCH_LIMIT));
        assert_eq!(param(&params, "category_ids"), None);
        assert_eq!(param(&params, "filter"), None);
    }

    #[test]
    fn search_confines_to_the_category_when_there_is_one() {
        let params = search_params("fiesta", "EBAY_GB", &filter(Some("9801"), None));

        assert_eq!(param(&params, "category_ids"), Some("9801"));
    }

    #[test]
    fn an_empty_category_is_not_sent() {
        let params = search_params("fiesta", "EBAY_GB", &filter(Some(""), None));

        assert_eq!(param(&params, "category_ids"), None);
    }

    #[test]
    fn the_price_ceiling_is_sent_with_its_currency() {
        let params = search_params("fiesta", "EBAY_GB", &filter(None, Some(150_000)));

        assert_eq!(param(&params, "filter"), Some("price:[..1500.00],priceCurrency:GBP"));
    }

    #[test]
    fn a_price_ceiling_keeps_its_pence() {
        let params = search_params("fiesta", "EBAY_GB", &filter(None, Some(129_905)));

        assert_eq!(param(&params, "filter"), Some("price:[..1299.05],priceCurrency:GBP"));
    }

    #[test]
    fn no_ceiling_is_sent_for_a_marketplace_with_no_known_currency() {
        let params = search_params("fiesta", "EBAY_MARS", &filter(Some("9801"), Some(150_000)));

        assert_eq!(param(&params, "filter"), None);
        assert_eq!(param(&params, "category_ids"), Some("9801"));
    }

    #[test]
    fn a_zero_ceiling_means_none() {
        let params = search_params("fiesta", "EBAY_GB", &filter(None, Some(0)));

        assert_eq!(param(&params, "filter"), None);
    }

    fn node(id: &str, name: &str) -> (String, String) {
        (id.to_string(), name.to_string())
    }

    /// Root first, as `parse_category_chain` hands them over.
    fn chain(nodes: &[(&str, &str)]) -> CategoryChain {
        nodes.iter().map(|(i, n)| node(i, n)).collect()
    }

    fn cars(make_id: &str, make: &str) -> CategoryChain {
        chain(&[("9800", "Cars, Motorcycles & Vehicles"), ("9801", "Cars"), (make_id, make)])
    }

    #[test]
    fn a_suggestion_becomes_a_chain_from_the_root_down() {
        let body: Value = serde_json::from_str(
            r#"{"categorySuggestions":[{
                "category":{"categoryId":"9844","categoryName":"Ford"},
                "categoryTreeNodeAncestors":[
                    {"categoryId":"9801","categoryName":"Cars","categoryTreeNodeLevel":2},
                    {"categoryId":"9800","categoryName":"Cars, Motorcycles & Vehicles","categoryTreeNodeLevel":1}]},
              {"category":{"categoryId":"6030","categoryName":"Car Parts"}}]}"#,
        )
        .unwrap();

        assert_eq!(
            parse_category_chain(&body),
            Some(chain(&[("9800", "Cars, Motorcycles & Vehicles"), ("9801", "Cars"), ("9844", "Ford")]))
        );
    }

    #[test]
    fn ancestors_with_no_levels_are_taken_nearest_parent_first() {
        let body: Value = serde_json::from_str(
            r#"{"categorySuggestions":[{
                "category":{"categoryId":"3","categoryName":"Leaf"},
                "categoryTreeNodeAncestors":[
                    {"categoryId":"2","categoryName":"Parent"},
                    {"categoryId":"1","categoryName":"Root"}]}]}"#,
        )
        .unwrap();

        assert_eq!(
            parse_category_chain(&body),
            Some(chain(&[("1", "Root"), ("2", "Parent"), ("3", "Leaf")]))
        );
    }

    #[test]
    fn no_suggestions_is_none_not_an_error() {
        let body: Value = serde_json::from_str(r#"{"categoryTreeId":"3"}"#).unwrap();

        assert_eq!(parse_category_chain(&body), None);
    }

    #[test]
    fn a_suggestion_missing_its_id_or_name_is_none() {
        let no_name: Value =
            serde_json::from_str(r#"{"categorySuggestions":[{"category":{"categoryId":"9801"}}]}"#).unwrap();
        let blank_id: Value = serde_json::from_str(
            r#"{"categorySuggestions":[{"category":{"categoryId":" ","categoryName":"Cars"}}]}"#,
        )
        .unwrap();

        assert_eq!(parse_category_chain(&no_name), None);
        assert_eq!(parse_category_chain(&blank_id), None);
    }

    #[test]
    fn terms_in_different_makes_agree_on_the_cars_category_above_them() {
        let votes = [cars("9844", "Ford"), cars("9858", "Vauxhall/Opel"), cars("9880", "Toyota")];

        assert_eq!(vote_category(&votes), Some(node("9801", "Cars")));
    }

    #[test]
    fn terms_in_the_same_leaf_agree_on_the_leaf() {
        let votes = [cars("9844", "Ford"), cars("9844", "Ford")];

        assert_eq!(vote_category(&votes), Some(node("9844", "Ford")));
    }

    #[test]
    fn one_term_that_lands_somewhere_silly_is_outvoted() {
        let parts = chain(&[("131090", "Vehicle Parts & Accessories"), ("33660", "Fuel Filters")]);
        let votes = [cars("9844", "Ford"), cars("9858", "Vauxhall/Opel"), cars("9880", "Toyota"), parts];

        assert_eq!(vote_category(&votes), Some(node("9801", "Cars")));
    }

    #[test]
    fn no_majority_means_no_category_rather_than_a_guess() {
        let votes = [
            cars("9844", "Ford"),
            chain(&[("11450", "Clothes"), ("15724", "Women")]),
            chain(&[("619", "Musical Instruments"), ("33034", "Electric Guitars")]),
        ];

        assert_eq!(vote_category(&votes), None);
    }

    #[test]
    fn exactly_half_is_not_a_majority() {
        let votes = [cars("9844", "Ford"), chain(&[("11450", "Clothes")])];

        // Only the Ford chain's nodes have 1 of 2 votes: not more than half.
        assert_eq!(vote_category(&votes), None);
    }

    #[test]
    fn nothing_to_vote_on_is_none() {
        assert_eq!(vote_category(&[]), None);
    }

    #[test]
    fn knows_the_uk_and_refuses_to_guess_the_rest() {
        assert_eq!(marketplace_currency("EBAY_GB"), Some("GBP"));
        assert_eq!(category_tree_id("EBAY_GB"), Some("3"));
        assert_eq!(marketplace_currency("EBAY_MARS"), None);
        assert_eq!(category_tree_id("EBAY_MARS"), None);
    }

    #[test]
    fn returns_none_for_non_numeric_slug_tail() {
        assert_eq!(
            parse_legacy_item_id("https://www.ebay.co.uk/itm/Fender-Strat/"),
            None
        );
    }
}
