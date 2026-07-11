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

use std::sync::{Arc, Mutex};

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
    /// The volume this node's sink was last successfully set to (0-100).
    /// Set at pair time (`bluetooth::DEFAULT_INITIAL_VOLUME_PCT`) and kept
    /// current by `persist_paired_volume` on every successful
    /// `BluetoothVolumeRequest` — there's no way to query a sink's actual
    /// volume back from pactl, so this persisted value is the only source
    /// of truth for "what did we last set it to." `None` only for a device
    /// paired before this field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    volume_pct: Option<u8>,
}

fn write_paired_device(device: &PairedDevice) {
    let path = bluetooth_sink_state_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match serde_json::to_string(device) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&path, json) {
                warn!(error = %e, "audio: failed to persist paired bluetooth device");
            }
        }
        Err(e) => warn!(error = %e, "audio: failed to serialize paired bluetooth device"),
    }
}

/// Persists the outcome of a successful `bluetooth::pair()` so subsequent
/// playback targets the resolved sink explicitly instead of relying on the
/// OS default, and so the device survives an agent restart for unpair/status
/// purposes. Node-local only — survives this node's own restarts, not
/// migrated between nodes.
fn persist_paired_device(mac: &str, name: &str, sink_name: Option<&str>, volume_pct: Option<u8>) {
    write_paired_device(&PairedDevice {
        mac: mac.to_string(),
        name: name.to_string(),
        sink_name: sink_name.map(str::to_string),
        volume_pct,
    });
}

/// Updates just the volume field of the currently-persisted paired device
/// (if any), leaving mac/name/sink_name untouched — called after a
/// successful `BluetoothVolumeRequest` so a later status/pair-result
/// report, or a restarted agent, still knows the last volume actually
/// applied. No-op if nothing is currently persisted as paired.
fn persist_paired_volume(pct: u8) {
    if let Some(mut device) = paired_device() {
        device.volume_pct = Some(pct);
        write_paired_device(&device);
    }
}

