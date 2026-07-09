//! Audio output sink — Phase 2/3 of `plans/audio-output-integration.md`.
//! Plays a clip fetched from a URL (the same coordinator-served
//! `/api/voice/tts/{id}` clips the ESPHome puck fetches — see
//! `capabilities/voice/src/tts.rs`) via whatever local audio hardware this
//! node is physically connected to: a directly-paired Bluetooth speaker
//! (Phase 2, e.g. a kitchen or office room speaker) or HDMI-out through a
//! TV to its soundbar (Phase 3).
//!
//! **A node can run more than one backend at once** — e.g. a Pi wired to a
//! TV over HDMI that's also, at the same time, the Bluetooth host for a
//! room speaker. `AUDIO_BACKENDS` is a comma-separated list of the
//! backends this node has configured (e.g. `"hdmi,bluetooth"`); each
//! `AudioPlayRequest` names which one it wants via `sink` (`None` uses the
//! list's first entry as this node's default).
//!
//! **Unverified without hardware in hand** (see the assumptions list this
//! shipped with): the exact playback command for either backend. Rather
//! than hard-code a specific audio stack (PipeWire vs PulseAudio vs
//! BlueALSA for Bluetooth; a specific ALSA HDMI device name that varies by
//! Pi model/firmware), the actual shell command per backend is a
//! configurable template (`AUDIO_PLAY_CMD_<BACKEND>`, `{file}`
//! substituted) so it can be corrected without a code change once real
//! hardware confirms what's actually installed. The built-in defaults
//! below are reasonable starting guesses, not confirmed-working commands.

use async_trait::async_trait;
use capability_core::Capability;
use shared::MeshMessage;
use tokio::sync::mpsc::Sender;
use tracing::{info, warn};

/// This node's configured backends, in priority order — the first is the
/// default used when a request doesn't name a `sink`. Purely descriptive
/// beyond that (used in logs and to pick play commands) — the mesh
/// doesn't distinguish sink *kinds* at the `Feature::Audio` level, only
/// "this node advertises audio"; which room/purpose each backend serves
/// is entirely a registry-side preference (`room-audio-sink:<room>`, see
/// `coordinator/src/audio.rs`).
///
/// `pub` because the agent's `detect_capabilities()` reports this same
/// list in `NodeCapabilities.audio_backends` — one parser, no drift.
pub fn configured_backends() -> Vec<String> {
    std::env::var("AUDIO_BACKENDS")
        .unwrap_or_else(|_| "bluetooth".into())
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn default_backend() -> Option<String> {
    configured_backends().into_iter().next()
}

/// The actual playback command for one backend, `{file}` replaced with the
/// downloaded clip's path. Built-in defaults differ by backend since they
/// need fundamentally different audio paths (PulseAudio/PipeWire's default
/// sink for a paired Bluetooth speaker vs raw ALSA for HDMI) — **both are
/// unverified guesses**, override via `AUDIO_PLAY_CMD_<BACKEND>` (e.g.
/// `AUDIO_PLAY_CMD_HDMI`) once real hardware confirms what's actually
/// installed and named. Returns `None` for a backend name this crate
/// doesn't have a built-in default for and that has no override set —
/// that's a node misconfiguration, not a runtime error to guess through.
fn play_cmd_template_for(backend: &str) -> Option<String> {
    let override_var = format!("AUDIO_PLAY_CMD_{}", backend.to_uppercase());
    if let Ok(cmd) = std::env::var(&override_var) {
        return Some(cmd);
    }
    match backend {
        "hdmi" => {
            // ALSA device name for Pi HDMI audio varies by model/firmware
            // and which HDMI port — "default" relies on the system's own
            // ALSA config picking the right card, which may not be true
            // out of the box. AUDIO_ALSA_DEVICE overrides just the device
            // half without needing the whole command re-templated.
            let device = std::env::var("AUDIO_ALSA_DEVICE").unwrap_or_else(|_| "default".into());
            Some(format!("aplay -D {device} {{file}}"))
        }
        // paplay targets PulseAudio/PipeWire's current default sink — for
        // a paired Bluetooth speaker to be that default, it must already
        // be trusted+connected+set-as-default via a one-time bluetoothctl
        // setup this capability does NOT perform (see assumptions).
        "bluetooth" => Some("paplay {file}".into()),
        _ => None,
    }
}

/// Which backend a request should actually use: the named `sink`, or this
/// node's default if unspecified. Errors if the node isn't configured for
/// the requested (or default) backend at all — a clear failure instead of
/// silently falling back to whatever happens to be first.
fn resolve_backend(requested: Option<&str>) -> Result<String, String> {
    let backends = configured_backends();
    let backend = match requested {
        Some(b) => b.to_string(),
        None => default_backend().ok_or("node has no AUDIO_BACKENDS configured")?,
    };
    if !backends.iter().any(|b| b == &backend) {
        return Err(format!(
            "node is not configured for backend '{backend}' (has: {})",
            backends.join(", ")
        ));
    }
    Ok(backend)
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
            backends = %configured_backends().join(","),
            "audio: ready (backends selected via AUDIO_BACKENDS)"
        );
        Ok(())
    }

    async fn handle(&self, msg: MeshMessage, _tx: Sender<MeshMessage>) {
        let MeshMessage::AudioPlay(req) = msg else {
            return;
        };
        if let Err(e) = play_url(&req.url, req.sink.as_deref()).await {
            warn!(request_id = %req.request_id, error = %e, "audio: playback failed");
        }
    }
}

