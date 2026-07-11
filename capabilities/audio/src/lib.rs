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

mod bluetooth;

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
        // Target the specific sink resolved by the dashboard's "Scan for
        // Bluetooth" pairing flow (see `bluetooth.rs`), not PipeWire's
        // *default* sink — a node can be paired to a device that never
        // became the OS default, or the default can drift after a reboot
        // or a second device gets paired system-wide. Falls back to
        // "paplay {file}" against the default sink only if nothing has
        // been paired through the dashboard yet.
        "bluetooth" => match paired_bluetooth_sink() {
            Some(sink) => Some(format!("paplay --device={sink} {{file}}")),
            None => Some("paplay {file}".into()),
        },
        _ => None,
    }
}

/// `~/.ai-mesh/bluetooth_sink.txt` (override: `AUDIO_STATE_DIR`) — NOT
/// `download_dir()` (which defaults to `std::env::temp_dir()`, wiped on
/// reboot). This needs to actually survive a reboot: confirmed live
/// 2026-07-10/11 pairing a Bluetooth amp, where losing this file after a
/// restart meant playback silently fell back to the default sink instead
/// of the paired speaker. Mirrors the node-id persistence convention in
/// `agent::identity::generate_node_id`.
fn bluetooth_sink_state_path() -> std::path::PathBuf {
    let base = std::env::var("AUDIO_STATE_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("."))
                .join(".ai-mesh")
        });
    base.join("bluetooth_sink.txt")
}

/// The currently-paired Bluetooth device, persisted as JSON rather than a
/// bare sink name — `mac`/`name` are needed so a restarted agent can still
/// confirm which device an unpair request is targeting and which MAC the
/// status-polling loop (`bluetooth_status_loop`) should check.
#[derive(serde::Serialize, serde::Deserialize)]
struct PairedDevice {
    mac: String,
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sink_name: Option<String>,
}

/// Persists the outcome of a successful `bluetooth::pair()` so subsequent
/// playback targets the resolved sink explicitly instead of relying on the
/// OS default, and so the device survives an agent restart for unpair/status
/// purposes. Node-local only — survives this node's own restarts, not
/// migrated between nodes.
fn persist_paired_device(mac: &str, name: &str, sink_name: Option<&str>) {
    let path = bluetooth_sink_state_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let device = PairedDevice {
        mac: mac.to_string(),
        name: name.to_string(),
        sink_name: sink_name.map(str::to_string),
    };
    match serde_json::to_string(&device) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&path, json) {
                warn!(error = %e, "audio: failed to persist paired bluetooth device");
            }
        }
        Err(e) => warn!(error = %e, "audio: failed to serialize paired bluetooth device"),
    }
}

fn paired_device() -> Option<PairedDevice> {
    let text = std::fs::read_to_string(bluetooth_sink_state_path()).ok()?;
    serde_json::from_str(&text).ok()
}

/// Removes the persisted paired-device state — called after a successful
/// unpair of the currently-paired MAC so a stale sink/status isn't reused.
fn clear_paired_device() {
    let _ = std::fs::remove_file(bluetooth_sink_state_path());
}

