//! Songs tab HTTP API — chord sheets from Ultimate Guitar, a saved library,
//! and a "play it on Spotify" hand-off. See plans/chords-songbook.md.
//! Coordinator-only, like the Hunts tab: UG has no API and the browser can't
//! read its pages across origins, so the coordinator fetches them.

use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use chords::{Tab, UgClient, UgError};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::http::auth::Authed;
use crate::http::state::DashboardState;
use crate::registry::{ChordSongPrefs, Registry};

fn ug_client() -> &'static UgClient {
    static CLIENT: std::sync::OnceLock<UgClient> = std::sync::OnceLock::new();
    CLIENT.get_or_init(UgClient::new)
}

fn ug_error(e: UgError) -> axum::response::Response {
    let status = match e {
        UgError::NotFound => StatusCode::NOT_FOUND,
        UgError::Blocked | UgError::Unreachable(_) => StatusCode::BAD_GATEWAY,
        UgError::Other(_) => StatusCode::BAD_GATEWAY,
    };
    (status, e.to_string()).into_response()
}

/// The versions Jon asked to have in the library from the start. Only their
/// addresses live here: the sheets carry the songs' lyrics, which aren't ours
/// to commit, so the coordinator fetches them from UG on its own network.
const STARTER_SONGS: &[&str] = &[
    "https://tabs.ultimate-guitar.com/tab/bob-marley/no-woman-no-cry-chords-45479",
    "https://tabs.ultimate-guitar.com/tab/bastille/pompeii-chords-1227805",
    "https://tabs.ultimate-guitar.com/tab/jimi-hendrix/angel-chords-70247",
    "https://tabs.ultimate-guitar.com/tab/gorillaz/clint-eastwood-chords-1742648",
];
/// `dashboard_preferences` namespace remembering which starters were added,
/// so one deleted from the library stays deleted.
const STARTER_PREFS_USER: &str = "__chords__";

/// Add the starter songs in the background, once each. A song UG wouldn't
/// give us (blocked, offline) is tried again at the next start.
pub fn seed_starter_songs(registry: Arc<Mutex<Registry>>) {
    tokio::spawn(async move { seed_with(ug_client(), STARTER_SONGS, &registry).await });
}

async fn seed_with(client: &UgClient, urls: &[&str], registry: &Arc<Mutex<Registry>>) {
    for (i, url) in urls.iter().enumerate() {
        let key = format!("starter:{url}");
        if registry
            .lock()
            .unwrap()
            .get_preference(STARTER_PREFS_USER, &key)
            .is_some()
        {
            continue;
        }
        if i > 0 {
            // One page at a time, unhurried — this is UG's site, not an API.
            tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        }
        match client.fetch_tab(url).await {
            Ok(tab) => {
                let reg = registry.lock().unwrap();
                if reg.save_chord_song(&tab).is_some() {
                    reg.set_preference(STARTER_PREFS_USER, &key, "1");
                    tracing::info!(song = %tab.song_name, "added starter song");
                }
            }
            // Gone from UG: nothing to retry.
            Err(UgError::NotFound) => {
                tracing::warn!(url, "starter song is no longer on Ultimate Guitar");
                registry
                    .lock()
                    .unwrap()
                    .set_preference(STARTER_PREFS_USER, &key, "gone");
            }
            Err(e) => {
                tracing::warn!(url, error = %e, "starter song not added; will retry at next start")
            }
        }
    }
}

/// Which UG versions are already saved, as UG tab id → saved song id — what
/// lets the dashboard star a version in the switcher.
fn saved_ids(reg: &Registry, tab_ids: impl Iterator<Item = i64>) -> HashMap<i64, String> {
    tab_ids
        .filter_map(|t| reg.chord_song_for_tab(t).map(|id| (t, id)))
        .collect()
}

#[derive(Deserialize)]
pub struct SearchQuery {
    q: String,
}

/// Search UG; songs come back grouped with every version of each.
pub async fn search(
    _: Authed,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    Query(q): Query<SearchQuery>,
) -> impl IntoResponse {
    let query = q.q.trim();
    if query.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            "type a song or artist to search for",
        )
            .into_response();
    }
    let hits = match ug_client().search(query).await {
        Ok(h) => h,
        Err(e) => return ug_error(e),
    };
    let groups = chords::group_versions(hits);
    let saved = {
        let reg = registry.lock().unwrap();
        saved_ids(
            &reg,
            groups
                .iter()
                .flat_map(|g| g.versions.iter().map(|v| v.tab_id)),
        )
    };
    Json(serde_json::json!({ "groups": groups, "saved": saved })).into_response()
}

#[derive(Deserialize)]
pub struct FetchQuery {
    url: String,
}

