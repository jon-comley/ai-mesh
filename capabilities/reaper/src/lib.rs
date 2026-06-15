use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use capability_core::Capability;
use shared::{
    MeshMessage, ReaperCommandRequest, ReaperCommandResult, ReaperScriptRequest,
    ReaperScriptResult, ReaperStatusReport,
};
use tokio::sync::mpsc::Sender;
use tracing::{debug, warn};

pub struct ReaperCapability {
    node_id: String,
    coordinator_tx: Arc<Mutex<Option<Sender<MeshMessage>>>>,
}

impl ReaperCapability {
    pub fn new(node_id: impl Into<String>) -> Self {
        Self {
            node_id: node_id.into(),
            coordinator_tx: Arc::new(Mutex::new(None)),
        }
    }

    /// Returns `host:port` for the REAPER web server.
    /// REAPER_HOST overrides the host (default: localhost).
    /// REAPER_PORT overrides the port (default: 8080).
    fn reaper_addr() -> String {
        let host = std::env::var("REAPER_HOST").unwrap_or_else(|_| "localhost".into());
        let port: u16 = std::env::var("REAPER_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(8080);
        format!("{host}:{port}")
    }

    fn tx(&self) -> Option<Sender<MeshMessage>> {
        self.coordinator_tx.lock().unwrap().clone()
    }
}

#[async_trait]
impl Capability for ReaperCapability {
    fn name(&self) -> &'static str {
        "reaper"
    }

    fn handles(&self, msg: &MeshMessage) -> bool {
        matches!(
            msg,
            MeshMessage::ReaperCommand(_) | MeshMessage::ReaperScript(_)
        )
    }

    async fn start(&self, tx: Sender<MeshMessage>) -> Result<(), String> {
        *self.coordinator_tx.lock().unwrap() = Some(tx.clone());

        let node_id = self.node_id.clone();
        let addr = Self::reaper_addr();
        let url = format!("http://{}/_/TRANSPORT", addr);
        let coordinator_tx = self.coordinator_tx.clone();

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .map_err(|e| format!("reaper: failed to build HTTP client: {e}"))?;

        tokio::spawn(async move {
            let mut first_success = true;
            loop {
                let report = match client.get(&url).send().await {
                    Ok(resp) if resp.status().is_success() => {
                        match parse_transport_response(resp, first_success).await {
                            Ok(fields) => {
                                first_success = false;
                                ReaperStatusReport {
                                    node_id: node_id.clone(),
                                    reaper_online: true,
                                    play_state: fields.0,
                                    position: fields.1,
                                    tempo: fields.2,
                                    ts_num: fields.3,
                                    ts_denom: fields.4,
                                }
                            }
                            Err(e) => {
                                debug!("reaper: failed to parse transport response: {e}");
                                offline_report(&node_id)
                            }
                        }
                    }
                    _ => {
                        debug!("reaper: REAPER web server unreachable at {url}");
                        offline_report(&node_id)
                    }
                };

                let tx_guard = coordinator_tx.lock().unwrap().clone();
                if let Some(tx) = tx_guard
                    && tx.send(MeshMessage::ReaperStatus(report)).await.is_err()
                {
                    debug!("reaper: coordinator channel closed, stopping poller");
                    break;
                }

                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        });

        Ok(())
    }

    async fn handle(&self, msg: MeshMessage, _tx: Sender<MeshMessage>) {
        let addr = Self::reaper_addr();
        match msg {
            MeshMessage::ReaperCommand(cmd) => {
                let result = execute_command(&cmd, &addr).await;
                if let Some(tx) = self.tx()
                    && tx
                        .send(MeshMessage::ReaperCommandResult(result))
                        .await
                        .is_err()
                {
                    warn!("reaper: coordinator channel closed while sending command result");
                }
            }
            MeshMessage::ReaperScript(cmd) => {
                let result = execute_script(&cmd, &addr).await;
                if let Some(tx) = self.tx()
                    && tx
                        .send(MeshMessage::ReaperScriptResult(result))
                        .await
                        .is_err()
                {
                    warn!("reaper: coordinator channel closed while sending script result");
                }
            }
            _ => {}
        }
    }
}

// ── Transport response parsing ────────────────────────────────────────────────

/// Returns (play_state, position, tempo, ts_num, ts_denom).
async fn parse_transport_response(
    resp: reqwest::Response,
    log_raw: bool,
) -> Result<(u8, f64, f64, u32, u32), String> {
    let body = resp
        .text()
        .await
        .map_err(|e| format!("failed to read body: {e}"))?;

    if log_raw {
        debug!("reaper: raw /_/TRANSPORT response: {:?}", body);
    }

    // Try JSON first (newer REAPER web server builds).
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
        let play_state = v["play"].as_u64().unwrap_or(0) as u8;
        let position = v["position"].as_f64().unwrap_or(0.0);
        let tempo = v["tempo"].as_f64().unwrap_or(120.0);
        let ts_num = v["ts_num"].as_u64().unwrap_or(4) as u32;
        let ts_denom = v["ts_denom"].as_u64().unwrap_or(4) as u32;
        return Ok((play_state, position, tempo, ts_num, ts_denom));
    }

