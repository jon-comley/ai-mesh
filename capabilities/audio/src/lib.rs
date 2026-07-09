//! Audio output sink — Phase 2/3 of `plans/audio-output-integration.md`.
//! Plays a clip fetched from a URL (the same coordinator-served
//! `/api/voice/tts/{id}` clips the ESPHome puck fetches — see
//! `capabilities/voice/src/tts.rs`) via whatever local audio hardware this
//! node is physically connected to: a directly-paired Bluetooth speaker
//! (Phase 2, the kitchen Blaupunkt/Fishman) or HDMI-out through the Frame
//! TV to the soundbar (Phase 3, the Pi 4 behind the TV).
//!
//! **Unverified without hardware in hand** (see the assumptions list this
//! shipped with): the exact playback command for either backend. Rather
//! than hard-code a specific audio stack (PipeWire vs PulseAudio vs
//! BlueALSA for Bluetooth; a specific ALSA HDMI device name that varies by
//! Pi model/firmware), the actual shell command is a configurable
//! template (`AUDIO_PLAY_CMD`, `{file}` substituted) so it can be
//! corrected without a code change once real hardware confirms what's
//! actually installed. The defaults below are reasonable starting guesses,
//! not confirmed-working commands.

use async_trait::async_trait;
use capability_core::Capability;
use shared::MeshMessage;
use tokio::sync::mpsc::Sender;
use tracing::{info, warn};

/// Which physical output this node drives. Purely descriptive (used in
/// logs and to pick the default play command) — the mesh doesn't
/// distinguish sink *kinds*, only "this node advertises Feature::Audio";
/// which room/purpose it serves is entirely a registry-side preference
/// (`room-audio-sink:<room>`, set via the dashboard — not yet built as UI,
/// see the assumptions list).
fn backend_name() -> String {
    std::env::var("AUDIO_BACKEND").unwrap_or_else(|_| "bluetooth".into())
}

/// The actual playback command, `{file}` replaced with the downloaded
/// clip's path. Defaults differ by backend since they need fundamentally
/// different audio paths (PulseAudio/PipeWire's default sink for a paired
/// Bluetooth speaker vs raw ALSA for HDMI) — **both are unverified
/// guesses**, override via `AUDIO_PLAY_CMD` once real hardware confirms
/// what's actually installed and named.
fn play_cmd_template() -> String {
    if let Ok(cmd) = std::env::var("AUDIO_PLAY_CMD") {
        return cmd;
    }
    match backend_name().as_str() {
        "hdmi" => {
            // ALSA device name for Pi HDMI audio varies by model/firmware
            // and which HDMI port — "default" relies on the system's own
            // ALSA config picking the right card, which may not be true
            // out of the box. AUDIO_ALSA_DEVICE overrides just the device
            // half without needing the whole command re-templated.
            let device = std::env::var("AUDIO_ALSA_DEVICE").unwrap_or_else(|_| "default".into());
            format!("aplay -D {device} {{file}}")
        }
        // paplay targets PulseAudio/PipeWire's current default sink — for
        // a paired Bluetooth speaker to be that default, it must already
        // be trusted+connected+set-as-default via a one-time bluetoothctl
        // setup this capability does NOT perform (see assumptions).
        _ => "paplay {file}".into(),
    }
}

fn download_dir() -> std::path::PathBuf {
    std::env::var("AUDIO_CACHE_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("ai-mesh-audio"))
}

pub struct AudioCapability {
    node_id: String,
}

impl AudioCapability {
    pub fn new(node_id: impl Into<String>) -> Self {
        Self {
            node_id: node_id.into(),
        }
    }
}

#[async_trait]
impl Capability for AudioCapability {
    fn name(&self) -> &'static str {
        "audio"
    }

    fn handles(&self, msg: &MeshMessage) -> bool {
        matches!(msg, MeshMessage::AudioPlay(_))
    }

    async fn start(&self, _tx: Sender<MeshMessage>) -> Result<(), String> {
        info!(
            node_id = %self.node_id,
            backend = %backend_name(),
            "audio: ready (backend selected via AUDIO_BACKEND)"
        );
        Ok(())
    }

    async fn handle(&self, msg: MeshMessage, _tx: Sender<MeshMessage>) {
        let MeshMessage::AudioPlay(req) = msg else {
            return;
        };
        if let Err(e) = play_url(&req.url).await {
            warn!(request_id = %req.request_id, error = %e, "audio: playback failed");
        }
    }
}

