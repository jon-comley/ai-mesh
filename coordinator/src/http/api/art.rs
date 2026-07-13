//! Frame TV art display — coordinator side. See plans/frame-tv-art-display.md;
//! `show_art`/`get_art_status` are the original minimal v1 slice (§10 step
//! 3): push one image to whichever node currently advertises the `art`
//! feature, and read back its last reported status. `search_art`/`next_art`/
//! `get_art_current` build the actual slideshow on top of that once the
//! physical chain (coordinator → node → TV) was proven: search the Met
//! Museum's Open Access API on the fly (no local catalogue/ingest pipeline —
//! that's still deferred, see plan §5), optionally have the local LLM pick
//! the best subset for viewing, and auto-advance through the result on a
//! timer. TV input-switch/art-mode control (plan §7) is still separate,
//! deliberately deferred work.

use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::Deserialize;
use shared::{ArtBatchRequest, ArtShowRequest, ChatTurn, MeshMessage};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::gen_request_id;
use crate::http::auth::Authed;
use crate::http::state::{ArtRotationItem, DashboardState};
use crate::inference::dispatch_local_inference;
use crate::registry::Registry;

/// First node advertising the `art` feature, connected or not — only one is
/// expected to exist in practice (one TV, one Pi), so there's no need for the
/// room/device-style "which one" disambiguation other domains have. Whether
/// it's actually reachable *right now* is determined by `send_to_node`'s own
/// return value at the call site, not here.
fn art_node_id(registry: &Arc<Mutex<Registry>>) -> Option<String> {
    registry
        .lock()
        .unwrap()
        .nodes_with_feature(shared::Feature::Art)
        .into_iter()
        .map(|n| n.id)
        .next()
}

/// Shared by `show_art`, `search_art`, `next_art`, and the auto-advance
/// timer — one place that builds the request and calls `send_to_node`.
fn send_show_request(state: &DashboardState, node_id: &str, image_url: String) -> bool {
    state.send_to_node(
        node_id,
        MeshMessage::ArtShow(ArtShowRequest {
            request_id: gen_request_id(),
            image_url,
        }),
    )
}

#[derive(Deserialize)]
pub struct ShowArtBody {
    image_url: String,
}

/// `POST /api/art/show` — fetch `image_url` and display it fullscreen on
/// whichever node currently owns the art display.
pub async fn show_art(
    axum::extract::Extension(registry): axum::extract::Extension<Arc<Mutex<Registry>>>,
    _: Authed,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<ShowArtBody>,
) -> impl IntoResponse {
    let image_url = body.image_url.trim().to_owned();
    if image_url.is_empty() {
        return (StatusCode::BAD_REQUEST, "image_url must not be empty").into_response();
    }
    let Some(node_id) = art_node_id(&registry) else {
        return (StatusCode::SERVICE_UNAVAILABLE, "no art node connected").into_response();
    };
    if send_show_request(&state, &node_id, image_url) {
        StatusCode::NO_CONTENT.into_response()
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "art node not reachable").into_response()
    }
}

/// `GET /api/art/status` — the art node's last reported status (empty object
/// if nothing has ever reported in).
pub async fn get_art_status(
    _: Authed,
    State(state): State<Arc<DashboardState>>,
) -> impl IntoResponse {
    match state.get_art_snapshot() {
        Some(snap) => Json(serde_json::json!({
            "node_id": snap.node_id,
            "viewer_running": snap.viewer_running,
            "current_url": snap.current_url,
            "error": snap.error,
        }))
        .into_response(),
        None => Json(serde_json::json!({})).into_response(),
    }
}

// ── On-the-fly slideshow: search the Met's Open Access API, optionally let ──
// the local LLM curate the result, auto-advance on a timer. ─────────────────

