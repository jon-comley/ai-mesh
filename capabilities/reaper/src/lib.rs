use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use capability_core::Capability;
use shared::{MeshMessage, ReaperCommandRequest, ReaperCommandResult, ReaperStatusReport};
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
        matches!(msg, MeshMessage::ReaperCommand(_))
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
        let MeshMessage::ReaperCommand(cmd) = msg else {
            return;
        };
        let result = execute_command(&cmd, &Self::reaper_addr()).await;
        if let Some(tx) = self.tx()
            && tx
                .send(MeshMessage::ReaperCommandResult(result))
                .await
                .is_err()
        {
            warn!("reaper: coordinator channel closed while sending command result");
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

    // Fall back to tab-delimited format:
    // play_state \t play_rate \t repeat \t position \t loop_mode \t tempo \t ts_num \t ts_denom
    let parts: Vec<&str> = body.trim().split('\t').collect();
    if parts.len() >= 8 {
        let play_state = parts[0].parse::<u8>().unwrap_or(0);
        let position = parts[3].parse::<f64>().unwrap_or(0.0);
        let tempo = parts[5].parse::<f64>().unwrap_or(120.0);
        let ts_num = parts[6].parse::<u32>().unwrap_or(4);
        let ts_denom = parts[7].parse::<u32>().unwrap_or(4);
        return Ok((play_state, position, tempo, ts_num, ts_denom));
    }

    Err(format!(
        "unrecognised REAPER transport format (len={})",
        parts.len()
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
        "record" => Some(1009),
        "rewind" => Some(40113),
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

    let url = format!("http://{}/_/command/{}", addr, action_path);
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
