//! Minimal Spotify Web API client — token refresh plus the handful of player
//! endpoints the capability needs. Hand-rolled reqwest in the house style
//! (see capability-reaper / coordinator soundbar) rather than a full SDK
//! crate: the needed surface is tiny and every error must come back as a
//! human-readable sentence.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::Mutex;
use tracing::{info, warn};

const TOKEN_URL: &str = "https://accounts.spotify.com/api/token";
const API_BASE: &str = "https://api.spotify.com/v1";

/// Errors from the Web API, pre-phrased for humans except the one case the
/// caller must branch on: a player command that needs an explicit device_id
/// retry (no active device, or the id we sent has gone stale).
pub enum ApiError {
    DeviceUnavailable,
    Other(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // Callers normally retry this; if it still surfaces, say what it means.
            ApiError::DeviceUnavailable => write!(f, "Spotify can't find an active player"),
            ApiError::Other(msg) => write!(f, "{msg}"),
        }
    }
}

/// A player command, paired with the HTTP shape Spotify expects for it.
pub enum PlayerCall {
    /// `Some(body)` starts specific content; `None` resumes where it left off.
    Play(Option<Value>),
    Pause,
    Next,
    Previous,
    SeekMs(u64),
    VolumePercent(u8),
    Shuffle(bool),
}

struct CachedToken {
    access_token: String,
    expires_at: Instant,
}

pub struct SpotifyClient {
    http: reqwest::Client,
    token: Mutex<Option<CachedToken>>,
}

impl Default for SpotifyClient {
    fn default() -> Self {
        Self::new()
    }
}