/// How many of the Met's (often thousands of) matching object IDs to fetch
/// full details for — capped to keep one search fast and polite to the API.
const SEARCH_CANDIDATE_CAP: usize = 25;
/// Candidate fetch cap when `by_artist` is set — generous, since a named
/// artist's *actual* public-domain, has-image body of work at the Met is
/// typically far smaller than the "often thousands" of keyword hits the raw
/// search returns (most of which are tangential, not by that artist at all
/// — see `SearchArtBody::by_artist`'s doc comment).
const ARTIST_SEARCH_CANDIDATE_CAP: usize = 100;
/// Max images kept in the final rotation, whether LLM-curated or not.
const ROTATION_CAP: usize = 12;
const DEFAULT_INTERVAL_SECS: u64 = 30;
/// Floor on the auto-advance interval so a bad `interval_secs` value can't
/// hammer the art node (or, if ever pointed at a rate-limited source, the
/// upstream API) every few hundred milliseconds.
const MIN_INTERVAL_SECS: u64 = 5;
/// A broad, generic search term for the general slideshow — the Met's `q`
/// parameter is required (an empty value errors), and combined with
/// `isHighlight=true` this is deliberately wide rather than narrow (verified
/// live: 1,850 results), since "general" should mean "good art, anything",
/// not one specific theme.
const GENERAL_QUERY: &str = "art";
/// How many images the general slideshow's local batch holds — this many
/// get downloaded and cached *on the node itself* (see
/// `MeshMessage::ArtBatch`), so it's capped well below the Met's SD-card
/// budget on a Pi Zero 2 W, not just "fast to fetch" like the specific
/// search's cap.
const GENERAL_BATCH_CAP: usize = 30;
const GENERAL_INTERVAL_SECS: u64 = 45;
/// How long a specific search can sit un-engaged-with (no new search, no
/// manual `/api/art/next`) before the auto-advance timer gives up and
/// reverts to the general slideshow — see `ArtRotationState::last_engaged_at`.
const SPECIFIC_IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// How long LLM curation gets before giving up and falling back to the raw
/// search order — deliberately much shorter than the shared
/// `dispatch_local_inference`'s own 150s ceiling, so this nice-to-have can't
/// tie up a node's one-inference-at-a-time slot for long if a real
/// voice/chat request needs it. See `curate_with_llm`'s doc comment.
const CURATION_TIMEOUT: Duration = Duration::from_secs(20);

fn met_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .user_agent("ai-mesh-coordinator")
            .build()
            .unwrap_or_default()
    })
}

#[derive(Deserialize)]
struct MetSearchResponse {
    #[serde(rename = "objectIDs", default)]
    object_ids: Option<Vec<i64>>,
}

#[derive(Deserialize)]
struct MetObject {
    #[serde(rename = "primaryImage", default)]
    primary_image: String,
    #[serde(rename = "isPublicDomain", default)]
    is_public_domain: bool,
    #[serde(default)]
    title: String,
    #[serde(rename = "artistDisplayName", default)]
    artist_display_name: String,
    #[serde(rename = "objectDate", default)]
    object_date: String,
    #[serde(default)]
    medium: String,
}

