//! eBay bargain-finder ("Hunts") HTTP API — see plans/ebay-bargain-finder.md.
//! Coordinator-only (no agent-side capability): term-generation and the
//! bargain-verdict step both go through the same cloud gateway `chat.rs`
//! uses (`crate::cloud::GatewayConfig`), and every network call falls back
//! to a heuristic rather than failing outright when the gateway isn't
//! configured.

use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use chrono::Timelike;
use ebay::{EbayClient, EbayError, HuntSpec, ItemDetail, Listing, TermEntry};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::http::auth::Authed;
use crate::http::state::DashboardState;
use crate::registry::{EbayFindRecord, Registry};

/// Preferences namespace (user_id) under which eBay creds + ntfy topic are
/// stored, mirroring `crate::cloud::GATEWAY_USER`.
pub const EBAY_USER: &str = "__ebay__";

fn default_marketplace() -> String {
    "EBAY_GB".into()
}

// ── config: client_id/secret + ntfy topic, via dashboard_preferences ───────

struct EbayCreds {
    client_id: String,
    client_secret: String,
    ntfy_topic_url: Option<String>,
}

fn load_ebay_creds(reg: &Registry) -> EbayCreds {
    let pref = |k: &str| reg.get_preference(EBAY_USER, k).filter(|v| !v.is_empty());
    EbayCreds {
        client_id: pref("client_id")
            .or_else(|| std::env::var("EBAY_CLIENT_ID").ok())
            .unwrap_or_default(),
        client_secret: pref("client_secret")
            .or_else(|| std::env::var("EBAY_CLIENT_SECRET").ok())
            .unwrap_or_default(),
        ntfy_topic_url: pref("ntfy_topic_url").or_else(|| std::env::var("NTFY_TOPIC_URL").ok()),
    }
}

fn key_hint(k: &str) -> String {
    let tail: String = k
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("…{tail}")
}

#[derive(Serialize, Default)]
pub struct EbayConfigSnapshot {
    client_id_set: bool,
    client_secret_set: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    client_secret_hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ntfy_topic_url: Option<String>,
}

fn config_snapshot(creds: &EbayCreds) -> EbayConfigSnapshot {
    EbayConfigSnapshot {
        client_id_set: !creds.client_id.is_empty(),
        client_secret_set: !creds.client_secret.is_empty(),
        client_secret_hint: (!creds.client_secret.is_empty())
            .then(|| key_hint(&creds.client_secret)),
        ntfy_topic_url: creds.ntfy_topic_url.clone(),
    }
}

/// `GET /api/ebay/config` — masked eBay credential status + ntfy topic.
pub async fn get_config(
    _: Authed,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
) -> impl IntoResponse {
    let creds = load_ebay_creds(&registry.lock().unwrap());
    Json(config_snapshot(&creds)).into_response()
}

#[derive(Deserialize)]
pub struct SetEbayConfigBody {
    client_id: Option<String>,
    client_secret: Option<String>,
    ntfy_topic_url: Option<String>,
}

/// `POST /api/ebay/config` — set any subset of client_id/client_secret/ntfy
/// topic. Fields not present are left unchanged (an empty string clears one).
pub async fn set_config(
    _: Authed,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    Json(body): Json<SetEbayConfigBody>,
) -> impl IntoResponse {
    {
        let reg = registry.lock().unwrap();
        if let Some(v) = body.client_id.as_deref() {
            reg.set_preference(EBAY_USER, "client_id", v.trim());
        }
        if let Some(v) = body.client_secret.as_deref() {
            reg.set_preference(EBAY_USER, "client_secret", v.trim());
        }
        if let Some(v) = body.ntfy_topic_url.as_deref() {
            reg.set_preference(EBAY_USER, "ntfy_topic_url", v.trim());
        }
    }
    let creds = load_ebay_creds(&registry.lock().unwrap());
    Json(config_snapshot(&creds)).into_response()
}

fn build_client(reg: &Registry) -> Option<EbayClient> {
    let creds = load_ebay_creds(reg);
    if creds.client_id.is_empty() || creds.client_secret.is_empty() {
        return None;
    }
    Some(EbayClient::new(creds.client_id, creds.client_secret))
}

// ── analyze: pasted URL -> item detail + LLM-suggested search terms ────────

#[derive(Deserialize)]
pub struct AnalyzeBody {
    url: String,
}

#[derive(Serialize)]
pub struct AnalyzeResponse {
    item_id: String,
    title: String,
    terms: Vec<TermEntry>,
    marketplace: String,
}

