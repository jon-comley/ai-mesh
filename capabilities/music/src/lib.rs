//! Music capability — Spotify playback and control (plans/spotify-music.md).
//!
//! Two planes:
//! - control plane (this phase): Spotify Web API (search, play, pause, seek,
//!   …) driven by `MusicCommand` messages from the coordinator's
//!   `music_control` tool;
//! - playback engine (Phase 4): a supervised `librespot` child process (pipe
//!   backend) feeding PCM into the node's PipeWire sink.
//!
//! Every result message is a finished human-readable sentence: the
//! coordinator relays it verbatim to chat and voice TTS with no second LLM
//! turn to rewrite it.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use capability_core::Capability;
use serde_json::Value;
use shared::{MeshMessage, MusicCommandRequest, MusicCommandResult};
use tokio::sync::mpsc::Sender;
use tracing::warn;

mod web_api;

use web_api::{ApiError, PlayerCall, SpotifyClient};

pub struct MusicCapability {
    #[allow(dead_code)] // device naming/logs once the Phase 4 supervisor lands
    node_id: String,
    coordinator_tx: Arc<Mutex<Option<Sender<MeshMessage>>>>,
    api: SpotifyClient,
    /// The librespot device's Spotify Connect id, resolved by name and cached;
    /// re-resolved when a command reports the device gone (librespot restarts
    /// get a fresh id).
    device_id: tokio::sync::Mutex<Option<String>>,
}

/// The Spotify Connect device name librespot registers under (Phase 4 passes
/// the same value to `librespot --name`); commands target it by this name.
fn device_name() -> String {
    std::env::var("SPOTIFY_DEVICE_NAME").unwrap_or_else(|_| "AI Mesh".into())
}

impl MusicCapability {
    pub fn new(node_id: impl Into<String>) -> Self {
        Self {
            node_id: node_id.into(),
            coordinator_tx: Arc::new(Mutex::new(None)),
            api: SpotifyClient::new(),
            device_id: tokio::sync::Mutex::new(None),
        }
    }

    fn tx(&self) -> Option<Sender<MeshMessage>> {
        self.coordinator_tx.lock().unwrap().clone()
    }

    async fn execute(&self, cmd: &MusicCommandRequest) -> MusicCommandResult {
        let outcome = self.run(&cmd.action, &cmd.params).await;
        let (ok, message) = match outcome {
            Ok(m) => (true, m),
            Err(m) => (false, m),
        };
        MusicCommandResult {
            request_id: cmd.request_id.clone(),
            ok,
            message,
        }
    }

    async fn run(&self, action: &str, params: &Value) -> Result<String, String> {
        match action {
            "play" => self.cmd_play(params).await,
            "resume" => {
                self.player_command(PlayerCall::Play(None)).await?;
                Ok("Resumed".into())
            }
            "pause" => {
                self.player_command(PlayerCall::Pause).await?;
                Ok("Paused".into())
            }
            "next" => {
                self.player_command(PlayerCall::Next).await?;
                Ok("Skipped to the next track".into())
            }
            "previous" => {
                self.player_command(PlayerCall::Previous).await?;
                Ok("Went back to the previous track".into())
            }
            "seek" => self.cmd_seek(params).await,
            "volume" => {
                let percent = int_param(params, "percent")
                    .ok_or("volume needs a percent between 0 and 100")?
                    .clamp(0, 100) as u8;
                self.player_command(PlayerCall::VolumePercent(percent))
                    .await?;
                Ok(format!("Volume set to {percent}%"))
            }
            "shuffle" => {
                let on = params["on"].as_bool().unwrap_or(true);
                self.player_command(PlayerCall::Shuffle(on)).await?;
                Ok(if on { "Shuffle on" } else { "Shuffle off" }.into())
            }
            "status" => self.cmd_status().await,
            other => Err(format!(
                "I don't know the music action '{other}' — try play, pause, next, or status"
            )),
        }
    }