async fn play_url(url: &str, sink: Option<&str>) -> Result<(), String> {
    let backend = resolve_backend(sink)?;
    let template = play_cmd_template_for(&backend).ok_or_else(|| {
        format!(
            "no play command known for backend '{backend}' — set AUDIO_PLAY_CMD_{}",
            backend.to_uppercase()
        )
    })?;

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

    let result = run_play_command(&path, &template).await;
    let _ = tokio::fs::remove_file(&path).await;
    result
}

async fn run_play_command(path: &std::path::Path, template: &str) -> Result<(), String> {
    let cmd_line = template.replace("{file}", &path.to_string_lossy());
    let mut parts = cmd_line.split_whitespace();
    let program = parts.next().ok_or("empty play command")?;
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

    // AUDIO_BACKENDS/AUDIO_PLAY_CMD_*/AUDIO_ALSA_DEVICE are process-global
    // and these tests run in parallel by default — serialize rather than
    // race (same fix needed in coordinator/src/http/api/voice.rs's tests).
    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn clear_audio_env() {
        // SAFETY: caller holds ENV_LOCK for the duration of the test.
        unsafe {
            std::env::remove_var("AUDIO_BACKENDS");
            std::env::remove_var("AUDIO_PLAY_CMD_HDMI");
            std::env::remove_var("AUDIO_PLAY_CMD_BLUETOOTH");
            std::env::remove_var("AUDIO_ALSA_DEVICE");
        }
    }

    #[test]
    fn capability_only_handles_audio_play() {
        let cap = AudioCapability::new("test-node");
        assert_eq!(cap.name(), "audio");
        assert!(!cap.handles(&MeshMessage::Acknowledge));
        assert!(
            cap.handles(&MeshMessage::AudioPlay(shared::AudioPlayRequest {
                request_id: "r1".into(),
                url: "http://example/x.wav".into(),
                sink: None,
            }))
        );
    }

    #[test]
    fn single_backend_defaults_to_bluetooth() {
        let _guard = ENV_LOCK.blocking_lock();
        clear_audio_env();
        assert_eq!(configured_backends(), vec!["bluetooth".to_string()]);
        assert_eq!(default_backend(), Some("bluetooth".into()));
    }

    #[test]
    fn multiple_backends_parsed_in_order() {
        let _guard = ENV_LOCK.blocking_lock();
        clear_audio_env();
        // SAFETY: caller holds ENV_LOCK for the duration of the test.
        unsafe {
            std::env::set_var("AUDIO_BACKENDS", "hdmi, bluetooth");
        }
        assert_eq!(
            configured_backends(),
            vec!["hdmi".to_string(), "bluetooth".to_string()]
        );
        assert_eq!(default_backend(), Some("hdmi".into()));
        clear_audio_env();
    }

    #[test]
    fn hdmi_backend_template_uses_alsa_device_env() {
        let _guard = ENV_LOCK.blocking_lock();
        clear_audio_env();
        // SAFETY: caller holds ENV_LOCK for the duration of the test.
        unsafe {
            std::env::set_var("AUDIO_ALSA_DEVICE", "hw:1,0");
        }
        assert_eq!(
            play_cmd_template_for("hdmi"),
            Some("aplay -D hw:1,0 {file}".into())
        );
        clear_audio_env();
    }

    #[test]
    fn bluetooth_backend_defaults_to_paplay() {
        let _guard = ENV_LOCK.blocking_lock();
        clear_audio_env();
        assert_eq!(
            play_cmd_template_for("bluetooth"),
            Some("paplay {file}".into())
        );
    }

    #[test]
    fn unknown_backend_with_no_override_has_no_template() {
        let _guard = ENV_LOCK.blocking_lock();
        clear_audio_env();
        assert_eq!(play_cmd_template_for("airplay"), None);
    }

    #[test]
    fn per_backend_play_cmd_overrides_only_that_backend() {
        let _guard = ENV_LOCK.blocking_lock();
        clear_audio_env();
        // SAFETY: caller holds ENV_LOCK for the duration of the test.
        unsafe {
            std::env::set_var("AUDIO_PLAY_CMD_HDMI", "mpv --no-video {file}");
        }
        assert_eq!(
            play_cmd_template_for("hdmi"),
            Some("mpv --no-video {file}".into())
        );
        assert_eq!(
            play_cmd_template_for("bluetooth"),
            Some("paplay {file}".into())
        );
        clear_audio_env();
    }

    #[test]
    fn resolve_backend_uses_default_when_sink_unset() {
        let _guard = ENV_LOCK.blocking_lock();
        clear_audio_env();
        // SAFETY: caller holds ENV_LOCK for the duration of the test.
        unsafe {
            std::env::set_var("AUDIO_BACKENDS", "hdmi,bluetooth");
        }
        assert_eq!(resolve_backend(None), Ok("hdmi".into()));
        clear_audio_env();
    }

    #[test]
    fn resolve_backend_honours_explicit_sink() {
        let _guard = ENV_LOCK.blocking_lock();
        clear_audio_env();
        // SAFETY: caller holds ENV_LOCK for the duration of the test.
        unsafe {
            std::env::set_var("AUDIO_BACKENDS", "hdmi,bluetooth");
        }
        assert_eq!(resolve_backend(Some("bluetooth")), Ok("bluetooth".into()));
        clear_audio_env();
    }

    #[test]
    fn resolve_backend_rejects_unconfigured_sink() {
        let _guard = ENV_LOCK.blocking_lock();
        clear_audio_env();
        // SAFETY: caller holds ENV_LOCK for the duration of the test.
        unsafe {
            std::env::set_var("AUDIO_BACKENDS", "hdmi");
        }
        let err = resolve_backend(Some("bluetooth")).unwrap_err();
        assert!(err.contains("not configured for backend 'bluetooth'"));
        clear_audio_env();
    }

    #[tokio::test]
    async fn run_play_command_reports_nonzero_exit() {
        let _guard = ENV_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clip.wav");
        tokio::fs::write(&path, b"x").await.unwrap();
        let result = run_play_command(&path, "false {file}").await;
        assert!(result.is_err());
    }
}