/// `POST /api/ebay/analyze` — look up the pasted eBay item URL, then ask the
/// cloud gateway for good search terms (the clean title plus realistic
/// misspellings/mis-listings). Falls back to a single term (the item's own
/// title) if the gateway isn't configured or the call fails — never a hard
/// error, since the raw title alone is still a usable hunt.
pub async fn analyze(
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    _: Authed,
    Json(body): Json<AnalyzeBody>,
) -> impl IntoResponse {
    let url = body.url.trim().to_owned();
    if url.is_empty() {
        return (StatusCode::BAD_REQUEST, "url must not be empty").into_response();
    }
    if ebay::client::parse_legacy_item_id(&url).is_none() {
        return (
            StatusCode::BAD_REQUEST,
            "could not find an eBay item id in that URL",
        )
            .into_response();
    }
    let client = { build_client(&registry.lock().unwrap()) };
    let Some(client) = client else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "eBay client_id/client_secret not configured",
        )
            .into_response();
    };
    let item = match client.lookup_item(&url).await {
        Ok(item) => item,
        Err(e) => {
            let status = match e {
                EbayError::RateLimited => StatusCode::TOO_MANY_REQUESTS,
                _ => StatusCode::BAD_GATEWAY,
            };
            return (status, e.to_string()).into_response();
        }
    };
    let terms = generate_terms(&item, &registry).await;
    Json(AnalyzeResponse {
        item_id: item.item_id,
        title: item.title,
        terms,
        marketplace: default_marketplace(),
    })
    .into_response()
}

async fn generate_terms(item: &ItemDetail, registry: &Arc<Mutex<Registry>>) -> Vec<TermEntry> {
    let fallback = || {
        vec![TermEntry {
            text: item.title.clone(),
            enabled: true,
            is_misspelling: false,
        }]
    };
    let cfg = crate::cloud::GatewayConfig::load(&registry.lock().unwrap());
    let Some(provider) = cfg.provider() else {
        return fallback();
    };
    let category = item
        .category
        .clone()
        .unwrap_or_else(|| "unknown category".into());
    let condition = item
        .condition
        .clone()
        .unwrap_or_else(|| "unknown condition".into());
    let prompt = format!(
        "Someone wants to find a bargain on eBay for this item: \"{}\" (category: {category}, \
         condition: {condition}). Suggest up to 8 good eBay search terms for finding this cheap: \
         the correct/clean name, plus 3-6 realistic misspellings, abbreviations, or \
         mis-categorisations sellers might use that would make this item easy for other buyers to \
         miss (and thus underpriced). Reply with ONLY a JSON array of objects like \
         [{{\"text\":\"...\",\"is_misspelling\":false}}]. No other text.",
        item.title,
    );
    match provider
        .complete(&[shared::ChatTurn::user(prompt)], 0.4)
        .await
    {
        Ok(reply) => parse_term_candidates(&reply.text).unwrap_or_else(fallback),
        Err(e) => {
            tracing::warn!(error = %e, "ebay term-generation LLM call failed");
            fallback()
        }
    }
}

#[derive(Deserialize)]
struct TermCandidate {
    text: String,
    #[serde(default)]
    is_misspelling: bool,
}

/// Pull a JSON array of term candidates out of `text` — small/free models
/// sometimes wrap the array in prose despite instructions (see
/// `api::art::parse_curated_indices`'s doc comment for the same issue).
fn parse_term_candidates(text: &str) -> Option<Vec<TermEntry>> {
    let json_str = extract_json_array(text)?;
    let candidates: Vec<TermCandidate> = serde_json::from_str(json_str).ok()?;
    if candidates.is_empty() {
        return None;
    }
    Some(
        candidates
            .into_iter()
            .filter(|c| !c.text.trim().is_empty())
            .map(|c| TermEntry {
                text: c.text,
                enabled: true,
                is_misspelling: c.is_misspelling,
            })
            .collect(),
    )
}

/// First `[...]` substring in `text` — shared by term-candidate and
/// bargain-verdict parsing.
fn extract_json_array(text: &str) -> Option<&str> {
    let start = text.find('[')?;
    let end = text[start..].find(']')? + start;
    Some(&text[start..=end])
}

// ── hunts CRUD ───────────────────────────────────────────────────────────

pub async fn list_hunts(
    _: Authed,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
) -> impl IntoResponse {
    Json(registry.lock().unwrap().list_hunts()).into_response()
}