fn paired_bluetooth_sink() -> Option<String> {
    paired_device().and_then(|d| d.sink_name)
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
        match msg {
            MeshMessage::AudioPlay(_) => true,
            // Only a node actually configured for bluetooth playback should
            // touch bluetoothctl — otherwise an HDMI-only node would spawn
            // it and start driving whatever Bluetooth radio it happens to
            // have, which nobody asked for.
            MeshMessage::BluetoothScan(_)
            | MeshMessage::BluetoothPair(_)
            | MeshMessage::BluetoothClearCache(_)
            | MeshMessage::BluetoothUnpair(_) => {
                configured_backends().iter().any(|b| b == "bluetooth")
            }
            _ => false,
        }
    }

    async fn start(&self, tx: Sender<MeshMessage>) -> Result<(), String> {
        info!(
            node_id = %self.node_id,
            backends = %configured_backends().join(","),
            "audio: ready (backends selected via AUDIO_BACKENDS)"
        );
        if configured_backends().iter().any(|b| b == "bluetooth") {
            let node_id = self.node_id.clone();
            tokio::spawn(bluetooth_status_loop(node_id, tx));
        }
        Ok(())
    }

    async fn handle(&self, msg: MeshMessage, tx: Sender<MeshMessage>) {
        match msg {
            MeshMessage::AudioPlay(req) => {
                let result = play_url(&req.url, req.sink.as_deref()).await;
                let error = if let Err(e) = &result {
                    warn!(request_id = %req.request_id, error = %e, "audio: playback failed");
                    Some(e.clone())
                } else {
                    None
                };
                let _ = tx
                    .send(MeshMessage::AudioPlayResult(shared::AudioPlayResult {
                        request_id: req.request_id,
                        success: result.is_ok(),
                        error,
                    }))
                    .await;
            }
            MeshMessage::BluetoothScan(req) => {
                let node_id = self.node_id.clone();
                let result = bluetooth::scan(req.seconds, |dev| {
                    let mac = dev.mac.clone();
                    let name = dev.name.clone();
                    let rssi = dev.rssi;
                    match tx.try_send(MeshMessage::BluetoothDeviceFound(
                        shared::BluetoothDeviceInfo {
                            node_id: node_id.clone(),
                            mac: dev.mac,
                            name: dev.name,
                            rssi: dev.rssi,
                        },
                    )) {
                        Ok(()) => {
                            tracing::info!(%mac, %name, ?rssi, "bluetooth: device found, queued to coordinator")
                        }
                        Err(e) => {
                            warn!(%mac, %name, error = %e, "bluetooth: device found but failed to queue to coordinator")
                        }
                    }
                })
                .await;
                if let Err(e) = result {
                    warn!(request_id = %req.request_id, error = %e, "bluetooth: scan failed");
                    let _ = tx
                        .send(MeshMessage::BluetoothScanError(
                            shared::BluetoothScanError {
                                node_id: node_id.clone(),
                                error: e,
                            },
                        ))
                        .await;
                }
            }
            MeshMessage::BluetoothPair(req) => {
                let outcome = bluetooth::pair(&req.mac).await;
                let (success, name, error, sink_name) = match outcome {
                    Ok(o) => {
                        persist_paired_device(&req.mac, &o.name, o.sink_name.as_deref());
                        (true, o.name, None, o.sink_name)
                    }
                    Err(e) => {
                        warn!(mac = %req.mac, error = %e, "bluetooth: pairing failed");
                        (false, req.mac.clone(), Some(e), None)
                    }
                };
                let _ = tx
                    .send(MeshMessage::BluetoothPairResult(
                        shared::BluetoothPairResult {
                            node_id: self.node_id.clone(),
                            mac: req.mac,
                            name,
                            success,
                            error,
                            sink_name,
                        },
                    ))
                    .await;
            }
            MeshMessage::BluetoothUnpair(req) => {
                let result = bluetooth::unpair(&req.mac).await;
                let (success, error) = match result {
                    Ok(()) => {
                        if paired_device().is_some_and(|d| d.mac == req.mac) {
                            clear_paired_device();
                        }
                        (true, None)
                    }
                    Err(e) => {
                        warn!(mac = %req.mac, error = %e, "bluetooth: unpair failed");
                        (false, Some(e))
                    }
                };
                let _ = tx
                    .send(MeshMessage::BluetoothUnpairResult(
                        shared::BluetoothUnpairResult {
                            node_id: self.node_id.clone(),
                            mac: req.mac,
                            success,
                            error,
                        },
                    ))
                    .await;
            }
            MeshMessage::BluetoothClearCache(req) => {
                let (cleared, error) = match bluetooth::clear_cache().await {
                    Ok(n) => (Some(n), None),
                    Err(e) => {
                        warn!(request_id = %req.request_id, error = %e, "bluetooth: clear cache failed");
                        (None, Some(e))
                    }
                };
                let _ = tx
                    .send(MeshMessage::BluetoothClearCacheResult(
                        shared::BluetoothClearCacheResult {
                            node_id: self.node_id.clone(),
                            cleared,
                            error,
                        },
                    ))
                    .await;
            }
            _ => {}
        }
    }
}

const BLUETOOTH_STATUS_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);