impl SpotifyClient {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::new(),
            token: Mutex::new(None),
        }
    }

    /// Cached access token, refreshed via the long-lived refresh token when
    /// within 60 s of expiry. Credentials are read per call, not cached at
    /// construction, so a `spotify-push-creds` drop-in restart always wins.
    async fn access_token(&self) -> Result<String, ApiError> {
        let mut guard = self.token.lock().await;
        if let Some(t) = guard.as_ref()
            && t.expires_at > Instant::now()
        {
            return Ok(t.access_token.clone());
        }

        let (client_id, client_secret, refresh_token) = credentials()?;
        let resp = self
            .http
            .post(TOKEN_URL)
            .basic_auth(&client_id, Some(&client_secret))
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", &refresh_token),
            ])
            .send()
            .await
            .map_err(|e| ApiError::Other(format!("could not reach Spotify: {e}")))?;

        let status = resp.status();
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            let detail = body["error_description"]
                .as_str()
                .unwrap_or(status.as_str());
            return Err(ApiError::Other(format!(
                "Spotify sign-in failed ({detail}) — try re-running 'just spotify-auth'"
            )));
        }

        let Some(access_token) = body["access_token"].as_str() else {
            return Err(ApiError::Other(
                "Spotify sign-in returned no access token".into(),
            ));
        };
        // Spotify occasionally rotates the refresh token. Persist it, or the
        // control plane silently dies at the next refresh weeks later.
        if let Some(rotated) = body["refresh_token"].as_str()
            && rotated != refresh_token
        {
            persist_refresh_token(rotated);
        }
        let expires_in = body["expires_in"].as_u64().unwrap_or(3600);
        *guard = Some(CachedToken {
            access_token: access_token.to_string(),
            expires_at: Instant::now() + Duration::from_secs(expires_in.saturating_sub(60)),
        });
        Ok(access_token.to_string())
    }

    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<&Value>,
    ) -> Result<Value, ApiError> {
        let token = self.access_token().await?;
        let mut req = self
            .http
            .request(method, format!("{API_BASE}{path}"))
            .bearer_auth(token);
        if !query.is_empty() {
            req = req.query(query);
        }
        if let Some(b) = body {
            req = req.json(b);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| ApiError::Other(format!("could not reach Spotify: {e}")))?;

        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if status.is_success() {
            return Ok(value);
        }
        // Player-endpoint 404s mean "no active device" or "device not found"
        // (a stale id after librespot restarted) — both fixed the same way,
        // by re-resolving the device and retrying with an explicit id.
        if status == reqwest::StatusCode::NOT_FOUND && path.starts_with("/me/player") {
            return Err(ApiError::DeviceUnavailable);
        }
        let detail = value["error"]["message"].as_str().unwrap_or("").to_string();
        Err(ApiError::Other(match status.as_u16() {
            401 => "Spotify authorisation failed — try re-running 'just spotify-auth'".into(),
            403 => "Spotify refused the command — playback control needs Spotify Premium".into(),
            429 => "Spotify is rate-limiting requests — try again in a moment".into(),
            _ if !detail.is_empty() => format!("Spotify says: {detail}"),
            _ => format!("Spotify returned an unexpected error ({status})"),
        }))
    }

    /// Top search hits for `query`, e.g. `entity_type` "track" → `tracks.items`.
    pub async fn search(&self, query: &str, entity_type: &str) -> Result<Value, ApiError> {
        self.call(
            reqwest::Method::GET,
            "/search",
            &[
                ("q", query.to_string()),
                ("type", entity_type.to_string()),
                ("limit", "1".to_string()),
            ],
            None,
        )
        .await
    }

    /// The account's available Spotify Connect devices.
    pub async fn devices(&self) -> Result<Value, ApiError> {
        self.call(reqwest::Method::GET, "/me/player/devices", &[], None)
            .await
    }

    /// Current playback state, `None` when nothing is playing anywhere
    /// (Spotify answers 204 with an empty body).
    pub async fn player_state(&self) -> Result<Option<Value>, ApiError> {
        let value = self
            .call(reqwest::Method::GET, "/me/player", &[], None)
            .await?;
        Ok(if value.is_null() { None } else { Some(value) })
    }

    /// Issue one player command, optionally pinned to a device id.
    pub async fn player(&self, call: &PlayerCall, device_id: Option<&str>) -> Result<(), ApiError> {
        use reqwest::Method;
        let mut query: Vec<(&str, String)> = Vec::new();
        if let Some(id) = device_id {
            query.push(("device_id", id.to_string()));
        }
        let (method, path, body) = match call {
            PlayerCall::Play(body) => (Method::PUT, "/me/player/play", body.as_ref()),
            PlayerCall::Pause => (Method::PUT, "/me/player/pause", None),
            PlayerCall::Next => (Method::POST, "/me/player/next", None),
            PlayerCall::Previous => (Method::POST, "/me/player/previous", None),
            PlayerCall::SeekMs(ms) => {
                query.push(("position_ms", ms.to_string()));
                (Method::PUT, "/me/player/seek", None)
            }
            PlayerCall::VolumePercent(p) => {
                query.push(("volume_percent", p.to_string()));
                (Method::PUT, "/me/player/volume", None)
            }
            PlayerCall::Shuffle(on) => {
                query.push(("state", on.to_string()));
                (Method::PUT, "/me/player/shuffle", None)
            }
        };
        self.call(method, path, &query, body).await.map(|_| ())
    }
}

/// Where a rotated refresh token is persisted; read in preference to the
/// env var, which goes stale the moment Spotify rotates.
fn refresh_token_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_default()
        .join(".ai-mesh")
        .join("spotify_refresh_token")
}

fn credentials() -> Result<(String, String, String), ApiError> {
    let client_id = std::env::var("SPOTIFY_CLIENT_ID").unwrap_or_default();
    let client_secret = std::env::var("SPOTIFY_CLIENT_SECRET").unwrap_or_default();
    let refresh_token = std::fs::read_to_string(refresh_token_path())
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .or_else(|| std::env::var("SPOTIFY_REFRESH_TOKEN").ok())
        .unwrap_or_default();
    if client_id.is_empty() || client_secret.is_empty() || refresh_token.is_empty() {
        return Err(ApiError::Other(
            "Spotify credentials are not configured on this node — run 'just spotify-auth' \
             then 'just spotify-push-creds <node>'"
                .into(),
        ));
    }
    Ok((client_id, client_secret, refresh_token))
}

fn persist_refresh_token(token: &str) {
    let path = refresh_token_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::write(&path, token) {
        Ok(()) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
            }
            info!("music: Spotify rotated the refresh token — persisted to {path:?}");
        }
        Err(e) => warn!(
            "music: Spotify rotated the refresh token but persisting to {path:?} failed: {e} — \
             the control plane will break at the next refresh; re-run 'just spotify-auth'"
        ),
    }
}