    let parts: Vec<&str> = body.trim().split('\t').collect();

    // Format A (no header, 8 fields):
    // play_state \t play_rate \t repeat \t position \t loop_mode \t tempo \t ts_num \t ts_denom
    if parts.len() >= 8 && parts[0].parse::<u8>().is_ok() {
        let play_state = parts[0].parse::<u8>().unwrap_or(0);
        let position = parts[3].parse::<f64>().unwrap_or(0.0);
        let tempo = parts[5].parse::<f64>().unwrap_or(120.0);
        let ts_num = parts[6].parse::<u32>().unwrap_or(4);
        let ts_denom = parts[7].parse::<u32>().unwrap_or(4);
        return Ok((play_state, position, tempo, ts_num, ts_denom));
    }

    // Format B (TRANSPORT header, 6 fields):
    // TRANSPORT \t play_state \t position_secs \t repeat \t pos_str \t pos_str2
    if parts.first() == Some(&"TRANSPORT") && parts.len() >= 3 {
        let play_state = parts[1].parse::<u8>().unwrap_or(0);
        let position = parts[2].parse::<f64>().unwrap_or(0.0);
        return Ok((play_state, position, 120.0, 4, 4));
    }

    Err(format!(
        "unrecognised REAPER transport format (len={}, first={:?})",
        parts.len(),
        parts.first()
    ))
}

fn offline_report(node_id: &str) -> ReaperStatusReport {
    ReaperStatusReport {
        node_id: node_id.to_string(),
        reaper_online: false,
        play_state: 0,
        position: 0.0,
        tempo: 0.0,
        ts_num: 0,
        ts_denom: 0,
    }
}

// ── Command execution ─────────────────────────────────────────────────────────

/// Named transport actions → REAPER command IDs.
fn named_action_id(action: &str) -> Option<u32> {
    match action {
        "play" => Some(1008),
        "stop" => Some(1007),
        "pause" => Some(1016),
        "record" => Some(1013),
        "rewind" => Some(40042),
        "new_project" => Some(40023),
        "save" => Some(40022),
        _ => None,
    }
}

async fn execute_command(cmd: &ReaperCommandRequest, addr: &str) -> ReaperCommandResult {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return ReaperCommandResult {
                request_id: cmd.request_id.clone(),
                ok: false,
                message: format!("failed to build HTTP client: {e}"),
            };
        }
    };

    // Resolve the action to a URL path segment.
    let action_path = if let Some(id) = named_action_id(&cmd.action) {
        // Named transport action (play/stop/etc.)
        id.to_string()
    } else if cmd.action.chars().all(|c| c.is_ascii_digit()) {
        // Numeric action ID passed as a string.
        cmd.action.clone()
    } else {
        // Named string action (e.g. SWS "_SWS_ABOUT") — url-encode it.
        url_encode(&cmd.action)
    };

    let url = format!("http://{}/_/{};", addr, action_path);
    debug!("reaper: sending command GET {}", url);

    match client.get(&url).send().await {
        Ok(resp) if resp.status().is_success() || resp.status().as_u16() == 204 => {
            ReaperCommandResult {
                request_id: cmd.request_id.clone(),
                ok: true,
                message: "ok".into(),
            }
        }
        Ok(resp) => ReaperCommandResult {
            request_id: cmd.request_id.clone(),
            ok: false,
            message: format!("REAPER returned HTTP {}", resp.status()),
        },
        Err(e) => ReaperCommandResult {
            request_id: cmd.request_id.clone(),
            ok: false,
            message: format!("HTTP request failed: {e}"),
        },
    }
}