/// Search the Met's collection for `query`, fetch details for up to `cap`
/// matches, and keep only public-domain entries that actually have an
/// image — the Met's collection includes plenty of catalogue records
/// (fragments, study photos, non-public-domain loans) that aren't a good
/// fit for a slideshow. `highlights_only` adds `isHighlight=true` — the
/// Met's own curators' picks, used for the general slideshow so it doesn't
/// need LLM curation to look good (verified live: `q=art&isHighlight=true`
/// alone returns 1,850 results, so this is a genuinely broad "just show me
/// good art" pool, not a narrow one).
async fn fetch_met_candidates(
    query: &str,
    cap: usize,
    highlights_only: bool,
) -> Result<Vec<MetObject>, String> {
    let mut params = vec![("q", query), ("hasImages", "true")];
    if highlights_only {
        params.push(("isHighlight", "true"));
    }
    let resp = met_client()
        .get("https://collectionapi.metmuseum.org/public/collection/v1/search")
        .query(&params)
        .send()
        .await
        .map_err(|e| format!("could not reach the Met's API: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("Met search failed: HTTP {}", resp.status()));
    }
    let search: MetSearchResponse = resp
        .json()
        .await
        .map_err(|e| format!("unexpected response from the Met's search API: {e}"))?;
    let ids = search.object_ids.unwrap_or_default();
    if ids.is_empty() {
        return Err(format!("no results from the Met for \"{query}\""));
    }

    let fetches = ids.into_iter().take(cap).map(|id| async move {
        let url = format!("https://collectionapi.metmuseum.org/public/collection/v1/objects/{id}");
        let resp = met_client().get(&url).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        resp.json::<MetObject>().await.ok()
    });
    let candidates: Vec<MetObject> = futures_util::future::join_all(fetches)
        .await
        .into_iter()
        .flatten()
        .filter(|o| o.is_public_domain && !o.primary_image.is_empty())
        .collect();
    if candidates.is_empty() {
        return Err(format!(
            "no public-domain images with usable pictures found for \"{query}\""
        ));
    }
    Ok(candidates)
}

/// Ask whatever local model is currently Ready to pick the best subset of
/// `candidates` for a slideshow, in a good viewing order. Returns `None` on
/// *any* failure (no model ready, inference error, unparsable reply) — this
/// is a nice-to-have layered on top of a perfectly usable raw result, not a
/// hard dependency, so every failure mode falls back rather than erroring
/// the whole search out.
async fn curate_with_llm(
    query: &str,
    candidates: &[MetObject],
    registry: &Arc<Mutex<Registry>>,
    state: &Arc<DashboardState>,
) -> Option<Vec<usize>> {
    let model = registry.lock().unwrap().any_ready_llm_model()?;

    let mut listing = String::new();
    for (i, obj) in candidates.iter().enumerate() {
        let title = if obj.title.is_empty() {
            "Untitled"
        } else {
            &obj.title
        };
        let artist = if obj.artist_display_name.is_empty() {
            "unknown artist"
        } else {
            &obj.artist_display_name
        };
        let date = if obj.object_date.is_empty() {
            "date unknown"
        } else {
            &obj.object_date
        };
        let medium = if obj.medium.is_empty() {
            String::new()
        } else {
            format!(", {}", obj.medium)
        };
        listing.push_str(&format!(
            "{}. \"{title}\" — {artist} ({date}){medium}\n",
            i + 1
        ));
    }
    let prompt = format!(
        "Below is a numbered list of {} artworks matching a search for \"{query}\". \
         Pick up to {ROTATION_CAP} of the best, most visually interesting ones for a \
         home slideshow, in a good viewing order. Reply with ONLY a JSON array of the \
         chosen numbers, e.g. [3,1,9]. No other text.\n\n{listing}",
        candidates.len(),
    );

    let request_id = format!("art-curate-{}", gen_request_id());
    // `dispatch_local_inference`'s own timeout is 150s — far too generous for
    // a nice-to-have curation step layered on a node that's also the shared
    // path for real voice/chat commands (the agent enforces one inference at
    // a time per node, deliberately, to protect GPU/RAM — see
    // capabilities/llm's INFER_SEM). Giving up after CURATION_TIMEOUT and
    // falling back to the raw result caps how long this can ever compete
    // with something a person is actually waiting on. Abandoning the wait
    // early is safe: the coordinator's inference-result routing removes the
    // `pending_inferences` entry unconditionally when the (now-unwanted)
    // reply eventually arrives, so nothing leaks.
    let result = tokio::time::timeout(
        CURATION_TIMEOUT,
        dispatch_local_inference(
            &request_id,
            &model,
            vec![ChatTurn::user(prompt)],
            512,
            Some(0.2),
            registry,
            &state.connections,
            &state.pending_inferences,
        ),
    )
    .await
    .ok()?
    .ok()?;
    parse_curated_indices(&result.output, candidates.len())
}

/// Pull a JSON array of (1-based) picks out of `text` — small local models
/// sometimes wrap the array in a sentence despite being told not to, so this
/// looks for the first `[...]` substring rather than requiring the whole
/// reply to be pure JSON. Converts to 0-based indices, drops anything
/// out-of-range or repeated, and returns `None` if nothing usable survives.
fn parse_curated_indices(text: &str, candidate_count: usize) -> Option<Vec<usize>> {
    let start = text.find('[')?;
    let end = text[start..].find(']')? + start;
    let numbers: Vec<i64> = serde_json::from_str(&text[start..=end]).ok()?;
    let mut seen = std::collections::HashSet::new();
    let indices: Vec<usize> = numbers
        .into_iter()
        .filter_map(|n| usize::try_from(n - 1).ok())
        .filter(|&i| i < candidate_count && seen.insert(i))
        .collect();
    if indices.is_empty() {
        None
    } else {
        Some(indices)
    }
}

fn to_rotation_item(obj: &MetObject) -> ArtRotationItem {
    ArtRotationItem {
        image_url: obj.primary_image.clone(),
        title: if obj.title.is_empty() {
            "Untitled".into()
        } else {
            obj.title.clone()
        },
        artist: if obj.artist_display_name.is_empty() {
            "Unknown artist".into()
        } else {
            obj.artist_display_name.clone()
        },
        date: obj.object_date.clone(),
    }
}

#[derive(Deserialize)]
pub struct SearchArtBody {
    query: String,
    #[serde(default)]
    interval_secs: Option<u64>,
    /// Show *every* public-domain, has-image work whose own artist field
    /// actually matches `query` — not just a keyword hit anywhere in the
    /// Met's record (confirmed live: a plain "Claude Monet" search surfaces
    /// works merely *about* him, e.g. a portrait of his family painted by
    /// Manet). Skips both the LLM curation subset-pick and `ROTATION_CAP`.
    /// Off by default — a curated best-of-N is the better fit for a broad
    /// or generic query, where "every match" could be huge and mostly
    /// irrelevant.
    #[serde(default)]
    by_artist: bool,
}

/// `POST /api/art/search` — search the Met's Open Access collection for
/// `query`, show the first (optionally LLM-curated) result immediately, and
/// start auto-advancing through the rest every `interval_secs` (default 30s,
/// floor 5s). No local catalogue involved — this hits the Met's API live on
/// every search, matching the "serve them up on the fly" ask rather than
/// requiring the batch curation pipeline from plan §5 to exist first.
pub async fn search_art(
    axum::extract::Extension(registry): axum::extract::Extension<Arc<Mutex<Registry>>>,
    _: Authed,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<SearchArtBody>,
) -> impl IntoResponse {
    let query = body.query.trim().to_owned();
    if query.is_empty() {
        return (StatusCode::BAD_REQUEST, "query must not be empty").into_response();
    }
    let Some(node_id) = art_node_id(&registry) else {
        return (StatusCode::SERVICE_UNAVAILABLE, "no art node connected").into_response();
    };

    let candidate_cap = if body.by_artist {
        ARTIST_SEARCH_CANDIDATE_CAP
    } else {
        SEARCH_CANDIDATE_CAP
    };
    let mut candidates = match fetch_met_candidates(&query, candidate_cap, false).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, query = %query, "art search failed");
            return (StatusCode::BAD_GATEWAY, e).into_response();
        }
    };
    if body.by_artist {
        let needle = query.to_lowercase();
        candidates.retain(|o| o.artist_display_name.to_lowercase().contains(&needle));
        if candidates.is_empty() {
            return (
                StatusCode::BAD_GATEWAY,
                format!("no public-domain images actually credited to \"{query}\" found"),
            )
                .into_response();
        }
    }

    let order = if body.by_artist {
        tracing::info!(
            query = %query,
            count = candidates.len(),
            "art search: showing full artist collection, no curation or cap"
        );
        (0..candidates.len()).collect()
    } else {
        match curate_with_llm(&query, &candidates, &registry, &state).await {
            Some(indices) => {
                tracing::info!(query = %query, count = indices.len(), "art search: LLM-curated order");
                indices
            }
            None => {
                tracing::info!(query = %query, "art search: no LLM curation available, using raw order");
                (0..candidates.len().min(ROTATION_CAP)).collect()
            }
        }
    };

    let items: Vec<ArtRotationItem> = order
        .into_iter()
        .map(|i| to_rotation_item(&candidates[i]))
        .collect();
    let generation = state.set_art_rotation(query.clone(), items);
    let Some(first) = state.art_rotation_current_item() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "rotation built but empty",
        )
            .into_response();
    };
    if !send_show_request(&state, &node_id, first.image_url) {
        return (StatusCode::SERVICE_UNAVAILABLE, "art node not reachable").into_response();
    }

    let interval = Duration::from_secs(
        body.interval_secs
            .unwrap_or(DEFAULT_INTERVAL_SECS)
            .max(MIN_INTERVAL_SECS),
    );
    spawn_art_rotation_timer(state.clone(), registry.clone(), generation, interval);

    Json(state.art_rotation_status().unwrap_or_default()).into_response()
}