#[derive(Deserialize)]
pub struct CreateHuntBody {
    name: String,
    source_url: String,
    #[serde(default)]
    terms: Vec<TermEntry>,
    #[serde(default)]
    timeslots: Vec<u16>,
    #[serde(default = "default_marketplace")]
    marketplace: String,
}

pub async fn create_hunt(
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    _: Authed,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<CreateHuntBody>,
) -> impl IntoResponse {
    let name = body.name.trim().to_owned();
    if name.is_empty() {
        return (StatusCode::BAD_REQUEST, "name must not be empty").into_response();
    }
    let hunt = registry.lock().unwrap().create_hunt(
        &name,
        &body.source_url,
        body.terms,
        body.timeslots,
        &body.marketplace,
    );
    if hunt.enabled {
        arm_hunt_timer(state.clone(), registry.clone(), hunt.clone());
    }
    Json(hunt).into_response()
}

#[derive(Deserialize, Default)]
pub struct UpdateHuntBody {
    name: Option<String>,
    terms: Option<Vec<TermEntry>>,
    timeslots: Option<Vec<u16>>,
    marketplace: Option<String>,
    enabled: Option<bool>,
}

pub async fn update_hunt(
    Path(id): Path<String>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    _: Authed,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<UpdateHuntBody>,
) -> impl IntoResponse {
    let updated = registry.lock().unwrap().update_hunt(
        &id,
        body.name.as_deref(),
        body.terms,
        body.timeslots,
        body.marketplace.as_deref(),
        body.enabled,
    );
    let Some(hunt) = updated else {
        return (StatusCode::NOT_FOUND, "hunt not found").into_response();
    };
    // Any update (timeslots, terms, enabled) invalidates an outstanding
    // timer — re-arm picks up the fresh spec; disabling just kills it.
    if hunt.enabled {
        arm_hunt_timer(state.clone(), registry.clone(), hunt.clone());
    } else {
        state.bump_ebay_hunt_generation(&hunt.id);
    }
    Json(hunt).into_response()
}

pub async fn delete_hunt(
    Path(id): Path<String>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    _: Authed,
    State(state): State<Arc<DashboardState>>,
) -> impl IntoResponse {
    let existed = registry.lock().unwrap().delete_hunt(&id);
    if !existed {
        return StatusCode::NOT_FOUND.into_response();
    }
    state.remove_ebay_hunt_generation(&id);
    StatusCode::NO_CONTENT.into_response()
}

// ── background timer ────────────────────────────────────────────────────

/// Seconds since local midnight, right now — feeds `ebay::schedule::next_wakeup`.
/// Local (not UTC) time so the OS/chrono timezone database handles BST/GMT
/// transitions rather than a naive UTC assumption silently drifting hunts by
/// an hour twice a year; see plans/ebay-bargain-finder.md.
fn local_secs_since_midnight() -> u32 {
    let now = chrono::Local::now();
    now.hour() * 3600 + now.minute() * 60 + now.second()
}

/// (Re-)arm `hunt`'s background timer: capture its current generation and
/// spawn a loop that sleeps until the next configured timeslot, runs one
/// search cycle, and repeats — self-cancelling as soon as its captured
/// generation no longer matches (a newer update/delete/re-arm superseded
/// it), the same trick as `art.rs`'s rotation timer but keyed per hunt since
/// hunts run concurrently. No-ops (spawns nothing) if `hunt` has no
/// timeslots configured yet.
fn arm_hunt_timer(state: Arc<DashboardState>, registry: Arc<Mutex<Registry>>, hunt: HuntSpec) {
    if hunt.timeslots.is_empty() {
        return;
    }
    let generation = state.bump_ebay_hunt_generation(&hunt.id);
    let hunt_id = hunt.id.clone();
    tokio::spawn(async move {
        loop {
            let sleep_for =
                ebay::schedule::next_wakeup(local_secs_since_midnight(), &hunt.timeslots);
            if sleep_for == Duration::MAX {
                break;
            }
            tokio::time::sleep(sleep_for).await;
            let current = state
                .ebay_hunt_generation(&hunt_id)
                .load(std::sync::atomic::Ordering::SeqCst);
            if current != generation {
                break; // superseded by a newer update/delete/re-arm
            }
            let Some(current_hunt) = registry.lock().unwrap().get_hunt(&hunt_id) else {
                break; // deleted
            };
            if !current_hunt.enabled {
                break;
            }
            match run_hunt_cycle(&current_hunt, &registry, &state).await {
                Ok(count) => {
                    tracing::info!(hunt_id = %hunt_id, count, "ebay hunt cycle complete")
                }
                Err(e) => {
                    tracing::warn!(hunt_id = %hunt_id, error = %e, "ebay hunt cycle failed")
                }
            }
        }
    });
}

