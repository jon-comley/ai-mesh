//! Music capability — Spotify playback and control (plans/spotify-music.md).
//!
//! Two planes, both landing in later phases:
//! - control plane: Spotify Web API (search, play, pause, seek, …) driven by
//!   `MusicCommand` messages from the coordinator's `music_control` tool;
//! - playback engine: a supervised `librespot` child process (pipe backend)
//!   feeding PCM into the node's PipeWire sink.
//!
//! This is the Phase 1 skeleton: wire types and routing exist end-to-end, but
//! every command answers "not configured yet" until the Web API client
//! (Phase 3) and the librespot supervisor (Phase 4) land.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use capability_core::Capability;
use shared::{MeshMessage, MusicCommandRequest, MusicCommandResult};
use tokio::sync::mpsc::Sender;
use tracing::warn;

pub struct MusicCapability {
    #[allow(dead_code)] // used by the Phase 3 Web API client (device naming, logs)
    node_id: String,
    coordinator_tx: Arc<Mutex<Option<Sender<MeshMessage>>>>,
}

impl MusicCapability {
    pub fn new(node_id: impl Into<String>) -> Self {
        Self {
            node_id: node_id.into(),
            coordinator_tx: Arc::new(Mutex::new(None)),
        }
    }

    fn tx(&self) -> Option<Sender<MeshMessage>> {
        self.coordinator_tx.lock().unwrap().clone()
    }

    async fn execute(&self, cmd: &MusicCommandRequest) -> MusicCommandResult {
        // Phase 3 replaces this stub with the Spotify Web API dispatch
        // (search/play/pause/seek/volume/status).
        MusicCommandResult {
            request_id: cmd.request_id.clone(),
            ok: false,
            message: "the music capability is not configured yet".into(),
        }
    }
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

        cap.handle(MeshMessage::MusicCommand(command("play")), tx)
            .await;

        match rx.recv().await {
            Some(MeshMessage::MusicCommandResult(result)) => {
                assert_eq!(result.request_id, "req-1");
                assert!(!result.ok);
                assert!(result.message.contains("not configured"));
            }
            other => panic!("expected MusicCommandResult, got {other:?}"),
        }
    }
}