fn paired_device() -> Option<PairedDevice> {
    let text = std::fs::read_to_string(bluetooth_sink_state_path()).ok()?;
    match serde_json::from_str(&text) {
        Ok(device) => Some(device),
        Err(e) => {
            // A missing file (nothing ever paired) is normal and handled
            // above via `.ok()?` — this is the file existing but not
            // parsing, which means real corruption, not "unpaired". Worth
            // a trail: silently treating this the same as "unpaired" would
            // otherwise erase a MAC/sink_name the caller might actually
            // need to debug why playback/status suddenly went quiet.
            warn!(error = %e, "audio: persisted paired-device file is corrupt — treating as unpaired");
            None
        }
    }
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

/// Sender for the agent's *current* coordinator connection, shared with the
/// background `bluetooth_status_loop`. `start()` refreshes this on every
/// call (`main.rs` re-runs `start()` on every coordinator reconnect) so a
/// status update pushed after a reconnect uses the live connection, not the
/// first one the loop ever captured.
struct AudioShared {
    mesh_tx: Mutex<Option<Sender<MeshMessage>>>,
}

pub struct AudioCapability {
    node_id: String,
    shared: Arc<AudioShared>,
}

impl AudioCapability {
    pub fn new(node_id: impl Into<String>) -> Self {
        Self {
            node_id: node_id.into(),
            shared: Arc::new(AudioShared {
                mesh_tx: Mutex::new(None),
            }),
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
            | MeshMessage::BluetoothUnpair(_)
            | MeshMessage::BluetoothStatusRequest
            | MeshMessage::BluetoothVolume(_)
            | MeshMessage::BluetoothMute(_) => {
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
        *self.shared.mesh_tx.lock().unwrap() = Some(tx);

        if configured_backends().iter().any(|b| b == "bluetooth") {
            // main.rs's coordinator-reconnect loop calls every capability's
            // start() again on each reconnect. Without this guard, each
            // reconnect would spawn another concurrent bluetooth_status_loop
            // racing the previous ones over the same bluetoothctl calls —
            // the exact class of bug voice's `run()` guard exists to
            // prevent (see its doc comment). The loop only ever needs to
            // start once per process; it reads the live sender out of
            // `shared.mesh_tx` on every send instead of closing over a tx
            // tied to whichever connection happened to start it.
            static STARTED: std::sync::Once = std::sync::Once::new();
            let mut already_started = true;
            STARTED.call_once(|| already_started = false);
            if !already_started {
                let node_id = self.node_id.clone();
                let shared = Arc::clone(&self.shared);
                tokio::spawn(bluetooth_status_loop(node_id, shared));
            }
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
                let (success, name, error, sink_name, volume_pct) = match outcome {
                    Ok(o) => {
                        persist_paired_device(
                            &req.mac,
                            &o.name,
                            o.sink_name.as_deref(),
                            o.volume_pct,
                        );
                        (true, o.name, None, o.sink_name, o.volume_pct)
                    }
                    Err(e) => {
                        warn!(mac = %req.mac, error = %e, "bluetooth: pairing failed");
                        (false, req.mac.clone(), Some(e), None, None)
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
                            volume_pct,
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
            MeshMessage::BluetoothStatusRequest => {
                // Coordinator asked for the current status right now — reply
                // unconditionally, unlike bluetooth_status_loop's own
                // change-gated push (see BluetoothStatusRequest's doc
                // comment for why: a coordinator restart loses its
                // in-memory status without this).
                if let Some(device) = paired_device() {
                    let connected = bluetooth::is_connected(&device.mac).await;
                    let _ = tx
                        .send(MeshMessage::BluetoothStatusUpdate(
                            shared::BluetoothStatusUpdate {
                                node_id: self.node_id.clone(),
                                mac: device.mac,
                                name: device.name,
                                connected,
                            },
                        ))
                        .await;
                }
            }
            MeshMessage::BluetoothVolume(req) => {
                let device = paired_device();
                let (success, error, volume_pct) = match device
                    .as_ref()
                    .and_then(|d| d.sink_name.as_deref())
                {
                    Some(sink) => match bluetooth::set_volume(sink, req.volume_pct).await {
                        Ok(()) => {
                            persist_paired_volume(req.volume_pct);
                            (true, None, Some(req.volume_pct))
                        }
                        Err(e) => {
                            warn!(error = %e, "bluetooth: set volume failed");
                            (false, Some(e), None)
                        }
                    },
                    None => {
                        warn!(
                            mac = %device.as_ref().map(|d| d.mac.as_str()).unwrap_or("none paired"),
                            "bluetooth: volume request but no sink resolved for this node's paired device"
                        );
                        (
                            false,
                            Some("no Bluetooth sink resolved for this node's paired device".into()),
                            None,
                        )
                    }
                };
                let _ = tx
                    .send(MeshMessage::BluetoothVolumeResult(
                        shared::BluetoothVolumeResult {
                            node_id: self.node_id.clone(),
                            success,
                            error,
                            volume_pct,
                        },
                    ))
                    .await;
            }
            MeshMessage::BluetoothMute(req) => {
                let device = paired_device();
                let (success, error) = match device.as_ref().and_then(|d| d.sink_name.as_deref()) {
                    Some(sink) => match bluetooth::set_mute(sink, req.muted).await {
                        Ok(()) => (true, None),
                        Err(e) => {
                            warn!(error = %e, "bluetooth: set mute failed");
                            (false, Some(e))
                        }
                    },
                    None => {
                        warn!(
                            mac = %device.as_ref().map(|d| d.mac.as_str()).unwrap_or("none paired"),
                            "bluetooth: mute request but no sink resolved for this node's paired device"
                        );
                        (
                            false,
                            Some("no Bluetooth sink resolved for this node's paired device".into()),
                        )
                    }
                };
                let _ = tx
                    .send(MeshMessage::BluetoothMuteResult(
                        shared::BluetoothMuteResult {
                            node_id: self.node_id.clone(),
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

/// Minimum gap between auto-reconnect attempts for the same device. Kept
/// long deliberately: this hardware's fragile Bluetooth module wedges
/// after repeated failed connection attempts (confirmed live with the
/// Fishman Loudbox — only a full mains power-cycle recovers it), so a
/// tight retry loop would turn "amp switched off for five minutes" into
/// "amp needs a full manual reset ritual." The first attempt after a
/// device is found disconnected is never delayed — only *subsequent*
/// retries back off.
const BLUETOOTH_RECONNECT_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(120);

/// Periodically checks whether this node's currently-paired Bluetooth
/// device is actually connected, attempting one rate-limited reconnect
/// when it isn't (see `BLUETOOTH_RECONNECT_COOLDOWN`), and pushing a
/// `BluetoothStatusUpdate` only when the connected state actually changes
/// — not a heartbeat. BlueZ can't distinguish "powered off" from "out of
/// range/disconnected"; both surface as `connected: false`, worded
/// honestly on the dashboard rather than guessing which.
///
/// Runs once for the agent process's lifetime — `start()` only spawns it on
/// the first call (see its `std::sync::Once` guard) since a coordinator
/// reconnect doesn't mean the paired device changed. It reads the live
/// coordinator sender out of `shared.mesh_tx` on every send rather than
/// closing over the sender from whichever `start()` call happened to spawn
/// it, so a status push after a reconnect still reaches the coordinator.
///
/// Known limitation: a coordinator restart that loses its in-memory
/// paired-status map won't be told the current state again until it next
/// actually changes — acceptable for a proposed sketch, not fixed here.
async fn bluetooth_status_loop(node_id: String, shared: Arc<AudioShared>) {
    let mut last_connected: Option<bool> = None;
    let mut last_mac: Option<String> = None;
    let mut reconnect_after = tokio::time::Instant::now();
    loop {
        match paired_device() {
            Some(device) => {
                // A freshly-paired or swapped device shouldn't inherit the
                // previous device's reconnect cooldown.
                if last_mac.as_deref() != Some(device.mac.as_str()) {
                    if let Some(previous) = &last_mac {
                        info!(previous_mac = %previous, mac = %device.mac, "bluetooth: paired device changed — reconnect cooldown reset");
                    }
                    reconnect_after = tokio::time::Instant::now();
                    last_mac = Some(device.mac.clone());
                }

                let mut connected = bluetooth::is_connected(&device.mac).await;
                if !connected && tokio::time::Instant::now() >= reconnect_after {
                    reconnect_after = tokio::time::Instant::now() + BLUETOOTH_RECONNECT_COOLDOWN;
                    match bluetooth::reconnect(&device.mac).await {
                        Ok(()) => {
                            info!(mac = %device.mac, "bluetooth: auto-reconnected");
                            connected = true;
                        }
                        Err(e) => {
                            warn!(mac = %device.mac, error = %e, "bluetooth: auto-reconnect attempt failed — backing off");
                        }
                    }
                }

                if last_connected != Some(connected) {
                    last_connected = Some(connected);
                    // Clone the *current* connection's sender out of the
                    // lock; the send itself must not hold the mutex across
                    // an await (same rationale as capability-voice's
                    // identical pattern).
                    let mesh_tx = shared.mesh_tx.lock().unwrap().clone();
                    if let Some(mesh_tx) = mesh_tx {
                        let _ = mesh_tx
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
            }
            None => {
                last_connected = None;
                last_mac = None;
            }
        }
        tokio::time::sleep(BLUETOOTH_STATUS_POLL_INTERVAL).await;
    }
}

async fn play_url(url: &str, sink: Option<&str>) -> Result<(), String> {
    let backend = resolve_backend(sink)?;
    // `paplay --device=<name>` does NOT error when <name> doesn't exist in
    // PipeWire's sink list — confirmed live 2026-07-11: a disconnected
    // Bluetooth speaker drops its sink node entirely, yet `paplay
    // --device=<stale-name>` silently played through the Pi's own onboard
    // default sink and exited 0. That false "success" broke the entire
    // room-routed voice-reply fallback chain (capability-voice's
    // room_with_audio_sink → AudioAnnounce → AudioPlayResult.success):
    // the coordinator believed the reply reached the kitchen speaker, so
    // it never told the voice pipeline to fall back to the puck, and the
    // reply was never heard anywhere. Checking the actual BlueZ connection
    // state first — the same live signal `bluetooth_status_loop` already
    // tracks — makes AudioPlayResult.success mean what callers assume it
    // means.
    if backend == "bluetooth" {
        let device = paired_device()
            .ok_or("bluetooth backend selected but no device is currently paired")?;
        if !bluetooth::is_connected(&device.mac).await {
            return Err(format!(
                "bluetooth device '{}' ({}, sink {}) is not connected — refusing to play \
                 (playback would silently go to this node's default sink instead)",
                device.name,
                device.mac,
                device.sink_name.as_deref().unwrap_or("unresolved")
            ));
        }
    }
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
        assert!(cap.handles(&MeshMessage::BluetoothStatusRequest));
        assert!(cap.handles(&MeshMessage::BluetoothVolume(
            shared::BluetoothVolumeRequest {
                request_id: "r5".into(),
                volume_pct: 50,
            }
        )));
        assert!(
            cap.handles(&MeshMessage::BluetoothMute(shared::BluetoothMuteRequest {
                request_id: "r6".into(),
                muted: true,
            }))
        );
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
        assert!(!cap.handles(&MeshMessage::BluetoothStatusRequest));
        assert!(!cap.handles(&MeshMessage::BluetoothVolume(
            shared::BluetoothVolumeRequest {
                request_id: "r4".into(),
                volume_pct: 50,
            }
        )));
        assert!(
            !cap.handles(&MeshMessage::BluetoothMute(shared::BluetoothMuteRequest {
                request_id: "r5".into(),
                muted: true,
            }))
        );
        clear_audio_env();
    }

    #[tokio::test]
    async fn status_request_replies_with_nothing_when_no_device_paired() {
        let _guard = ENV_LOCK.lock().await;
        clear_audio_env();
        let _dir = isolated_state_dir();
        let cap = AudioCapability::new("test-node");
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        cap.handle(MeshMessage::BluetoothStatusRequest, tx).await;
        assert!(rx.try_recv().is_err());
        clear_audio_env();
    }

    #[tokio::test]
    async fn status_request_replies_with_current_status_when_a_device_is_paired() {
        let _guard = ENV_LOCK.lock().await;
        clear_audio_env();
        let _dir = isolated_state_dir();
        persist_paired_device("AA:BB:CC:DD:EE:FF", "Fishman PA", None, None);
        let cap = AudioCapability::new("test-node");
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        cap.handle(MeshMessage::BluetoothStatusRequest, tx).await;
        match rx.try_recv() {
            Ok(MeshMessage::BluetoothStatusUpdate(update)) => {
                assert_eq!(update.mac, "AA:BB:CC:DD:EE:FF");
                assert_eq!(update.name, "Fishman PA");
                // No real bluetoothctl in this test environment, so
                // is_connected() reports false — the point of this test is
                // that a reply is sent unconditionally, not the value.
                assert!(!update.connected);
            }
            other => panic!("expected BluetoothStatusUpdate, got {other:?}"),
        }
        clear_audio_env();
    }

    #[tokio::test]
    async fn volume_request_fails_when_no_device_paired() {
        let _guard = ENV_LOCK.lock().await;
        clear_audio_env();
        let _dir = isolated_state_dir();
        let cap = AudioCapability::new("test-node");
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        cap.handle(
            MeshMessage::BluetoothVolume(shared::BluetoothVolumeRequest {
                request_id: "r1".into(),
                volume_pct: 50,
            }),
            tx,
        )
        .await;
        match rx.try_recv() {
            Ok(MeshMessage::BluetoothVolumeResult(result)) => {
                assert!(!result.success);
                assert!(result.error.is_some());
            }
            other => panic!("expected BluetoothVolumeResult, got {other:?}"),
        }
        clear_audio_env();
    }

    #[tokio::test]
    async fn volume_request_attempts_pactl_when_a_device_is_paired() {
        let _guard = ENV_LOCK.lock().await;
        clear_audio_env();
        let _dir = isolated_state_dir();
        // No real pactl sink in this test environment — the point of this
        // test is that a sink resolves and pactl is actually invoked (it
        // will fail here, but not with the "no sink resolved" error above).
        persist_paired_device(
            "AA:BB:CC:DD:EE:FF",
            "Fishman PA",
            Some("bluez_sink.test"),
            None,
        );
        let cap = AudioCapability::new("test-node");
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        cap.handle(
            MeshMessage::BluetoothVolume(shared::BluetoothVolumeRequest {
                request_id: "r2".into(),
                volume_pct: 50,
            }),
            tx,
        )
        .await;
        match rx.try_recv() {
            Ok(MeshMessage::BluetoothVolumeResult(result)) => {
                assert_ne!(
                    result.error.as_deref(),
                    Some("no Bluetooth sink resolved for this node's paired device")
                );
            }
            other => panic!("expected BluetoothVolumeResult, got {other:?}"),
        }
        clear_audio_env();
    }

    #[tokio::test]
    async fn mute_request_fails_when_no_device_paired() {
        let _guard = ENV_LOCK.lock().await;
        clear_audio_env();
        let _dir = isolated_state_dir();
        let cap = AudioCapability::new("test-node");
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        cap.handle(
            MeshMessage::BluetoothMute(shared::BluetoothMuteRequest {
                request_id: "r3".into(),
                muted: true,
            }),
            tx,
        )
        .await;
        match rx.try_recv() {
            Ok(MeshMessage::BluetoothMuteResult(result)) => {
                assert!(!result.success);
                assert!(result.error.is_some());
            }
            other => panic!("expected BluetoothMuteResult, got {other:?}"),
        }
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
            None,
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
            None,
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
            None,
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
    fn paired_device_treats_a_corrupt_file_as_unpaired() {
        let _guard = ENV_LOCK.blocking_lock();
        clear_audio_env();
        let _dir = isolated_state_dir();
        std::fs::write(bluetooth_sink_state_path(), b"not valid json").unwrap();
        assert!(paired_device().is_none());
    }

    #[test]
    fn persist_paired_volume_updates_only_the_volume_field() {
        let _guard = ENV_LOCK.blocking_lock();
        clear_audio_env();
        let _dir = isolated_state_dir();
        persist_paired_device(
            "AA:BB:CC:DD:EE:FF",
            "Fishman PA",
            Some("bluez_sink.test"),
            Some(20),
        );
        persist_paired_volume(45);
        let device = paired_device().unwrap();
        assert_eq!(device.mac, "AA:BB:CC:DD:EE:FF");
        assert_eq!(device.name, "Fishman PA");
        assert_eq!(device.sink_name.as_deref(), Some("bluez_sink.test"));
        assert_eq!(device.volume_pct, Some(45));
    }

    #[test]
    fn persist_paired_volume_is_a_noop_when_nothing_paired() {
        let _guard = ENV_LOCK.blocking_lock();
        clear_audio_env();
        let _dir = isolated_state_dir();
        persist_paired_volume(45);
        assert!(paired_device().is_none());
    }

    #[test]
    fn clear_paired_device_removes_persisted_state() {
        let _guard = ENV_LOCK.blocking_lock();
        clear_audio_env();
        let _dir = isolated_state_dir();
        persist_paired_device("AA:BB:CC:DD:EE:FF", "Fishman PA", None, None);
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
    async fn play_url_refuses_a_disconnected_bluetooth_sink() {
        let _guard = ENV_LOCK.lock().await;
        clear_audio_env();
        let _dir = isolated_state_dir();
        // No real bluetoothctl in this test environment, so is_connected()
        // reports false — exactly the real-world "device is off" case this
        // guard exists for (see play_url's doc comment: confirmed live
        // 2026-07-11, paplay silently "succeeded" against a stale sink).
        persist_paired_device(
            "AA:BB:CC:DD:EE:FF",
            "Fishman PA",
            Some("bluez_sink.test"),
            None,
        );
        let err = play_url("http://example/clip.wav", Some("bluetooth"))
            .await
            .unwrap_err();
        assert!(err.contains("not connected"), "unexpected error: {err}");
        clear_audio_env();
    }

    #[tokio::test]
    async fn play_url_refuses_bluetooth_when_nothing_paired() {
        let _guard = ENV_LOCK.lock().await;
        clear_audio_env();
        let _dir = isolated_state_dir();
        let err = play_url("http://example/clip.wav", Some("bluetooth"))
            .await
            .unwrap_err();
        assert!(
            err.contains("no device is currently paired"),
            "unexpected error: {err}"
        );
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