/// Background auto-advance for the slideshow — one of these is spawned per
/// search and quietly stops itself as soon as `generation` no longer matches
/// the live rotation (a newer search replaced it, or nothing to show at
/// all), rather than fighting over which rotation should be on screen. Also
/// reverts to the general slideshow once the rotation's gone un-engaged-with
/// for `SPECIFIC_IDLE_TIMEOUT` — see `ArtRotationState::last_engaged_at`.
fn spawn_art_rotation_timer(
    state: Arc<DashboardState>,
    registry: Arc<Mutex<Registry>>,
    generation: u64,
    interval: Duration,
) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(interval).await;
            if state.art_rotation_idle_for(SPECIFIC_IDLE_TIMEOUT) {
                state.clear_art_rotation();
                if let Some(node_id) = art_node_id(&registry) {
                    match send_general_batch(&state, &node_id, false).await {
                        Ok(count) => {
                            tracing::info!(
                                count,
                                "art: idle timeout, reverted to general slideshow"
                            )
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "art: could not revert to general slideshow")
                        }
                    }
                }
                break;
            }
            let Some(item) = state.advance_art_rotation_if_current(generation) else {
                break;
            };
            let Some(node_id) = art_node_id(&registry) else {
                break;
            };
            // A momentarily-unreachable node just misses this beat — the
            // rotation itself stays alive and the next tick tries again,
            // rather than tearing down the whole slideshow over one blip.
            let _ = send_show_request(&state, &node_id, item.image_url);
        }
    });
}