/// Re-arm every enabled hunt's background timer from persisted rows — called
/// once at coordinator startup (`http::rearm_ebay_hunts`), since (unlike
/// art's session-only rotation) the whole point of a hunt is "check for me
/// while I'm not looking," so it must survive a restart.
pub(crate) fn rearm_all_hunts(state: Arc<DashboardState>, registry: Arc<Mutex<Registry>>) {
    let hunts = registry.lock().unwrap().list_hunts();
    let mut armed = 0;
    for hunt in hunts {
        if hunt.enabled && !hunt.timeslots.is_empty() {
            arm_hunt_timer(state.clone(), registry.clone(), hunt);
            armed += 1;
        }
    }
    tracing::info!(armed, "ebay: re-armed hunt timers at startup");
}

// ── run cycle: search -> diff -> verdict -> persist/broadcast/notify ───────

pub async fn run_now(
    Path(id): Path<String>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    _: Authed,
    State(state): State<Arc<DashboardState>>,
) -> impl IntoResponse {
    let hunt = registry.lock().unwrap().get_hunt(&id);
    let Some(hunt) = hunt else {
        return (StatusCode::NOT_FOUND, "hunt not found").into_response();
    };
    match run_hunt_cycle(&hunt, &registry, &state).await {
        Ok(count) => Json(serde_json::json!({ "new_listings": count })).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, e).into_response(),
    }
}

/// One search cycle for `hunt`: search each enabled term, diff against
/// already-seen listings, get a bargain verdict (LLM batch, or heuristic
/// fallback), persist + broadcast every new listing, and ntfy the bargains.
/// Shared by `run_now` and the background timer. Returns how many new
/// listings were found.
pub async fn run_hunt_cycle(
    hunt: &HuntSpec,
    registry: &Arc<Mutex<Registry>>,
    state: &Arc<DashboardState>,
) -> Result<usize, String> {
    let creds = load_ebay_creds(&registry.lock().unwrap());
    if creds.client_id.is_empty() || creds.client_secret.is_empty() {
        return Err("eBay client_id/client_secret not configured".into());
    }
    let terms = hunt.active_terms();
    if terms.is_empty() {
        return Ok(0);
    }
    let client = EbayClient::new(creds.client_id, creds.client_secret);
    let mut term_matches: Vec<(String, Listing)> = Vec::new();
    for term in &terms {
        match client
            .search(std::slice::from_ref(term), &hunt.marketplace)
            .await
        {
            Ok(listings) => term_matches.extend(listings.into_iter().map(|l| (term.clone(), l))),
            Err(EbayError::RateLimited) => {
                tracing::warn!(hunt_id = %hunt.id, term = %term, "eBay rate limited, skipping this cycle");
                return Err("eBay rate limited".into());
            }
            Err(e) => {
                tracing::warn!(hunt_id = %hunt.id, term = %term, error = %e, "eBay search failed for term");
            }
        }
    }
    if term_matches.is_empty() {
        return Ok(0);
    }

    let verdicts = get_verdicts(hunt, &term_matches, registry).await;
    let processed = {
        let reg = registry.lock().unwrap();
        process_hunt_results(hunt, &term_matches, &verdicts, &reg, state)
    };
    let count = processed.len();

    if let Some(topic) = creds.ntfy_topic_url.filter(|t| !t.is_empty()) {
        for (find, notify) in &processed {
            if !notify {
                continue;
            }
            let title = format!("eBay find: {}", hunt.name);
            let price = find
                .price_minor
                .map(|p| {
                    format!(
                        "{:.2} {}",
                        p as f64 / 100.0,
                        find.currency.clone().unwrap_or_default()
                    )
                })
                .unwrap_or_else(|| "price unknown".into());
            let body = format!("{} — {price}", find.title);
            if let Err(e) =
                ebay::ntfy::send_ntfy(&topic, &title, &body, Some(&find.item_web_url)).await
            {
                tracing::warn!(error = %e, "ebay ntfy push failed");
            }
        }
    }

    Ok(count)
}