    async fn cmd_play(&self, params: &Value) -> Result<String, String> {
        let query = params["query"].as_str().map(str::trim).unwrap_or("");
        if query.is_empty() {
            // "play"/"play music" with nothing named — resume where it left off.
            self.player_command(PlayerCall::Play(None)).await?;
            return Ok("Resumed".into());
        }
        let entity = params["entity_type"]
            .as_str()
            .filter(|e| matches!(*e, "album" | "artist" | "playlist"))
            .unwrap_or("track");
        let results = self
            .api
            .search(query, entity)
            .await
            .map_err(|e| e.to_string())?;
        let Some(hit) = search_hit(&results, entity) else {
            return Err(format!("I couldn't find '{query}' on Spotify"));
        };
        let Some(uri) = hit["uri"].as_str() else {
            return Err("Spotify returned a result without a URI".into());
        };
        // A track plays as a one-shot uri list; albums/artists/playlists play
        // as a context so Spotify continues through their contents.
        let body = if entity == "track" {
            serde_json::json!({ "uris": [uri] })
        } else {
            serde_json::json!({ "context_uri": uri })
        };
        let summary = hit_summary(hit, entity);
        // A fresh play always targets the librespot device explicitly —
        // nothing may be "active" yet as far as Spotify is concerned.
        let device = self.resolve_device(false).await?;
        match self
            .api
            .player(&PlayerCall::Play(Some(body.clone())), Some(&device))
            .await
        {
            Ok(()) => Ok(summary),
            Err(ApiError::DeviceUnavailable) => {
                // Cached id gone stale (librespot restarted) — re-resolve once.
                let device = self.resolve_device(true).await?;
                self.api
                    .player(&PlayerCall::Play(Some(body)), Some(&device))
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(summary)
            }
            Err(e) => Err(e.to_string()),
        }
    }

    async fn cmd_seek(&self, params: &Value) -> Result<String, String> {
        let seconds =
            int_param(params, "seconds").ok_or("seek needs a number of seconds, e.g. -30")?;
        let Some(state) = self.api.player_state().await.map_err(|e| e.to_string())? else {
            return Err("Nothing is playing".into());
        };
        let progress = state["progress_ms"].as_i64().unwrap_or(0);
        let duration = state["item"]["duration_ms"].as_i64().unwrap_or(i64::MAX);
        let target = (progress + seconds * 1000).clamp(0, duration) as u64;
        self.player_command(PlayerCall::SeekMs(target)).await?;
        Ok(if seconds < 0 {
            format!("Went back {} seconds", -seconds)
        } else {
            format!("Skipped ahead {seconds} seconds")
        })
    }

    async fn cmd_status(&self) -> Result<String, String> {
        let Some(state) = self.api.player_state().await.map_err(|e| e.to_string())? else {
            return Ok("Nothing is playing".into());
        };
        Ok(status_sentence(&state))
    }

    /// Run a player command against whatever is active; if Spotify says no
    /// active device (or our id went stale), pin it to the librespot device
    /// and retry once.
    async fn player_command(&self, call: PlayerCall) -> Result<(), String> {
        match self.api.player(&call, None).await {
            Ok(()) => Ok(()),
            Err(ApiError::DeviceUnavailable) => {
                let device = self.resolve_device(true).await?;
                self.api
                    .player(&call, Some(&device))
                    .await
                    .map_err(|e| e.to_string())
            }
            Err(e) => Err(e.to_string()),
        }
    }

    /// The librespot device's Connect id, from cache unless `force`.
    async fn resolve_device(&self, force: bool) -> Result<String, String> {
        let mut cache = self.device_id.lock().await;
        if !force && let Some(id) = cache.as_ref() {
            return Ok(id.clone());
        }
        let name = device_name();
        let devices = self.api.devices().await.map_err(|e| e.to_string())?;
        let found = devices["devices"]
            .as_array()
            .and_then(|list| list.iter().find(|d| d["name"].as_str() == Some(&name)))
            .and_then(|d| d["id"].as_str())
            .map(str::to_string);
        match found {
            Some(id) => {
                *cache = Some(id.clone());
                Ok(id)
            }
            None => {
                *cache = None;
                Err(format!(
                    "the Spotify player '{name}' isn't registered yet — is librespot running on \
                     this node?"
                ))
            }
        }
    }
}