/// Build (or reuse the cached) general-slideshow batch and hand it to the
/// node as one `ArtBatch` — shared by `POST /api/art/general` and the
/// auto-advance timer's idle-timeout revert above.
async fn send_general_batch(
    state: &Arc<DashboardState>,
    node_id: &str,
    force_refresh: bool,
) -> Result<usize, String> {
    let items = get_or_build_general_batch(state, force_refresh).await?;
    let urls: Vec<String> = items.iter().map(|i| i.image_url.clone()).collect();
    let count = urls.len();
    if !state.send_to_node(
        node_id,
        MeshMessage::ArtBatch(ArtBatchRequest {
            request_id: gen_request_id(),
            image_urls: urls,
            interval_secs: GENERAL_INTERVAL_SECS,
        }),
    ) {
        return Err("art node not reachable".into());
    }
    Ok(count)
}

/// Reuse the cached batch unless `force_refresh` (or nothing's cached yet) —
/// avoids re-querying the Met API every time a specific search goes idle.
async fn get_or_build_general_batch(
    state: &Arc<DashboardState>,
    force_refresh: bool,
) -> Result<Vec<ArtRotationItem>, String> {
    if !force_refresh {
        let cached = state.get_general_art_batch();
        if let Some(cached) = cached
            && !cached.is_empty()
        {
            return Ok(cached);
        }
    }
    let candidates = fetch_met_candidates(GENERAL_QUERY, GENERAL_BATCH_CAP, true).await?;
    let items: Vec<ArtRotationItem> = candidates.iter().map(to_rotation_item).collect();
    state.set_general_art_batch(items.clone());
    Ok(items)
}

#[derive(Deserialize, Default)]
pub struct GeneralArtBody {
    #[serde(default)]
    refresh: bool,
}

/// `POST /api/art/general` — (re)start the general/default slideshow: a
/// cached (or, with `{"refresh": true}`, freshly-fetched) batch of the Met's
/// own highlighted public-domain works, handed to the node in one
/// `ArtBatch` so it cycles locally from then on — no LLM curation needed
/// (the Met's `isHighlight` flag already is the curation), and no further
/// coordinator involvement per image. Cancels any active specific rotation.
pub async fn general_art(
    axum::extract::Extension(registry): axum::extract::Extension<Arc<Mutex<Registry>>>,
    _: Authed,
    State(state): State<Arc<DashboardState>>,
    Json(body): Json<GeneralArtBody>,
) -> impl IntoResponse {
    let Some(node_id) = art_node_id(&registry) else {
        return (StatusCode::SERVICE_UNAVAILABLE, "no art node connected").into_response();
    };
    state.clear_art_rotation();
    match send_general_batch(&state, &node_id, body.refresh).await {
        Ok(count) => Json(serde_json::json!({ "count": count })).into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "general art batch failed");
            (StatusCode::BAD_GATEWAY, e).into_response()
        }
    }
}

/// `POST /api/art/next` — manually advance the active rotation by one,
/// wrapping at the end. Always acts on whatever the current live rotation
/// is (unlike the auto-advance timer, this isn't scoped to a generation).
pub async fn next_art(
    axum::extract::Extension(registry): axum::extract::Extension<Arc<Mutex<Registry>>>,
    _: Authed,
    State(state): State<Arc<DashboardState>>,
) -> impl IntoResponse {
    let Some(node_id) = art_node_id(&registry) else {
        return (StatusCode::SERVICE_UNAVAILABLE, "no art node connected").into_response();
    };
    let Some(item) = state.manual_advance_art_rotation() else {
        return (StatusCode::NOT_FOUND, "no active rotation").into_response();
    };
    if !send_show_request(&state, &node_id, item.image_url) {
        return (StatusCode::SERVICE_UNAVAILABLE, "art node not reachable").into_response();
    }
    Json(state.art_rotation_status().unwrap_or_default()).into_response()
}