/// One UG version, live and unsaved.
pub async fn fetch(
    _: Authed,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    Query(q): Query<FetchQuery>,
) -> impl IntoResponse {
    if !chords::is_ug_url(&q.url) {
        return (
            StatusCode::BAD_REQUEST,
            "only Ultimate Guitar tab links (https://tabs.ultimate-guitar.com/tab/…) can be fetched",
        )
            .into_response();
    }
    let tab = match ug_client().fetch_tab(&q.url).await {
        Ok(t) => t,
        Err(e) => return ug_error(e),
    };
    let saved = {
        let reg = registry.lock().unwrap();
        saved_ids(
            &reg,
            tab.tab_id
                .into_iter()
                .chain(tab.versions.iter().map(|v| v.tab_id)),
        )
    };
    Json(serde_json::json!({ "tab": tab, "saved": saved })).into_response()
}

pub async fn list_songs(
    _: Authed,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
) -> impl IntoResponse {
    Json(registry.lock().unwrap().list_chord_songs())
}

#[derive(Deserialize)]
pub struct PasteBody {
    #[serde(default)]
    artist: String,
    #[serde(default)]
    title: String,
    text: String,
}

/// A save is either a sheet the dashboard fetched, or pasted text.
#[derive(Deserialize)]
#[serde(untagged)]
pub enum SaveBody {
    Paste { paste: PasteBody },
    Tab(Box<Tab>),
}

pub async fn save_song(
    _: Authed,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    Json(body): Json<SaveBody>,
) -> impl IntoResponse {
    let mut tab = match body {
        SaveBody::Paste { paste } => {
            if paste.title.trim().is_empty() || paste.text.trim().is_empty() {
                return (
                    StatusCode::BAD_REQUEST,
                    "a pasted song needs a title and its chords",
                )
                    .into_response();
            }
            chords::parse_pasted(&paste.artist, &paste.title, &paste.text)
        }
        SaveBody::Tab(tab) => *tab,
    };
    if tab.song_name.trim().is_empty() || tab.content.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            "nothing to save — the song has no title or chords",
        )
            .into_response();
    }
    if tab.artist_name.trim().is_empty() {
        tab.artist_name = "Unknown artist".into();
    }
    // `refresh` fetches `url` later, so only ever store a UG address there.
    if tab.url.as_deref().is_some_and(|u| !chords::is_ug_url(u)) {
        tab.url = None;
    }
    match registry.lock().unwrap().save_chord_song(&tab) {
        Some(song) => (StatusCode::CREATED, Json(song)).into_response(),
        None => (StatusCode::INTERNAL_SERVER_ERROR, "could not save the song").into_response(),
    }
}