async fn play_url(url: &str) -> Result<(), String> {
    let dir = download_dir();
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| e.to_string())?;
    let path = dir.join(format!("{}.wav", uuid::Uuid::new_v4()));

    let resp = reqwest::get(url).await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("fetch returned {}", resp.status()));
    }
    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
    tokio::fs::write(&path, &bytes)
        .await
        .map_err(|e| e.to_string())?;

    let result = run_play_command(&path).await;
    let _ = tokio::fs::remove_file(&path).await;
    result
}

async fn run_play_command(path: &std::path::Path) -> Result<(), String> {
    let cmd_line = play_cmd_template().replace("{file}", &path.to_string_lossy());
    let mut parts = cmd_line.split_whitespace();
    let program = parts.next().ok_or("empty AUDIO_PLAY_CMD")?;
    let status = tokio::process::Command::new(program)
        .args(parts)
        .status()
        .await
        .map_err(|e| format!("failed to run '{cmd_line}': {e}"))?;
    if status.success() {
        info!(cmd = %cmd_line, "audio: played clip");
        Ok(())
    } else {
        Err(format!("'{cmd_line}' exited with {status}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // AUDIO_BACKEND/AUDIO_PLAY_CMD/AUDIO_ALSA_DEVICE are process-global and
    // these tests run in parallel by default — serialize rather than race
    // (same fix needed in coordinator/src/http/api/voice.rs's tests today).
    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[test]
    fn capability_only_handles_audio_play() {
        let cap = AudioCapability::new("test-node");
        assert_eq!(cap.name(), "audio");
        assert!(!cap.handles(&MeshMessage::Acknowledge));
        assert!(
            cap.handles(&MeshMessage::AudioPlay(shared::AudioPlayRequest {
                request_id: "r1".into(),
                url: "http://example/x.wav".into(),
            }))
        );
    }

    #[test]
    fn hdmi_backend_template_uses_alsa_device_env() {
        let _guard = ENV_LOCK.blocking_lock();
        // SAFETY: caller holds ENV_LOCK for the duration of the test.
        unsafe {
            std::env::set_var("AUDIO_BACKEND", "hdmi");
            std::env::remove_var("AUDIO_PLAY_CMD");
            std::env::set_var("AUDIO_ALSA_DEVICE", "hw:1,0");
        }
        assert_eq!(play_cmd_template(), "aplay -D hw:1,0 {file}");
        unsafe {
            std::env::remove_var("AUDIO_BACKEND");
            std::env::remove_var("AUDIO_ALSA_DEVICE");
        }
    }

    #[test]
    fn bluetooth_backend_defaults_to_paplay() {
        let _guard = ENV_LOCK.blocking_lock();
        // SAFETY: caller holds ENV_LOCK for the duration of the test.
        unsafe {
            std::env::remove_var("AUDIO_BACKEND");
            std::env::remove_var("AUDIO_PLAY_CMD");
        }
        assert_eq!(play_cmd_template(), "paplay {file}");
    }

    #[test]
    fn explicit_audio_play_cmd_overrides_backend_defaults() {
        let _guard = ENV_LOCK.blocking_lock();
        // SAFETY: caller holds ENV_LOCK for the duration of the test.
        unsafe {
            std::env::set_var("AUDIO_PLAY_CMD", "mpv --no-video {file}");
        }
        assert_eq!(play_cmd_template(), "mpv --no-video {file}");
        unsafe {
            std::env::remove_var("AUDIO_PLAY_CMD");
        }
    }

    #[tokio::test]
    async fn run_play_command_reports_nonzero_exit() {
        let _guard = ENV_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clip.wav");
        tokio::fs::write(&path, b"x").await.unwrap();
        // SAFETY: caller holds ENV_LOCK for the duration of the test.
        unsafe {
            std::env::set_var("AUDIO_PLAY_CMD", "false {file}");
        }
        let result = run_play_command(&path).await;
        unsafe {
            std::env::remove_var("AUDIO_PLAY_CMD");
        }
        assert!(result.is_err());
    }
}