/// Diff `term_matches` (each listing tagged with the term that surfaced it)
/// against already-seen ids for `hunt`, persist + broadcast every new one,
/// and return each with whether it should get a phone push. Duplicate
/// `item_id`s across terms within one cycle are deduped, keeping the first
/// term that matched. Pure w.r.t. network — the only I/O is the registry
/// (sync/local) and the WS broadcast — so this is unit-testable without a
/// live eBay/LLM call; see the tests below.
fn process_hunt_results(
    hunt: &HuntSpec,
    term_matches: &[(String, Listing)],
    verdicts: &HashMap<String, (bool, String)>,
    registry: &Registry,
    state: &DashboardState,
) -> Vec<(EbayFindRecord, bool)> {
    let mut deduped: HashMap<String, (String, Listing)> = HashMap::new();
    for (term, listing) in term_matches {
        deduped
            .entry(listing.item_id.clone())
            .or_insert_with(|| (term.clone(), listing.clone()));
    }
    let listings: Vec<Listing> = deduped.values().map(|(_, l)| l.clone()).collect();
    let seen = registry.seen_listing_ids(&hunt.id);
    let fresh = ebay::diff::new_listings(&seen, &listings);

    let mut results = Vec::new();
    for listing in fresh {
        let term = deduped
            .get(&listing.item_id)
            .map(|(t, _)| t.clone())
            .unwrap_or_default();
        let (is_bargain, verdict_text) = match verdicts.get(&listing.item_id) {
            Some((bargain, reason)) => (
                *bargain,
                Some(if *bargain {
                    format!("bargain: {reason}")
                } else {
                    format!("not a bargain: {reason}")
                }),
            ),
            // Heuristic mode (gateway unconfigured) or the LLM's reply
            // omitted this item: unjudged, but still notify-worthy.
            None => (true, None),
        };
        let record = registry.insert_find(&hunt.id, &listing, &term, verdict_text.as_deref());
        registry.mark_listing_seen(&hunt.id, &listing.item_id);
        state.push_ebay_find(&hunt.id, &hunt.name, record.clone());
        results.push((record, is_bargain));
    }
    results
}

async fn get_verdicts(
    hunt: &HuntSpec,
    term_matches: &[(String, Listing)],
    registry: &Arc<Mutex<Registry>>,
) -> HashMap<String, (bool, String)> {
    let cfg = crate::cloud::GatewayConfig::load(&registry.lock().unwrap());
    let Some(provider) = cfg.provider() else {
        return HashMap::new();
    };
    let mut listing = String::new();
    for (_, l) in term_matches {
        let price = l
            .price_minor
            .map(|p| {
                format!(
                    "{:.2} {}",
                    p as f64 / 100.0,
                    l.currency.clone().unwrap_or_default()
                )
            })
            .unwrap_or_else(|| "price unknown".into());
        let condition = l
            .condition
            .clone()
            .unwrap_or_else(|| "condition unknown".into());
        listing.push_str(&format!(
            "- item_id {}: \"{}\" — {price} ({condition})\n",
            l.item_id, l.title
        ));
    }
    let prompt = format!(
        "A user is hunting for bargains related to \"{}\". Below are newly found eBay listings \
         matching their search terms. For each, decide if it looks like a genuine bargain \
         (underpriced, mis-listed, or a rare find) versus a normal-priced listing — a \
         suspiciously low price on a \"for parts/not working\" item is NOT a bargain. Reply with \
         ONLY a JSON array like [{{\"item_id\":\"...\",\"is_bargain\":true,\"reason\":\"...\"}}]. \
         No other text.\n\n{listing}",
        hunt.name,
    );
    let reply = match provider
        .complete(&[shared::ChatTurn::user(prompt)], 0.2)
        .await
    {
        Ok(r) => r.text,
        Err(e) => {
            tracing::warn!(error = %e, "ebay bargain-verdict LLM call failed");
            return HashMap::new();
        }
    };
    parse_verdicts(&reply)
}

#[derive(Deserialize)]
struct VerdictEntry {
    item_id: String,
    #[serde(default)]
    is_bargain: bool,
    #[serde(default)]
    reason: String,
}

fn parse_verdicts(text: &str) -> HashMap<String, (bool, String)> {
    let Some(json_str) = extract_json_array(text) else {
        return HashMap::new();
    };
    let entries: Vec<VerdictEntry> = serde_json::from_str(json_str).unwrap_or_default();
    entries
        .into_iter()
        .map(|e| (e.item_id, (e.is_bargain, e.reason)))
        .collect()
}