/// Periodically checks whether this node's currently-paired Bluetooth
/// device is actually connected, pushing a `BluetoothStatusUpdate` only
/// when that state changes — not a heartbeat. BlueZ can't distinguish
/// "powered off" from "out of range/disconnected"; both surface as
/// `connected: false`, worded honestly on the dashboard rather than
/// guessing which.
///
/// Known limitation: this runs once for the agent process's lifetime
/// (`start()` is spawned outside the reconnect loop, per
/// `capability_core::Capability`'s contract), so a coordinator restart that
/// loses its in-memory paired-status map won't be told the current state
/// again until it next actually changes — acceptable for a proposed sketch,
/// not fixed here.
async fn bluetooth_status_loop(node_id: String, tx: Sender<MeshMessage>) {
    let mut last_connected: Option<bool> = None;
    loop {
        match paired_device() {
            Some(device) => {
                let connected = bluetooth::is_connected(&device.mac).await;
                if last_connected != Some(connected) {
                    last_connected = Some(connected);
                    let _ = tx
                        .send(MeshMessage::BluetoothStatusUpdate(
                            shared::BluetoothStatusUpdate {
                                node_id: node_id.clone(),
                                mac: device.mac,
                                name: device.name,
                                connected,
                            },
                        ))
                        .await;
                }
            }
            None => last_connected = None,
        }
        tokio::time::sleep(BLUETOOTH_STATUS_POLL_INTERVAL).await;
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
            std::env::remove_var("AUDIO_CACHE_DIR");
            std::env::remove_var("AUDIO_STATE_DIR");
        }
    }

    /// Points `AUDIO_STATE_DIR` (and so `bluetooth_sink_state_path()`) at a
    /// fresh tempdir so sink-persistence tests can't see a real paired
    /// device's leftover state file, or leak state into other tests.
    fn isolated_state_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: caller holds ENV_LOCK for the duration of the test.
        unsafe {
            std::env::set_var("AUDIO_STATE_DIR", dir.path());
        }
        dir
    }

    #[test]
    fn capability_handles_audio_play_and_bluetooth_messages_when_bluetooth_configured() {
        let _guard = ENV_LOCK.blocking_lock();
        clear_audio_env();
        // configured_backends() defaults to "bluetooth" when AUDIO_BACKENDS
        // is unset, which handles() now consults.
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
        assert!(
            cap.handles(&MeshMessage::BluetoothScan(shared::BluetoothScanRequest {
                request_id: "r2".into(),
                seconds: 30,
            }))
        );
        assert!(
            cap.handles(&MeshMessage::BluetoothPair(shared::BluetoothPairRequest {
                request_id: "r3".into(),
                mac: "AA:BB:CC:DD:EE:FF".into(),
            }))
        );
        assert!(cap.handles(&MeshMessage::BluetoothUnpair(
            shared::BluetoothUnpairRequest {
                request_id: "r4".into(),
                mac: "AA:BB:CC:DD:EE:FF".into(),
            }
        )));
        clear_audio_env();
    }

    #[test]
    fn capability_ignores_bluetooth_messages_on_an_hdmi_only_node() {
        let _guard = ENV_LOCK.blocking_lock();
        clear_audio_env();
        // SAFETY: caller holds ENV_LOCK for the duration of the test.
        unsafe {
            std::env::set_var("AUDIO_BACKENDS", "hdmi");
        }
        let cap = AudioCapability::new("test-node");
        assert!(
            !cap.handles(&MeshMessage::BluetoothScan(shared::BluetoothScanRequest {
                request_id: "r1".into(),
                seconds: 30,
            }))
        );
        assert!(
            !cap.handles(&MeshMessage::BluetoothPair(shared::BluetoothPairRequest {
                request_id: "r2".into(),
                mac: "AA:BB:CC:DD:EE:FF".into(),
            }))
        );
        assert!(!cap.handles(&MeshMessage::BluetoothUnpair(
            shared::BluetoothUnpairRequest {
                request_id: "r3".into(),
                mac: "AA:BB:CC:DD:EE:FF".into(),
            }
        )));
        clear_audio_env();
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
    fn bluetooth_backend_defaults_to_paplay_when_nothing_paired() {
        let _guard = ENV_LOCK.blocking_lock();
        clear_audio_env();
        let _dir = isolated_state_dir();
        assert_eq!(
            play_cmd_template_for("bluetooth"),
            Some("paplay {file}".into())
        );
    }

    #[test]
    fn bluetooth_backend_targets_the_paired_sink_once_persisted() {
        let _guard = ENV_LOCK.blocking_lock();
        clear_audio_env();
        let _dir = isolated_state_dir();
        persist_paired_device(
            "AA:BB:CC:DD:EE:FF",
            "Fishman PA",
            Some("bluez_sink.AA_BB_CC_DD_EE_FF.a2dp_sink"),
        );
        assert_eq!(
            play_cmd_template_for("bluetooth"),
            Some("paplay --device=bluez_sink.AA_BB_CC_DD_EE_FF.a2dp_sink {file}".into())
        );
    }

    #[test]
    fn per_backend_override_still_wins_over_a_persisted_sink() {
        let _guard = ENV_LOCK.blocking_lock();
        clear_audio_env();
        let _dir = isolated_state_dir();
        persist_paired_device(
            "AA:BB:CC:DD:EE:FF",
            "Fishman PA",
            Some("bluez_sink.AA_BB_CC_DD_EE_FF.a2dp_sink"),
        );
        // SAFETY: caller holds ENV_LOCK for the duration of the test.
        unsafe {
            std::env::set_var("AUDIO_PLAY_CMD_BLUETOOTH", "mpv {file}");
        }
        assert_eq!(
            play_cmd_template_for("bluetooth"),
            Some("mpv {file}".into())
        );
        clear_audio_env();
    }

    #[test]
    fn paired_device_round_trips_through_json() {
        let _guard = ENV_LOCK.blocking_lock();
        clear_audio_env();
        let _dir = isolated_state_dir();
        assert!(paired_device().is_none());
        persist_paired_device(
            "AA:BB:CC:DD:EE:FF",
            "Fishman PA",
            Some("bluez_sink.AA_BB_CC_DD_EE_FF.a2dp_sink"),
        );
        let device = paired_device().unwrap();
        assert_eq!(device.mac, "AA:BB:CC:DD:EE:FF");
        assert_eq!(device.name, "Fishman PA");
        assert_eq!(
            device.sink_name.as_deref(),
            Some("bluez_sink.AA_BB_CC_DD_EE_FF.a2dp_sink")
        );
    }

    #[test]
    fn clear_paired_device_removes_persisted_state() {
        let _guard = ENV_LOCK.blocking_lock();
        clear_audio_env();
        let _dir = isolated_state_dir();
        persist_paired_device("AA:BB:CC:DD:EE:FF", "Fishman PA", None);
        assert!(paired_device().is_some());
        clear_paired_device();
        assert!(paired_device().is_none());
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