/// Accept integers that arrive as JSON numbers or numeric strings — small
/// models emit either (precedent: reaper_add_track's rec_input handling).
fn int_param(params: &Value, key: &str) -> Option<i64> {
    params[key]
        .as_i64()
        .or_else(|| params[key].as_str().and_then(|s| s.trim().parse().ok()))
}

/// First search hit for the entity type, skipping the null placeholder
/// entries Spotify sometimes returns in playlist results.
fn search_hit<'v>(results: &'v Value, entity: &str) -> Option<&'v Value> {
    let key = match entity {
        "album" => "albums",
        "artist" => "artists",
        "playlist" => "playlists",
        _ => "tracks",
    };
    results[key]["items"]
        .as_array()?
        .iter()
        .find(|item| !item.is_null())
}

/// Finished sentence describing what a successful play started.
fn hit_summary(hit: &Value, entity: &str) -> String {
    let name = hit["name"].as_str().unwrap_or("unknown");
    let by = hit["artists"][0]["name"]
        .as_str()
        .map(|a| format!(" by {a}"))
        .unwrap_or_default();
    match entity {
        "album" => format!("Playing the album '{name}'{by}"),
        "artist" => format!("Playing music by {name}"),
        "playlist" => format!("Playing the playlist '{name}'"),
        _ => format!("Now playing '{name}'{by}"),
    }
}

/// Finished sentence for "what's playing?" from a `/me/player` state.
fn status_sentence(state: &Value) -> String {
    let item = &state["item"];
    let Some(name) = item["name"].as_str() else {
        return "Nothing is playing".into();
    };
    let verb = if state["is_playing"].as_bool().unwrap_or(false) {
        "Playing"
    } else {
        "Paused on"
    };
    let by = item["artists"][0]["name"]
        .as_str()
        .map(|a| format!(" by {a}"))
        .unwrap_or_default();
    let album = item["album"]["name"]
        .as_str()
        .filter(|album| Some(*album) != item["name"].as_str())
        .map(|album| format!(", from the album {album}"))
        .unwrap_or_default();
    let position = match (item["duration_ms"].as_u64(), state["progress_ms"].as_u64()) {
        (Some(d), Some(p)) if d > 0 => format!(" ({} of {})", fmt_ms(p), fmt_ms(d)),
        _ => String::new(),
    };
    format!("{verb} '{name}'{by}{album}{position}")
}

fn fmt_ms(ms: u64) -> String {
    format!("{}:{:02}", ms / 60_000, (ms / 1000) % 60)
}