async fn execute_script(cmd: &ReaperScriptRequest, _addr: &str) -> ReaperScriptResult {
    let scripts_dir = std::env::var("REAPER_WSL_SCRIPTS_PATH")
        .unwrap_or_else(|_| "/mnt/c/Users/jonno/AppData/Roaming/REAPER/Scripts".into());

    let cmd_path = format!("{}/ai_mesh_cmd.lua", scripts_dir);
    let id_path = format!("{}/ai_mesh_id.txt", scripts_dir);

    if let Err(e) = tokio::fs::write(&cmd_path, cmd.code.as_bytes()).await {
        return ReaperScriptResult {
            request_id: cmd.request_id.clone(),
            ok: false,
            message: format!("failed to write command to {cmd_path}: {e}"),
        };
    }

    if let Err(e) = tokio::fs::write(&id_path, cmd.request_id.as_bytes()).await {
        return ReaperScriptResult {
            request_id: cmd.request_id.clone(),
            ok: false,
            message: format!("failed to write id to {id_path}: {e}"),
        };
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    ReaperScriptResult {
        request_id: cmd.request_id.clone(),
        ok: true,
        message: "dispatched to daemon".into(),
    }
}

fn url_encode(s: &str) -> String {
    s.chars()
        .flat_map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') {
                vec![c]
            } else {
                format!("%{:02X}", c as u32).chars().collect()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_maps_to_1013_not_play_stop() {
        // Regression: 1009 is "Transport: Play/stop" and silently plays instead of
        // recording. The record action is 1013.
        assert_eq!(named_action_id("record"), Some(1013));
    }

    #[test]
    fn rewind_maps_to_go_to_start() {
        // Regression: 40113 is not "go to start"; 40042 is.
        assert_eq!(named_action_id("rewind"), Some(40042));
    }

    #[test]
    fn known_transport_actions_resolve() {
        assert_eq!(named_action_id("play"), Some(1008));
        assert_eq!(named_action_id("stop"), Some(1007));
        assert_eq!(named_action_id("pause"), Some(1016));
        assert_eq!(named_action_id("new_project"), Some(40023));
        assert_eq!(named_action_id("save"), Some(40022));
    }

    #[test]
    fn unknown_action_is_none() {
        assert_eq!(named_action_id("frobnicate"), None);
    }

    #[test]
    fn url_encode_passes_unreserved_and_escapes_rest() {
        assert_eq!(url_encode("_SWS_ABOUT"), "_SWS_ABOUT");
        assert_eq!(url_encode("40075"), "40075");
        assert_eq!(url_encode("a b"), "a%20b");
    }

    #[tokio::test]
    async fn execute_script_writes_daemon_files() {
        let dir = std::env::temp_dir().join(format!("ai_mesh_test_{}", std::process::id()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        // SAFETY: single-threaded test, no concurrent env access.
        unsafe {
            std::env::set_var("REAPER_WSL_SCRIPTS_PATH", &dir);
        }

        let req = ReaperScriptRequest {
            request_id: "req-42".into(),
            code: "reaper.InsertTrackAtIndex(0, true)".into(),
        };
        let result = execute_script(&req, "127.0.0.1:8080").await;

        assert!(result.ok);
        assert_eq!(result.request_id, "req-42");

        let cmd = tokio::fs::read_to_string(dir.join("ai_mesh_cmd.lua"))
            .await
            .unwrap();
        let id = tokio::fs::read_to_string(dir.join("ai_mesh_id.txt"))
            .await
            .unwrap();
        assert_eq!(cmd, "reaper.InsertTrackAtIndex(0, true)");
        assert_eq!(id, "req-42");

        unsafe {
            std::env::remove_var("REAPER_WSL_SCRIPTS_PATH");
        }
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