/// A saved song with its sheet. Counts as playing it, for "recently played".
pub async fn get_song(
    _: Authed,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let reg = registry.lock().unwrap();
    match reg.get_chord_song(&id) {
        Some(song) => {
            reg.touch_chord_song_played(&id);
            Json(song).into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

pub async fn update_song(
    _: Authed,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    Path(id): Path<String>,
    Json(prefs): Json<ChordSongPrefs>,
) -> impl IntoResponse {
    if let Some(u) = prefs.youtube_url.as_deref()
        && !u.trim().is_empty()
        && youtube_id(u).is_none()
    {
        return (
            StatusCode::BAD_REQUEST,
            "that doesn't look like a YouTube video link",
        )
            .into_response();
    }
    match registry
        .lock()
        .unwrap()
        .update_chord_song_prefs(&id, &prefs)
    {
        Some(song) => Json(song).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Re-fetch a saved song's sheet from UG, keeping its settings.
pub async fn refresh_song(
    _: Authed,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let url = {
        let reg = registry.lock().unwrap();
        match reg.get_chord_song(&id) {
            Some(s) => s.summary.source_url,
            None => return StatusCode::NOT_FOUND.into_response(),
        }
    };
    let Some(url) = url.filter(|u| chords::is_ug_url(u)) else {
        return (
            StatusCode::BAD_REQUEST,
            "this song was pasted, so there's nothing to refresh from",
        )
            .into_response();
    };
    let tab = match ug_client().fetch_tab(&url).await {
        Ok(t) => t,
        Err(e) => return ug_error(e),
    };
    match registry.lock().unwrap().refresh_chord_song(&id, &tab) {
        Some(song) => Json(song).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

pub async fn delete_song(
    _: Authed,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if registry.lock().unwrap().delete_chord_song(&id) {
        StatusCode::NO_CONTENT.into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

#[derive(Deserialize)]
pub struct SpotifyBody {
    /// "play" (needs `query`) or "pause".
    action: String,
    #[serde(default)]
    query: String,
}

/// Hand the song to the house Spotify player — the same music node the Chat
/// tab's "play …" uses, so it needs a music node connected.
pub async fn spotify(
    _: Authed,
    State(state): State<Arc<DashboardState>>,
    Extension(registry): Extension<Arc<Mutex<Registry>>>,
    Json(body): Json<SpotifyBody>,
) -> impl IntoResponse {
    let args = match body.action.as_str() {
        "play" if !body.query.trim().is_empty() => {
            serde_json::json!({ "action": "play", "query": body.query.trim(), "entity_type": "track" })
        }
        "pause" => serde_json::json!({ "action": "pause" }),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                "action must be play (with a query) or pause",
            )
                .into_response();
        }
    };
    let request_id = format!("chords-spotify-{}", super::gen_request_id());
    let message = crate::intent::dispatch_music_command(
        &request_id,
        &args,
        &registry,
        &state.connections,
        &state.pending_intents,
    )
    .await;
    Json(serde_json::json!({ "message": message })).into_response()
}

/// The video id in a YouTube link — watch, youtu.be, shorts, embed and music
/// forms. The dashboard embeds by id, never by the pasted URL.
pub fn youtube_id(url: &str) -> Option<String> {
    let url = url.trim();
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let (host, path) = rest.split_once('/')?;
    let host = host.to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    let host = host.strip_prefix("m.").unwrap_or(host);
    let id = match host {
        "youtu.be" => path.split(['?', '&', '#', '/']).next(),
        "youtube.com" | "music.youtube.com" | "youtube-nocookie.com" => {
            if let Some(q) = path.strip_prefix("watch?") {
                q.split('&').find_map(|kv| kv.strip_prefix("v="))
            } else {
                ["shorts/", "embed/", "live/"]
                    .iter()
                    .find_map(|p| path.strip_prefix(p))
                    .and_then(|p| p.split(['?', '&', '#', '/']).next())
            }
        }
        _ => None,
    }?;
    let id = id.split('#').next().unwrap_or(id);
    (id.len() == 11
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
    .then(|| id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::api::test_util::*;
    use axum::Router;
    use axum::routing::{get, post};

    fn chords_router(state: Arc<DashboardState>, registry: Arc<Mutex<Registry>>) -> Router {
        Router::new()
            .route("/api/chords/search", get(search))
            .route("/api/chords/fetch", get(fetch))
            .route("/api/chords/songs", get(list_songs).post(save_song))
            .route(
                "/api/chords/songs/{id}",
                get(get_song).patch(update_song).delete(delete_song),
            )
            .route("/api/chords/songs/{id}/refresh", post(refresh_song))
            .route("/api/chords/spotify", post(spotify))
            .layer(axum::Extension(registry))
            .with_state(state)
    }

    fn router() -> (Router, Arc<Mutex<Registry>>) {
        let registry = make_registry();
        let state = make_state(vec![], empty_connections());
        (chords_router(state, registry.clone()), registry)
    }

    #[tokio::test]
    async fn requires_a_token() {
        let registry = make_registry();
        let state = make_state(vec!["secret".into()], empty_connections());
        let r = chords_router(state, registry);
        assert_eq!(
            send(r, "GET", "/api/chords/songs", "").await,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn fetch_refuses_anything_but_ug() {
        let (r, _) = router();
        let status = send(
            r,
            "GET",
            "/api/chords/fetch?url=http%3A%2F%2F169.254.169.254%2Ftab%2Fx",
            "",
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn an_empty_search_is_refused_without_calling_ug() {
        let (r, _) = router();
        assert_eq!(
            send(r, "GET", "/api/chords/search?q=%20", "").await,
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn paste_save_get_update_delete() {
        let (r, registry) = router();
        let (status, body) = send_with_body(
            r.clone(),
            "POST",
            "/api/chords/songs",
            r#"{"paste":{"artist":"Bob Marley","title":"No Woman No Cry","text":"C  G/B  Am  F\nA made-up line for the test"}}"#,
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let song: serde_json::Value = serde_json::from_str(&body).unwrap();
        let id = song["id"].as_str().unwrap().to_string();
        assert!(song["content"].as_str().unwrap().starts_with("[ch]C[/ch]"));

        let (status, body) = send_with_body(r.clone(), "GET", "/api/chords/songs", "").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("No Woman No Cry"));
        assert!(!body.contains("content"), "the list leaves the sheets out");

        let (status, body) =
            send_with_body(r.clone(), "GET", &format!("/api/chords/songs/{id}"), "").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("A made-up line for the test"));
        assert!(
            registry
                .lock()
                .unwrap()
                .get_chord_song(&id)
                .unwrap()
                .summary
                .last_played_ms
                .is_some()
        );

        let (status, body) = send_with_body(
            r.clone(),
            "PATCH",
            &format!("/api/chords/songs/{id}"),
            r#"{"transpose":2,"scroll_speed":25,"youtube_url":"https://youtu.be/dQw4w9WgXcQ"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let song: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(song["transpose"], 2);
        assert_eq!(song["scroll_speed"], 25.0);

        let status = send(
            r.clone(),
            "PATCH",
            &format!("/api/chords/songs/{id}"),
            r#"{"youtube_url":"https://evil.example/watch?v=dQw4w9WgXcQ"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // A pasted song has no UG page to refresh from.
        let status = send(
            r.clone(),
            "POST",
            &format!("/api/chords/songs/{id}/refresh"),
            "",
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let status = send(r.clone(), "DELETE", &format!("/api/chords/songs/{id}"), "").await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let status = send(r, "GET", &format!("/api/chords/songs/{id}"), "").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_fetched_tab_saves_and_a_foreign_url_is_dropped() {
        let (r, registry) = router();
        let body = serde_json::json!({
            "tab_id": 1227805, "song_name": "Pompeii", "artist_name": "Bastille",
            "kind": "Chords", "capo": 2, "content": "[ch]Am[/ch] made-up words",
            "url": "https://evil.example/tab/x"
        });
        let (status, _) = send_with_body(r, "POST", "/api/chords/songs", &body.to_string()).await;
        assert_eq!(status, StatusCode::CREATED);
        let reg = registry.lock().unwrap();
        let id = reg.chord_song_for_tab(1227805).unwrap();
        assert_eq!(reg.get_chord_song(&id).unwrap().summary.source_url, None);
    }

    #[tokio::test]
    async fn a_save_with_no_chords_is_refused() {
        let (r, _) = router();
        let status = send(
            r,
            "POST",
            "/api/chords/songs",
            r#"{"paste":{"artist":"A","title":"S","text":"   "}}"#,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn spotify_without_a_music_node_says_so() {
        let (r, _) = router();
        let (status, body) = send_with_body(
            r.clone(),
            "POST",
            "/api/chords/spotify",
            r#"{"action":"play","query":"Pompeii Bastille"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("no music node connected"));
        let status = send(r, "POST", "/api/chords/spotify", r#"{"action":"play"}"#).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    /// A local server answering every request with `status` and `body`.
    async fn serve(status: &'static str, body: &'static str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = vec![0u8; 8192];
                let _ = sock.read(&mut buf).await;
                let resp = format!(
                    "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            }
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn starter_songs_are_added_once_and_stay_deleted() {
        let base = serve(
            "200 OK",
            include_str!("../../../../chords/tests/fixtures/tab.html"),
        )
        .await;
        let client = UgClient::with_base_url(&base);
        let url = format!("{base}/tab/bastille/pompeii-chords-1227805");
        let registry = make_registry();

        seed_with(&client, &[&url], &registry).await;
        let songs = registry.lock().unwrap().list_chord_songs();
        assert_eq!(songs.len(), 1);
        assert_eq!(songs[0].title, "Pompeii");

        // Deleted by hand, then the coordinator restarts: not re-added.
        registry.lock().unwrap().delete_chord_song(&songs[0].id);
        seed_with(&client, &[&url], &registry).await;
        assert!(registry.lock().unwrap().list_chord_songs().is_empty());
    }

    #[tokio::test]
    async fn a_blocked_starter_song_is_tried_again_next_time() {
        let base = serve("403 Forbidden", "").await;
        let client = UgClient::with_base_url(&base);
        let url = format!("{base}/tab/x");
        let registry = make_registry();
        seed_with(&client, &[&url], &registry).await;
        let reg = registry.lock().unwrap();
        assert!(reg.list_chord_songs().is_empty());
        assert!(
            reg.get_preference(STARTER_PREFS_USER, &format!("starter:{url}"))
                .is_none()
        );
    }

    #[test]
    fn starter_songs_are_all_ug_links() {
        assert_eq!(STARTER_SONGS.len(), 4);
        assert!(STARTER_SONGS.iter().all(|u| chords::is_ug_url(u)));
    }

    #[test]
    fn youtube_ids_come_out_of_every_link_form() {
        for url in [
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://youtube.com/watch?feature=share&v=dQw4w9WgXcQ&t=10",
            "https://m.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://youtu.be/dQw4w9WgXcQ?si=abc",
            "https://www.youtube.com/shorts/dQw4w9WgXcQ",
            "https://www.youtube.com/embed/dQw4w9WgXcQ",
            "https://music.youtube.com/watch?v=dQw4w9WgXcQ&list=x",
        ] {
            assert_eq!(youtube_id(url).as_deref(), Some("dQw4w9WgXcQ"), "{url}");
        }
        for url in [
            "https://evil.example/watch?v=dQw4w9WgXcQ",
            "https://youtube.com/watch?v=short",
            "https://youtube.com/watch?v=dQw4w9WgXcQ\"><script>",
            "youtube.com/watch?v=dQw4w9WgXcQ",
        ] {
            assert_eq!(youtube_id(url), None, "{url}");
        }
    }
}