/// `GET /api/art/current` — the active rotation's query, position, and the
/// currently-showing item's metadata (empty object if no search has run yet
/// this coordinator session).
pub async fn get_art_current(
    _: Authed,
    State(state): State<Arc<DashboardState>>,
) -> impl IntoResponse {
    match state.art_rotation_status() {
        Some(v) => Json(v).into_response(),
        None => Json(serde_json::json!({})).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::api::test_util::*;
    use axum::Router;
    use axum::routing::{get, post};
    use shared::{ArtStatusReport, NodeCapabilities, NodeIdentity, NodeRole};
    use tokio::sync::mpsc;

    fn art_router(state: Arc<DashboardState>, registry: Arc<Mutex<Registry>>) -> Router {
        Router::new()
            .route("/api/art/show", post(show_art))
            .route("/api/art/status", get(get_art_status))
            .route("/api/art/search", post(search_art))
            .route("/api/art/next", post(next_art))
            .route("/api/art/current", get(get_art_current))
            .route("/api/art/general", post(general_art))
            .layer(axum::Extension(registry))
            .with_state(state)
    }

    fn register_art_node(registry: &Arc<Mutex<Registry>>, node_id: &str) {
        let mut reg = registry.lock().unwrap();
        reg.update_heartbeat(NodeIdentity {
            id: node_id.into(),
            hostname: node_id.into(),
            ip: "127.0.0.1".into(),
            role: NodeRole::Compute,
        });
        reg.update_capabilities(
            node_id,
            NodeCapabilities {
                features: vec![shared::Feature::Art],
                ..NodeCapabilities::default()
            },
        );
    }

    #[tokio::test]
    async fn show_art_sends_to_the_art_node() {
        let registry = make_registry();
        register_art_node(&registry, "pi-zero-1");
        let connections = empty_connections();
        let (tx, mut rx) = mpsc::channel::<MeshMessage>(4);
        connections.lock().unwrap().insert("pi-zero-1".into(), tx);
        let state = make_state(vec![], connections);

        let status = send(
            art_router(state, registry),
            "POST",
            "/api/art/show?token=",
            r#"{"image_url":"https://example.com/a.jpg"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        match rx.try_recv().unwrap() {
            MeshMessage::ArtShow(req) => {
                assert_eq!(req.image_url, "https://example.com/a.jpg");
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[tokio::test]
    async fn show_art_returns_400_for_empty_url() {
        let registry = make_registry();
        let state = make_state(vec![], empty_connections());
        let status = send(
            art_router(state, registry),
            "POST",
            "/api/art/show?token=",
            r#"{"image_url":"  "}"#,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn show_art_returns_503_when_no_art_node() {
        let registry = make_registry();
        let state = make_state(vec![], empty_connections());
        let status = send(
            art_router(state, registry),
            "POST",
            "/api/art/show?token=",
            r#"{"image_url":"https://example.com/a.jpg"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn show_art_returns_503_when_art_node_not_connected() {
        let registry = make_registry();
        // Node advertises the feature but has no live TCP connection.
        register_art_node(&registry, "pi-zero-1");
        let state = make_state(vec![], empty_connections());
        let status = send(
            art_router(state, registry),
            "POST",
            "/api/art/show?token=",
            r#"{"image_url":"https://example.com/a.jpg"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn show_art_returns_401_for_wrong_token() {
        let registry = make_registry();
        let state = make_state(vec!["secret".into()], empty_connections());
        let status = send(
            art_router(state, registry),
            "POST",
            "/api/art/show?token=wrong",
            r#"{"image_url":"https://example.com/a.jpg"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn get_art_status_returns_empty_object_when_none_reported() {
        let registry = make_registry();
        let state = make_state(vec![], empty_connections());
        let (status, body) = send_with_body(
            art_router(state, registry),
            "GET",
            "/api/art/status?token=",
            "",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.trim(), "{}");
    }

    #[tokio::test]
    async fn get_art_status_returns_last_pushed_report() {
        let registry = make_registry();
        let state = make_state(vec![], empty_connections());
        state.push_art_status(ArtStatusReport {
            node_id: "pi-zero-1".into(),
            viewer_running: true,
            current_url: Some("https://example.com/a.jpg".into()),
            error: None,
        });
        let (status, body) = send_with_body(
            art_router(state, registry),
            "GET",
            "/api/art/status?token=",
            "",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("https://example.com/a.jpg"), "body: {body}");
    }

    // ── search_art / next_art / get_art_current — validation + rotation ────
    // state only. The happy path (a real Met search, optional LLM curation)
    // isn't unit-tested — same call this session made for the Hugging Face
    // model search: no point mocking an external API's exact shape, verify
    // it for real with a live curl against the running coordinator instead.

    #[tokio::test]
    async fn search_art_returns_400_for_empty_query() {
        let registry = make_registry();
        let state = make_state(vec![], empty_connections());
        let status = send(
            art_router(state, registry),
            "POST",
            "/api/art/search?token=",
            r#"{"query":"  "}"#,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn search_art_returns_503_when_no_art_node() {
        let registry = make_registry();
        let state = make_state(vec![], empty_connections());
        let status = send(
            art_router(state, registry),
            "POST",
            "/api/art/search?token=",
            r#"{"query":"Leonardo da Vinci"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn next_art_returns_404_when_no_rotation_exists() {
        let registry = make_registry();
        register_art_node(&registry, "pi-zero-1");
        let connections = empty_connections();
        let (tx, _rx) = mpsc::channel::<MeshMessage>(4);
        connections.lock().unwrap().insert("pi-zero-1".into(), tx);
        let state = make_state(vec![], connections);

        let status = send(
            art_router(state, registry),
            "POST",
            "/api/art/next?token=",
            "",
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn next_art_returns_503_when_no_art_node() {
        let registry = make_registry();
        let state = make_state(vec![], empty_connections());
        let status = send(
            art_router(state, registry),
            "POST",
            "/api/art/next?token=",
            "",
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn next_art_advances_and_sends_show_request() {
        let registry = make_registry();
        register_art_node(&registry, "pi-zero-1");
        let connections = empty_connections();
        let (tx, mut rx) = mpsc::channel::<MeshMessage>(4);
        connections.lock().unwrap().insert("pi-zero-1".into(), tx);
        let state = make_state(vec![], connections);
        state.set_art_rotation(
            "Monet".into(),
            vec![
                ArtRotationItem {
                    image_url: "https://example.com/1.jpg".into(),
                    title: "One".into(),
                    artist: "Monet".into(),
                    date: "1900".into(),
                },
                ArtRotationItem {
                    image_url: "https://example.com/2.jpg".into(),
                    title: "Two".into(),
                    artist: "Monet".into(),
                    date: "1901".into(),
                },
            ],
        );

        let status = send(
            art_router(state, registry),
            "POST",
            "/api/art/next?token=",
            "",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        match rx.try_recv().unwrap() {
            MeshMessage::ArtShow(req) => assert_eq!(req.image_url, "https://example.com/2.jpg"),
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[tokio::test]
    async fn get_art_current_returns_empty_object_when_no_rotation() {
        let registry = make_registry();
        let state = make_state(vec![], empty_connections());
        let (status, body) = send_with_body(
            art_router(state, registry),
            "GET",
            "/api/art/current?token=",
            "",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body.trim(), "{}");
    }

    #[tokio::test]
    async fn get_art_current_returns_current_item() {
        let registry = make_registry();
        let state = make_state(vec![], empty_connections());
        state.set_art_rotation(
            "Da Vinci".into(),
            vec![ArtRotationItem {
                image_url: "https://example.com/mona.jpg".into(),
                title: "Mona Lisa".into(),
                artist: "Leonardo da Vinci".into(),
                date: "1503".into(),
            }],
        );
        let (status, body) = send_with_body(
            art_router(state, registry),
            "GET",
            "/api/art/current?token=",
            "",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("Mona Lisa"), "body: {body}");
        assert!(body.contains("Leonardo da Vinci"), "body: {body}");
    }

    // ── ArtRotationState via DashboardState's public methods ────────────────

    #[test]
    fn set_art_rotation_generation_increments_each_call() {
        let state = DashboardState::new(Arc::new(vec![]), empty_connections());
        let g1 = state.set_art_rotation("a".into(), vec![]);
        let g2 = state.set_art_rotation("b".into(), vec![]);
        assert!(g2 > g1);
    }

    #[test]
    fn advance_art_rotation_wraps_and_stops_on_stale_generation() {
        let state = DashboardState::new(Arc::new(vec![]), empty_connections());
        let generation = state.set_art_rotation(
            "q".into(),
            vec![
                ArtRotationItem {
                    image_url: "1".into(),
                    title: "".into(),
                    artist: "".into(),
                    date: "".into(),
                },
                ArtRotationItem {
                    image_url: "2".into(),
                    title: "".into(),
                    artist: "".into(),
                    date: "".into(),
                },
            ],
        );
        let second = state.advance_art_rotation_if_current(generation).unwrap();
        assert_eq!(second.image_url, "2");
        let wrapped = state.advance_art_rotation_if_current(generation).unwrap();
        assert_eq!(wrapped.image_url, "1");

        // A newer search bumps the generation — the old timer's next tick
        // should see it's stale and get None rather than fighting the new one.
        state.set_art_rotation("newer".into(), vec![]);
        assert!(state.advance_art_rotation_if_current(generation).is_none());
    }

    #[test]
    fn manual_advance_ignores_generation() {
        let state = DashboardState::new(Arc::new(vec![]), empty_connections());
        state.set_art_rotation(
            "q".into(),
            vec![
                ArtRotationItem {
                    image_url: "1".into(),
                    title: "".into(),
                    artist: "".into(),
                    date: "".into(),
                },
                ArtRotationItem {
                    image_url: "2".into(),
                    title: "".into(),
                    artist: "".into(),
                    date: "".into(),
                },
            ],
        );
        let item = state.manual_advance_art_rotation().unwrap();
        assert_eq!(item.image_url, "2");
    }

    // ── parse_curated_indices ────────────────────────────────────────────────

    #[test]
    fn parse_curated_indices_reads_plain_array() {
        let out = parse_curated_indices("[3,1,2]", 5).unwrap();
        assert_eq!(out, vec![2, 0, 1]);
    }

    #[test]
    fn parse_curated_indices_extracts_array_from_surrounding_text() {
        let out = parse_curated_indices("Sure! Here you go: [2, 4] — enjoy!", 5).unwrap();
        assert_eq!(out, vec![1, 3]);
    }

    #[test]
    fn parse_curated_indices_drops_out_of_range_and_duplicates() {
        let out = parse_curated_indices("[1, 99, 1, 0, 2]", 3).unwrap();
        // 1-based input: 1->0, 99 out of range, 1 is a dup, 0 underflows, 2->1
        assert_eq!(out, vec![0, 1]);
    }

    #[test]
    fn parse_curated_indices_none_when_nothing_survives() {
        assert!(parse_curated_indices("[99, 100]", 3).is_none());
    }

    #[test]
    fn parse_curated_indices_none_without_brackets() {
        assert!(parse_curated_indices("I don't know", 5).is_none());
    }

    // ── general_art — validation + cached-batch path only. The un-cached ────
    // path hits the real Met API live, same call as search_art's happy path:
    // not unit-tested, verified with a live curl instead.

    fn fake_general_item(url: &str) -> ArtRotationItem {
        ArtRotationItem {
            image_url: url.into(),
            title: "".into(),
            artist: "".into(),
            date: "".into(),
        }
    }

    #[tokio::test]
    async fn general_art_returns_503_when_no_art_node() {
        let registry = make_registry();
        let state = make_state(vec![], empty_connections());
        let status = send(
            art_router(state, registry),
            "POST",
            "/api/art/general?token=",
            "{}",
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn general_art_sends_cached_batch_to_node() {
        let registry = make_registry();
        register_art_node(&registry, "pi-zero-1");
        let connections = empty_connections();
        let (tx, mut rx) = mpsc::channel::<MeshMessage>(4);
        connections.lock().unwrap().insert("pi-zero-1".into(), tx);
        let state = make_state(vec![], connections);
        state.set_general_art_batch(vec![
            fake_general_item("https://example.com/1.jpg"),
            fake_general_item("https://example.com/2.jpg"),
        ]);

        let status = send(
            art_router(state, registry),
            "POST",
            "/api/art/general?token=",
            "{}",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        match rx.try_recv().unwrap() {
            MeshMessage::ArtBatch(req) => {
                assert_eq!(
                    req.image_urls,
                    vec!["https://example.com/1.jpg", "https://example.com/2.jpg"]
                );
                assert_eq!(req.interval_secs, GENERAL_INTERVAL_SECS);
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[tokio::test]
    async fn general_art_clears_any_active_specific_rotation() {
        let registry = make_registry();
        register_art_node(&registry, "pi-zero-1");
        let connections = empty_connections();
        let (tx, _rx) = mpsc::channel::<MeshMessage>(4);
        connections.lock().unwrap().insert("pi-zero-1".into(), tx);
        let state = make_state(vec![], connections);
        state.set_art_rotation(
            "Monet".into(),
            vec![fake_general_item("https://example.com/monet.jpg")],
        );
        state.set_general_art_batch(vec![fake_general_item("https://example.com/1.jpg")]);

        assert!(state.art_rotation_current_item().is_some());
        let status = send(
            art_router(state.clone(), registry),
            "POST",
            "/api/art/general?token=",
            "{}",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(state.art_rotation_current_item().is_none());
    }
}