// ── finds feed ───────────────────────────────────────────────────────────

#[derive(Deserialize, Default)]
pub struct FindsQuery {
    hunt_id: Option<String>,
    #[serde(default = "default_finds_limit")]
    limit: u32,
}

fn default_finds_limit() -> u32 {
    50
}

pub async fn list_finds(
    _: Authed,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    Query(q): Query<FindsQuery>,
) -> impl IntoResponse {
    let finds = registry
        .lock()
        .unwrap()
        .list_finds(q.hunt_id.as_deref(), q.limit);
    Json(finds).into_response()
}

pub async fn mark_reviewed(
    Path(id): Path<String>,
    _: Authed,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
) -> impl IntoResponse {
    if registry.lock().unwrap().mark_find_reviewed(&id) {
        StatusCode::NO_CONTENT.into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::api::test_util::*;
    use axum::Router;
    use axum::routing::{get, patch, post};

    fn ebay_router(state: Arc<DashboardState>, registry: Arc<Mutex<Registry>>) -> Router {
        Router::new()
            .route("/api/ebay/analyze", post(analyze))
            .route("/api/ebay/hunts", get(list_hunts).post(create_hunt))
            .route(
                "/api/ebay/hunts/{id}",
                patch(update_hunt).delete(delete_hunt),
            )
            .route("/api/ebay/hunts/{id}/run-now", post(run_now))
            .route("/api/ebay/finds", get(list_finds))
            .route("/api/ebay/finds/{id}/reviewed", post(mark_reviewed))
            .route("/api/ebay/config", get(get_config).post(set_config))
            .layer(axum::Extension(registry))
            .with_state(state)
    }

    fn sample_listing(id: &str) -> Listing {
        Listing {
            item_id: id.to_string(),
            title: format!("Fender Strat {id}"),
            price_minor: Some(45000),
            currency: Some("GBP".into()),
            image_url: None,
            item_web_url: format!("https://ebay.co.uk/itm/{id}"),
            condition: Some("Used".into()),
        }
    }

    // ── create/update/delete hunts ──────────────────────────────────────

    #[tokio::test]
    async fn create_hunt_returns_400_for_empty_name() {
        let registry = make_registry();
        let state = make_state(vec![], empty_connections());
        let status = send(
            ebay_router(state, registry),
            "POST",
            "/api/ebay/hunts?token=",
            r#"{"name":"  ","source_url":"https://ebay.co.uk/itm/1"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn create_hunt_persists_and_lists_it() {
        let registry = make_registry();
        let state = make_state(vec![], empty_connections());
        let (status, body) = send_with_body(
            ebay_router(state.clone(), registry.clone()),
            "POST",
            "/api/ebay/hunts?token=",
            r#"{"name":"Strat","source_url":"https://ebay.co.uk/itm/123","terms":[],"timeslots":[]}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("\"name\":\"Strat\""), "body: {body}");
        assert_eq!(registry.lock().unwrap().list_hunts().len(), 1);
    }

    #[tokio::test]
    async fn update_hunt_returns_404_for_unknown_id() {
        let registry = make_registry();
        let state = make_state(vec![], empty_connections());
        let status = send(
            ebay_router(state, registry),
            "PATCH",
            "/api/ebay/hunts/nope?token=",
            r#"{"enabled":false}"#,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn update_hunt_disables_without_timeslots_configured() {
        let registry = make_registry();
        let hunt = registry.lock().unwrap().create_hunt(
            "Strat",
            "https://x",
            vec![],
            vec![480],
            "EBAY_GB",
        );
        let state = make_state(vec![], empty_connections());
        let (status, body) = send_with_body(
            ebay_router(state, registry.clone()),
            "PATCH",
            &format!("/api/ebay/hunts/{}?token=", hunt.id),
            r#"{"enabled":false}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("\"enabled\":false"), "body: {body}");
    }

    #[tokio::test]
    async fn delete_hunt_returns_404_for_unknown_id() {
        let registry = make_registry();
        let state = make_state(vec![], empty_connections());
        let status = send(
            ebay_router(state, registry),
            "DELETE",
            "/api/ebay/hunts/nope?token=",
            "",
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_hunt_removes_it() {
        let registry = make_registry();
        let hunt =
            registry
                .lock()
                .unwrap()
                .create_hunt("Strat", "https://x", vec![], vec![], "EBAY_GB");
        let state = make_state(vec![], empty_connections());
        let status = send(
            ebay_router(state, registry.clone()),
            "DELETE",
            &format!("/api/ebay/hunts/{}?token=", hunt.id),
            "",
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(registry.lock().unwrap().get_hunt(&hunt.id).is_none());
    }

    // ── analyze — validation only. The live lookup+LLM path isn't unit ─────
    // tested, same precedent as art.rs's live Met-API search: verified with
    // a real curl against the running coordinator instead.

    #[tokio::test]
    async fn analyze_returns_400_for_empty_url() {
        let registry = make_registry();
        let state = make_state(vec![], empty_connections());
        let status = send(
            ebay_router(state, registry),
            "POST",
            "/api/ebay/analyze?token=",
            r#"{"url":"  "}"#,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn analyze_returns_400_for_unparseable_url() {
        let registry = make_registry();
        let state = make_state(vec![], empty_connections());
        let status = send(
            ebay_router(state, registry),
            "POST",
            "/api/ebay/analyze?token=",
            r#"{"url":"https://ebay.co.uk/sch/i.html?_nkw=strat"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn analyze_returns_503_when_not_configured() {
        let registry = make_registry();
        let state = make_state(vec![], empty_connections());
        let status = send(
            ebay_router(state, registry),
            "POST",
            "/api/ebay/analyze?token=",
            r#"{"url":"https://ebay.co.uk/itm/123456789012"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    }

    // ── config ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn config_roundtrips_and_masks_secret() {
        let registry = make_registry();
        let state = make_state(vec![], empty_connections());
        let (status, body) = send_with_body(
            ebay_router(state.clone(), registry.clone()),
            "GET",
            "/api/ebay/config?token=",
            "",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("\"client_id_set\":false"), "body: {body}");

        let (status, body) = send_with_body(
            ebay_router(state, registry),
            "POST",
            "/api/ebay/config?token=",
            r#"{"client_id":"abc","client_secret":"supersecret1234","ntfy_topic_url":"https://ntfy.sh/x"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("\"client_id_set\":true"), "body: {body}");
        assert!(!body.contains("supersecret1234"), "secret leaked: {body}");
        assert!(body.contains("1234\""), "body: {body}");
    }

    // ── finds ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn list_finds_returns_inserted_records() {
        let registry = make_registry();
        let hunt =
            registry
                .lock()
                .unwrap()
                .create_hunt("Strat", "https://x", vec![], vec![], "EBAY_GB");
        registry
            .lock()
            .unwrap()
            .insert_find(&hunt.id, &sample_listing("1"), "strat", None);
        let state = make_state(vec![], empty_connections());
        let (status, body) = send_with_body(
            ebay_router(state, registry),
            "GET",
            "/api/ebay/finds?token=",
            "",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("Fender Strat 1"), "body: {body}");
    }

    #[tokio::test]
    async fn mark_reviewed_returns_404_for_unknown_id() {
        let registry = make_registry();
        let state = make_state(vec![], empty_connections());
        let status = send(
            ebay_router(state, registry),
            "POST",
            "/api/ebay/finds/nope/reviewed?token=",
            "",
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    // ── run_now ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn run_now_returns_404_for_unknown_hunt() {
        let registry = make_registry();
        let state = make_state(vec![], empty_connections());
        let status = send(
            ebay_router(state, registry),
            "POST",
            "/api/ebay/hunts/nope/run-now?token=",
            "",
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn run_now_returns_502_when_not_configured() {
        let registry = make_registry();
        let hunt = registry.lock().unwrap().create_hunt(
            "Strat",
            "https://x",
            vec![TermEntry {
                text: "fender strat".into(),
                enabled: true,
                is_misspelling: false,
            }],
            vec![],
            "EBAY_GB",
        );
        let state = make_state(vec![], empty_connections());
        let status = send(
            ebay_router(state, registry),
            "POST",
            &format!("/api/ebay/hunts/{}/run-now?token=", hunt.id),
            "",
        )
        .await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
    }

    // ── process_hunt_results — pure diff/verdict/persist/broadcast logic, ──
    // no network. This is the part the plan calls out as needing proof
    // independent of eBay/LLM timing.

    fn test_hunt(reg: &Registry) -> HuntSpec {
        reg.create_hunt("Strat", "https://x", vec![], vec![], "EBAY_GB")
    }

    #[test]
    fn process_hunt_results_inserts_fresh_listings() {
        let reg = Registry::new();
        let hunt = test_hunt(&reg);
        let state = DashboardState::new(Arc::new(vec![]), empty_connections());
        let matches = vec![("strat".to_string(), sample_listing("1"))];
        let results = process_hunt_results(&hunt, &matches, &HashMap::new(), &reg, &state);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0.item_id, "1");
        assert!(reg.has_seen_listing(&hunt.id, "1"));
    }

    #[test]
    fn process_hunt_results_skips_already_seen() {
        let reg = Registry::new();
        let hunt = test_hunt(&reg);
        reg.mark_listing_seen(&hunt.id, "1");
        let state = DashboardState::new(Arc::new(vec![]), empty_connections());
        let matches = vec![("strat".to_string(), sample_listing("1"))];
        let results = process_hunt_results(&hunt, &matches, &HashMap::new(), &reg, &state);
        assert!(results.is_empty());
    }

    #[test]
    fn process_hunt_results_dedupes_across_terms_keeping_first_term() {
        let reg = Registry::new();
        let hunt = test_hunt(&reg);
        let state = DashboardState::new(Arc::new(vec![]), empty_connections());
        let matches = vec![
            ("strat".to_string(), sample_listing("1")),
            ("stratocaster".to_string(), sample_listing("1")),
        ];
        let results = process_hunt_results(&hunt, &matches, &HashMap::new(), &reg, &state);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0.matched_term, "strat");
    }

    #[test]
    fn process_hunt_results_no_verdict_is_heuristic_notify() {
        let reg = Registry::new();
        let hunt = test_hunt(&reg);
        let state = DashboardState::new(Arc::new(vec![]), empty_connections());
        let matches = vec![("strat".to_string(), sample_listing("1"))];
        let results = process_hunt_results(&hunt, &matches, &HashMap::new(), &reg, &state);
        assert_eq!(results[0].0.verdict, None);
        assert!(results[0].1, "heuristic mode should still notify");
    }

    #[test]
    fn process_hunt_results_bargain_verdict_notifies() {
        let reg = Registry::new();
        let hunt = test_hunt(&reg);
        let state = DashboardState::new(Arc::new(vec![]), empty_connections());
        let matches = vec![("strat".to_string(), sample_listing("1"))];
        let mut verdicts = HashMap::new();
        verdicts.insert("1".to_string(), (true, "rare colour".to_string()));
        let results = process_hunt_results(&hunt, &matches, &verdicts, &reg, &state);
        assert_eq!(
            results[0].0.verdict.as_deref(),
            Some("bargain: rare colour")
        );
        assert!(results[0].1);
    }

    #[test]
    fn process_hunt_results_non_bargain_verdict_does_not_notify() {
        let reg = Registry::new();
        let hunt = test_hunt(&reg);
        let state = DashboardState::new(Arc::new(vec![]), empty_connections());
        let matches = vec![("strat".to_string(), sample_listing("1"))];
        let mut verdicts = HashMap::new();
        verdicts.insert("1".to_string(), (false, "fairly priced".to_string()));
        let results = process_hunt_results(&hunt, &matches, &verdicts, &reg, &state);
        assert_eq!(
            results[0].0.verdict.as_deref(),
            Some("not a bargain: fairly priced")
        );
        assert!(!results[0].1);
    }

    // ── parse_term_candidates / parse_verdicts / extract_json_array ────────

    #[test]
    fn parse_term_candidates_reads_plain_array() {
        let out = parse_term_candidates(
            r#"[{"text":"fender strat","is_misspelling":false},{"text":"fender strta","is_misspelling":true}]"#,
        )
        .unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].text, "fender strat");
        assert!(out[1].is_misspelling);
    }

    #[test]
    fn parse_term_candidates_extracts_from_surrounding_prose() {
        let out = parse_term_candidates(
            r#"Sure! Here are some terms: [{"text":"strat"}] — hope that helps!"#,
        )
        .unwrap();
        assert_eq!(out[0].text, "strat");
    }

    #[test]
    fn parse_term_candidates_none_for_garbage() {
        assert!(parse_term_candidates("I don't know").is_none());
    }

    #[test]
    fn parse_verdicts_reads_array() {
        let out = parse_verdicts(r#"[{"item_id":"1","is_bargain":true,"reason":"underpriced"}]"#);
        assert_eq!(out.get("1"), Some(&(true, "underpriced".to_string())));
    }

    #[test]
    fn parse_verdicts_empty_for_garbage() {
        assert!(parse_verdicts("not json").is_empty());
    }
}