#[async_trait]
impl Capability for MusicCapability {
    fn name(&self) -> &'static str {
        "music"
    }

    fn handles(&self, msg: &MeshMessage) -> bool {
        matches!(msg, MeshMessage::MusicCommand(_))
    }

    async fn start(&self, tx: Sender<MeshMessage>) -> Result<(), String> {
        // start() re-runs on every reconnect; keep the sender fresh so
        // background work always reaches the live connection. The Phase 4
        // librespot supervisor spawns here, Once-guarded like
        // capabilities/audio's bluetooth status loop.
        *self.coordinator_tx.lock().unwrap() = Some(tx);
        Ok(())
    }

    async fn handle(&self, msg: MeshMessage, _tx: Sender<MeshMessage>) {
        if let MeshMessage::MusicCommand(cmd) = msg {
            let result = self.execute(&cmd).await;
            if let Some(tx) = self.tx()
                && tx
                    .send(MeshMessage::MusicCommandResult(result))
                    .await
                    .is_err()
            {
                warn!("music: coordinator channel closed while sending command result");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    fn command(action: &str) -> MusicCommandRequest {
        MusicCommandRequest {
            request_id: "req-1".into(),
            action: action.into(),
            params: serde_json::json!({}),
        }
    }

    #[test]
    fn handles_music_command_only() {
        let cap = MusicCapability::new("n1");
        assert!(cap.handles(&MeshMessage::MusicCommand(command("play"))));
        assert!(
            !cap.handles(&MeshMessage::MusicCommandResult(MusicCommandResult {
                request_id: "req-1".into(),
                ok: true,
                message: "ok".into(),
            }))
        );
        assert!(!cap.handles(&MeshMessage::AuthToken("t".into())));
    }

    #[tokio::test]
    async fn handle_replies_with_result_for_request_id() {
        let cap = MusicCapability::new("n1");
        let (tx, mut rx) = mpsc::channel(4);
        cap.start(tx.clone()).await.unwrap();

        // No Spotify credentials in the test environment, so any command
        // fails fast with the not-configured sentence — before any HTTP.
        cap.handle(MeshMessage::MusicCommand(command("pause")), tx)
            .await;

        match rx.recv().await {
            Some(MeshMessage::MusicCommandResult(result)) => {
                assert_eq!(result.request_id, "req-1");
                assert!(!result.ok);
                assert!(
                    result.message.contains("not configured"),
                    "{}",
                    result.message
                );
            }
            other => panic!("expected MusicCommandResult, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unknown_action_names_itself() {
        let cap = MusicCapability::new("n1");
        let err = cap.run("blast", &serde_json::json!({})).await.unwrap_err();
        assert!(err.contains("'blast'"), "{err}");
    }

    #[test]
    fn int_param_accepts_numbers_and_numeric_strings() {
        assert_eq!(
            int_param(&serde_json::json!({"seconds": -30}), "seconds"),
            Some(-30)
        );
        assert_eq!(
            int_param(&serde_json::json!({"seconds": "-30"}), "seconds"),
            Some(-30)
        );
        assert_eq!(int_param(&serde_json::json!({}), "seconds"), None);
    }

    #[test]
    fn search_hit_picks_first_non_null_item() {
        let results = serde_json::json!({
            "playlists": { "items": [null, { "name": "Jazz Classics", "uri": "spotify:playlist:1" }] }
        });
        let hit = search_hit(&results, "playlist").unwrap();
        assert_eq!(hit["name"], "Jazz Classics");
        assert!(search_hit(&serde_json::json!({"tracks": {"items": []}}), "track").is_none());
    }

    #[test]
    fn hit_summary_phrases_each_entity() {
        let track =
            serde_json::json!({ "name": "Hey Jude", "artists": [{ "name": "The Beatles" }] });
        assert_eq!(
            hit_summary(&track, "track"),
            "Now playing 'Hey Jude' by The Beatles"
        );
        assert_eq!(
            hit_summary(&track, "album"),
            "Playing the album 'Hey Jude' by The Beatles"
        );
        assert_eq!(hit_summary(&track, "artist"), "Playing music by Hey Jude");
        assert_eq!(
            hit_summary(&track, "playlist"),
            "Playing the playlist 'Hey Jude'"
        );
    }

    #[test]
    fn status_sentence_reads_naturally() {
        let state = serde_json::json!({
            "is_playing": true,
            "progress_ms": 83_000,
            "item": {
                "name": "Blackbird",
                "duration_ms": 225_000,
                "artists": [{ "name": "The Beatles" }],
                "album": { "name": "The White Album" }
            }
        });
        assert_eq!(
            status_sentence(&state),
            "Playing 'Blackbird' by The Beatles, from the album The White Album (1:23 of 3:45)"
        );
        assert_eq!(
            status_sentence(&serde_json::json!({})),
            "Nothing is playing"
        );
    }

    #[test]
    fn status_sentence_skips_album_matching_track_name() {
        // Singles are usually on an album of the same name — saying it twice
        // reads badly over TTS.
        let state = serde_json::json!({
            "is_playing": false,
            "item": {
                "name": "Hey Jude",
                "artists": [{ "name": "The Beatles" }],
                "album": { "name": "Hey Jude" }
            }
        });
        assert_eq!(
            status_sentence(&state),
            "Paused on 'Hey Jude' by The Beatles"
        );
    }
}
