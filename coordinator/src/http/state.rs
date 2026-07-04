use crate::registry::RoomRecord;
use serde::Serialize;
use shared::MeshMessage;
use shared::hardware::NodeRole;
use shared::messages::{
    LightAction, LightStateReport, NodeRecordLite, ReaperStatusReport, SensorReport,
};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{Notify, broadcast, mpsc, oneshot};
use tracing::warn;

/// Live TCP sender channels keyed by node ID — shared between the TCP server and the HTTP API.
pub type NodeConnections = Arc<Mutex<HashMap<String, mpsc::Sender<MeshMessage>>>>;

/// Pending inference one-shots: request_id → (reply_channel, node_id).
pub type PendingInferences = Arc<Mutex<HashMap<String, (oneshot::Sender<MeshMessage>, String)>>>;

/// Pending intent-level one-shots: request_id → reply_channel.
pub type PendingIntents = Arc<Mutex<HashMap<String, oneshot::Sender<MeshMessage>>>>;

/// In-flight streaming inferences: request_id → (chunk/terminal channel, node_id).
/// The TCP demux feeds N `ModelInferenceChunk`s then one terminal
/// `ModelInferenceResult` (or a `MeshMessage::Error` on node death).
pub type PendingStreams = Arc<Mutex<HashMap<String, (mpsc::Sender<MeshMessage>, String)>>>;

/// Buffered chunks per streaming request. Sized so a briefly-slow SSE client
/// survives; a persistently slow one overflows and its stream is terminated
/// rather than buffering unboundedly on the coordinator.
pub const STREAM_CHANNEL_CAP: usize = 256;

const HEALTH_WINDOW: usize = 60;
const ERROR_RING_CAP: usize = 200;
const SECURITY_RING_CAP: usize = 200;

/// One captured WARN/ERROR log record — surfaced in the Errors tab.
#[derive(Clone, Debug, Serialize)]
pub struct ErrorEntry {
    pub ts_ms: u64,
    /// "WARN" or "ERROR"
    pub level: String,
    pub target: String,
    pub message: String,
}

/// Enum of auditable access events shown in the Security tab.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SecurityEventKind {
    NodeJoin,
    NodeLeave,
    NodeAuthFailed,
    DashboardConnect,
}

/// One security/access event.
#[derive(Clone, Debug, Serialize)]
pub struct SecurityEvent {
    pub ts_ms: u64,
    pub kind: SecurityEventKind,
    pub source: String,
    pub detail: String,
}

/// Events broadcast to all connected dashboard WebSocket clients.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type")]
pub enum DashboardEvent {
    TopologyUpdate {
        nodes: Vec<NodeDashInfo>,
    },
    HealthUpdate {
        node_id: String,
        samples: Vec<HealthSample>,
    },
    ModelUpdate {
        nodes: Vec<NodeModelInfo>,
    },
    LightingUpdate {
        devices: Vec<LightStateReport>,
        #[serde(default)]
        groups: Vec<String>,
    },
    SensorUpdate {
        sensors: Vec<SensorReport>,
    },
    RoomsUpdate {
        rooms: Vec<RoomInfo>,
        #[serde(default)]
        device_names: HashMap<String, String>,
    },
    ScenesUpdate {
        scenes: Vec<SceneInfo>,
    },
    SolarUpdate {
        azimuth: f64,
        elevation: f64,
    },
    EffectUpdate {
        room_id: String,
        effect_id: Option<String>,
        params: serde_json::Value,
        overrides: Vec<String>,
    },
    ZigbeeStatus {
        /// `Some(true)` online, `Some(false)` offline, `None` unknown — the
        /// coordinator has not yet received any `ZigbeeStatus` from a lighting
        /// node (no node connected, or it hasn't reported). Serialises to `null`.
        online: Option<bool>,
    },
    ErrorUpdate {
        errors: Vec<ErrorEntry>,
    },
    SecurityUpdate {
        events: Vec<SecurityEvent>,
    },
    ReaperUpdate {
        online: bool,
        /// 0=stopped, 1=playing, 2=paused, 5=recording
        play_state: u8,
        position: f64,
        tempo: f64,
        ts_num: u32,
        ts_denom: u32,
        /// Most recent command result: (action, ok, message). Drives the tab command log.
        #[serde(skip_serializing_if = "Option::is_none")]
        last_command: Option<(String, bool, String)>,
    },
    GatewayUpdate(GatewaySnapshot),
}

/// Combined gateway config (masked) + cumulative stats. Served by
/// `GET /api/gateway`, returned by `POST /api/gateway`, and broadcast as a
/// `GatewayUpdate` event so the Gateway tab updates live. The API key itself is
/// never included — only `key_set` and a non-revealing `key_hint`.
#[derive(Clone, Debug, Serialize, Default)]
pub struct GatewaySnapshot {
    pub enabled: bool,
    /// Compress history before forwarding (false = pure backend swap).
    pub compress: bool,
    /// Compression engine id: "statistical" | "local_llm_distiller" | "llmlingua2".
    pub engine: String,
    pub selected_model: String,
    pub base_url: String,
    pub key_set: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_hint: Option<String>,
    pub available_models: Vec<String>,
    /// One-click endpoint presets (OpenRouter / Anthropic / Groq / Gemini).
    pub presets: Vec<GatewayPreset>,
    pub calls: u64,
    pub tokens_before: u64,
    pub tokens_after: u64,
    pub tokens_saved: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_call_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

/// A selectable endpoint preset surfaced to the Gateway tab.
#[derive(Clone, Debug, Serialize)]
pub struct GatewayPreset {
    pub id: String,
    pub label: String,
    pub base_url: String,
    pub models: Vec<String>,
}

/// Cumulative, in-memory cloud-gateway usage stats (reset on coordinator restart).
#[derive(Clone, Debug, Default)]
pub struct GatewayStats {
    pub calls: u64,
    pub tokens_before: u64,
    pub tokens_after: u64,
    pub tokens_saved: u64,
    pub last_call_at: Option<i64>,
    pub last_error: Option<String>,
}

/// Snapshot of the currently-active effect (if any) for one room. Pushed to a
/// new WS client on connect alongside the other panel snapshots.
#[derive(Clone, Debug, Serialize)]
pub struct RoomEffectInfo {
    pub room_id: String,
    pub effect_id: Option<String>,
    pub params: serde_json::Value,
    pub overrides: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RoomInfo {
    pub id: String,
    pub name: String,
    pub position: i64,
    pub device_ids: Vec<String>,
    pub orientation_degrees: f32,
    pub has_window: bool,
    pub window_facing: Option<f32>,
    pub width_m: f64,
    pub depth_m: f64,
    pub height_m: f64,
    pub origin_x: f64,
    pub origin_y: f64,
}

impl From<RoomRecord> for RoomInfo {
    fn from(r: RoomRecord) -> Self {
        Self {
            id: r.id,
            name: r.name,
            position: r.position,
            device_ids: r.device_ids,
            orientation_degrees: r.orientation_degrees,
            has_window: r.has_window,
            window_facing: r.window_facing,
            width_m: r.width_m,
            depth_m: r.depth_m,
            height_m: r.height_m,
            origin_x: r.origin_x,
            origin_y: r.origin_y,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct SceneInfo {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub room_id: Option<String>,
    pub created_at: i64,
    pub position: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview_color: Option<[f32; 2]>,
}

/// Per-model entry in a `ModelUpdate` event — one per non-Unloaded allocation.
#[derive(Clone, Debug, Serialize)]
pub struct ModelEntry {
    pub name: String,
    pub size_mb: u64,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Per-node model summary — one per Compute node in a `ModelUpdate` event.
#[derive(Clone, Debug, Serialize)]
pub struct NodeModelInfo {
    pub node_id: String,
    pub hostname: String,
    pub role: String,
    /// Static RAM ceiling from HardwareSpec; 0.0 if hardware not yet reported.
    pub ram_gb: f32,
    pub models: Vec<ModelEntry>,
}

/// One health data point, coordinator-stamped.
#[derive(Clone, Debug, Serialize)]
pub struct HealthSample {
    /// Unix timestamp in milliseconds, set by the coordinator on receipt.
    pub ts_ms: u64,
    pub cpu_pct: f32,
    pub ram_used_gb: f32,
    pub ram_total_gb: f32,
    /// GPU utilisation 0.0–100.0; None on CPU-only nodes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gpu_pct: Option<f32>,
    /// GPU VRAM in use, gibibytes; None on CPU-only nodes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gpu_vram_used_gb: Option<f32>,
    /// Total GPU VRAM, gibibytes; None on CPU-only nodes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gpu_vram_total_gb: Option<f32>,
    /// Free space on the model storage filesystem, gibibytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk_free_gb: Option<f32>,
}

#[derive(Clone, Debug, Serialize)]
pub struct NodeDashInfo {
    pub id: String,
    pub name: String,
    pub role: String,
    pub ip: String,
    /// Age of last heartbeat in whole seconds.
    pub last_seen_secs: u64,
    /// "green" (<10 s), "amber" (10–30 s), "red" (>30 s).
    pub health: &'static str,
}

pub struct DashboardState {
    pub tx: broadcast::Sender<DashboardEvent>,
    /// Valid auth tokens — mirrors the mesh server's token list. Empty = no auth (dev mode).
    pub auth_tokens: Arc<Vec<String>>,
    health_store: Mutex<HashMap<String, VecDeque<HealthSample>>>,
    model_snapshot: Mutex<Vec<NodeModelInfo>>,
    light_snapshot: Mutex<HashMap<String, LightStateReport>>,
    /// Latest merged readings per sensor device (see `push_sensor_update`).
    sensor_snapshot: Mutex<HashMap<String, SensorReport>>,
    /// Group friendly name → node_id that owns it.
    group_snapshot: Mutex<HashMap<String, String>>,
    room_snapshot: Mutex<Vec<RoomInfo>>,
    scene_snapshot: Mutex<Vec<SceneInfo>>,
    /// Per-room currently-active effect (if any). Mirrors `room_effects` rows
    /// where `enabled = 1`. Pushed to new WS clients on connect.
    effect_snapshot: Mutex<HashMap<String, RoomEffectInfo>>,
    /// Last-known zigbee bridge status: `None` until a lighting node first
    /// reports (unknown — don't claim a bridge is healthy we've never heard
    /// from), then `Some(online)`. `ZigbeeStatus` is otherwise only broadcast on
    /// change, so this is replayed to new WS clients on connect.
    zigbee_status: Mutex<Option<bool>>,
    /// Live TCP sender channels — used by the HTTP API to push commands to agents.
    pub connections: NodeConnections,
    /// Wakes the EffectRunner for an immediate tick — used by activation /
    /// deactivation paths and any UX that needs effect output to reflect a
    /// state change without waiting for the next scheduled tick.
    pub solar_sweep_notify: Arc<Notify>,
    /// Location used by the JS solar calculator (served via GET /api/solar/config).
    pub lat: f64,
    pub lon: f64,
    error_log: Mutex<VecDeque<ErrorEntry>>,
    security_log: Mutex<VecDeque<SecurityEvent>>,
    /// Shared with the TCP server so the HTTP chat endpoint can dispatch inference requests.
    pub pending_inferences: PendingInferences,
    /// Shared with the TCP server so tool calls (scene_load) can wait for a reply.
    pub pending_intents: PendingIntents,
    /// Shared with the TCP server so streamed inference chunks reach the SSE emitter.
    pub pending_streams: PendingStreams,
    /// Last-known REAPER status — replayed to new WS clients on connect.
    reaper_snapshot: Mutex<Option<ReaperStatusReport>>,
    /// Cumulative cloud-gateway usage stats (in-memory; reset on restart).
    gateway_stats: Mutex<GatewayStats>,
}

impl DashboardState {
    pub fn new(auth_tokens: Arc<Vec<String>>, connections: NodeConnections) -> Arc<Self> {
        let lat = std::env::var("MESH_LATITUDE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(51.5074);
        let lon = std::env::var("MESH_LONGITUDE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(-0.1278);
        let (tx, _) = broadcast::channel(128);
        Arc::new(Self {
            tx,
            auth_tokens,
            health_store: Mutex::new(HashMap::new()),
            model_snapshot: Mutex::new(Vec::new()),
            light_snapshot: Mutex::new(HashMap::new()),
            sensor_snapshot: Mutex::new(HashMap::new()),
            group_snapshot: Mutex::new(HashMap::new()),
            room_snapshot: Mutex::new(Vec::new()),
            scene_snapshot: Mutex::new(Vec::new()),
            effect_snapshot: Mutex::new(HashMap::new()),
            zigbee_status: Mutex::new(None),
            connections,
            solar_sweep_notify: Arc::new(Notify::new()),
            lat,
            lon,
            error_log: Mutex::new(VecDeque::new()),
            security_log: Mutex::new(VecDeque::new()),
            pending_inferences: Arc::new(Mutex::new(HashMap::new())),
            pending_intents: Arc::new(Mutex::new(HashMap::new())),
            pending_streams: Arc::new(Mutex::new(HashMap::new())),
            reaper_snapshot: Mutex::new(None),
            gateway_stats: Mutex::new(GatewayStats::default()),
        })
    }

    /// Record a successful cloud-gateway call and its context token before/after
    /// counts. Clears any prior error. Does not broadcast — the caller assembles
    /// and broadcasts a full `GatewaySnapshot` afterwards.
    pub fn record_gateway_call(&self, tokens_before: u64, tokens_after: u64) {
        let mut s = self.gateway_stats.lock().unwrap();
        s.calls += 1;
        s.tokens_before += tokens_before;
        s.tokens_after += tokens_after;
        s.tokens_saved += tokens_before.saturating_sub(tokens_after);
        s.last_call_at = Some(chrono::Utc::now().timestamp());
        s.last_error = None;
    }

    /// Record a cloud-gateway failure (the request still falls back to local).
    pub fn record_gateway_error(&self, msg: String) {
        let mut s = self.gateway_stats.lock().unwrap();
        s.last_call_at = Some(chrono::Utc::now().timestamp());
        s.last_error = Some(msg);
    }

    /// Snapshot of cumulative gateway stats.
    pub fn get_gateway_stats(&self) -> GatewayStats {
        self.gateway_stats.lock().unwrap().clone()
    }

    /// Broadcast a gateway snapshot to connected dashboards (no-op if none).
    pub fn push_gateway_update(&self, snapshot: GatewaySnapshot) {
        if self.tx.receiver_count() > 0 {
            let _ = self.tx.send(DashboardEvent::GatewayUpdate(snapshot));
        }
    }

    /// Send `msg` to the named node's open TCP channel.
    /// Returns `true` if the message was queued, `false` if the node is not connected.
    pub fn send_to_node(&self, node_id: &str, msg: MeshMessage) -> bool {
        let guard = self.connections.lock().unwrap();
        match guard.get(node_id) {
            Some(tx) => match tx.try_send(msg) {
                Ok(()) => true,
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                    warn!(node_id, "send_to_node: channel full, message dropped");
                    false
                }
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => false,
            },
            None => false,
        }
    }

    /// Returns true when the supplied token is acceptable.
    pub fn auth_ok(&self, token: &str) -> bool {
        self.auth_tokens.is_empty() || self.auth_tokens.iter().any(|t| t == token)
    }

    /// Return a snapshot of all stored health windows — used to warm-start new WS clients.
    pub fn get_all_health_snapshots(&self) -> Vec<(String, Vec<HealthSample>)> {
        let store = self.health_store.lock().unwrap();
        store
            .iter()
            .map(|(id, deque)| (id.clone(), deque.iter().cloned().collect()))
            .collect()
    }

    /// Store and broadcast a model snapshot. Always stores (for snapshot-on-connect);
    /// broadcasts only when at least one WS client is connected.
    pub fn push_model_update(&self, nodes: Vec<NodeModelInfo>) {
        *self.model_snapshot.lock().unwrap() = nodes.clone();
        if self.tx.receiver_count() > 0 {
            let _ = self.tx.send(DashboardEvent::ModelUpdate { nodes });
        }
    }

    /// Return a point-in-time copy of the model snapshot for warm-starting new WS clients.
    pub fn get_model_snapshot(&self) -> Vec<NodeModelInfo> {
        self.model_snapshot.lock().unwrap().clone()
    }

    /// Store the latest state for one device and broadcast a LightingUpdate with all devices + groups.
    ///
    /// Availability-only reports (online=false, all payload fields null) only flip the `online`
    /// flag on the existing record to avoid clobbering brightness/colour with nulls.
    pub fn push_lighting_update(&self, report: LightStateReport) {
        // A Zigbee group publishes state on its base topic exactly like a device.
        // Never let a known group masquerade as a device in the snapshot.
        if self.is_known_group(&report.device_id) {
            return;
        }
        let devices = {
            let mut snap = self.light_snapshot.lock().unwrap();
            let availability_only = !report.online
                && report.brightness.is_none()
                && report.color_xy.is_none()
                && report.color_temp.is_none();
            if availability_only {
                if let Some(existing) = snap.get_mut(&report.device_id) {
                    // Only flip `online`; leave `on`, brightness, and colour
                    // untouched so the card retains its last known state while
                    // the device is unreachable.
                    existing.online = false;
                } else {
                    snap.insert(report.device_id.clone(), report);
                }
            } else {
                snap.insert(report.device_id.clone(), report);
            }
            snap.values().cloned().collect::<Vec<_>>()
        };
        if self.tx.receiver_count() > 0 {
            let groups = self.get_group_snapshot();
            let _ = self
                .tx
                .send(DashboardEvent::LightingUpdate { devices, groups });
        }
    }

    /// Merge one sensor's readings into the snapshot and broadcast a SensorUpdate
    /// with all sensors. Returns the merged record so the caller can persist it.
    ///
    /// Merge is field-wise: `Some` overwrites, `None` keeps the stored value.
    /// Sensors publish partial payloads (battery arrives rarely; an
    /// availability flip carries no readings at all), so a raw insert would
    /// wipe last-known readings on every partial publish.
    pub fn push_sensor_update(&self, report: SensorReport) -> SensorReport {
        let (merged, sensors) = {
            let mut snap = self.sensor_snapshot.lock().unwrap();
            let merged = match snap.get(&report.device_id) {
                Some(existing) => SensorReport {
                    node_id: report.node_id,
                    device_id: report.device_id,
                    temperature: report.temperature.or(existing.temperature),
                    humidity: report.humidity.or(existing.humidity),
                    battery: report.battery.or(existing.battery),
                    occupancy: report.occupancy.or(existing.occupancy),
                    contact: report.contact.or(existing.contact),
                    online: report.online,
                },
                None => report,
            };
            snap.insert(merged.device_id.clone(), merged.clone());
            (merged, snap.values().cloned().collect::<Vec<_>>())
        };
        if self.tx.receiver_count() > 0 {
            let _ = self.tx.send(DashboardEvent::SensorUpdate { sensors });
        }
        merged
    }

    /// Point-in-time copy of the sensor snapshot for warm-starting new WS clients.
    pub fn get_sensor_snapshot(&self) -> Vec<SensorReport> {
        self.sensor_snapshot
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect()
    }

    /// Add placeholder entries for discovered devices that haven't yet reported state.
    /// Broadcasts a LightingUpdate if any new placeholders were added and `emit` is true.
    pub fn push_device_discovery(&self, node_id: &str, devices: Vec<String>, emit: bool) {
        // Snapshot the group set first (separate lock, released before we take
        // the light-snapshot lock) so a group name never gets a device placeholder.
        let groups: HashSet<String> = self
            .group_snapshot
            .lock()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        let mut updated = false;
        {
            let mut snap = self.light_snapshot.lock().unwrap();
            for id in devices {
                if groups.contains(&id) {
                    continue;
                }
                if !snap.contains_key(&id) {
                    snap.insert(
                        id.clone(),
                        LightStateReport {
                            node_id: node_id.to_owned(),
                            device_id: id,
                            on: false,
                            brightness: Some(254),
                            color_xy: None,
                            color_temp: Some(370),
                            online: false, // unknown until first state report arrives
                        },
                    );
                    updated = true;
                }
            }
        }
        if emit && updated && self.tx.receiver_count() > 0 {
            let devices = self.get_light_snapshot();
            let groups = self.get_group_snapshot();
            let _ = self
                .tx
                .send(DashboardEvent::LightingUpdate { devices, groups });
        }
    }

    /// Apply an outbound command's intended effect to the in-memory light snapshot.
    ///
    /// Subsequent broadcasts (triggered by any device's later LightState report)
    /// will then carry the user's intended value rather than the pre-command
    /// value, preventing UI sliders from snapping back when another device's
    /// status report races ahead of the bulb's own confirmation.
    pub fn apply_command_to_snapshot(&self, device_id: &str, action: &LightAction) {
        let mut snap = self.light_snapshot.lock().unwrap();
        let Some(entry) = snap.get_mut(device_id) else {
            return;
        };
        match action {
            LightAction::On => entry.on = true,
            LightAction::Off => entry.on = false,
            LightAction::Toggle => entry.on = !entry.on,
            LightAction::Brightness(b) => {
                entry.brightness = Some(*b);
                entry.on = true;
            }
            LightAction::BrightnessTransition { value, .. } => {
                entry.brightness = Some(*value);
                entry.on = true;
            }
            LightAction::ColorTemp(ct) => entry.color_temp = Some(*ct),
            LightAction::ColorTempTransition { value, .. } => entry.color_temp = Some(*value),
            LightAction::ColorXY { x, y } => entry.color_xy = Some((*x, *y)),
            LightAction::ColorXYTransition { x, y, .. } => entry.color_xy = Some((*x, *y)),
        }
    }

    /// Store groups for a node and broadcast a LightingUpdate with all devices + new groups.
    pub fn push_group_update(&self, node_id: &str, groups: Vec<String>) {
        let group_names: HashSet<String> = {
            let mut snap = self.group_snapshot.lock().unwrap();
            snap.retain(|_, v| v != node_id);
            for g in groups {
                snap.insert(g, node_id.to_owned());
            }
            snap.keys().cloned().collect()
        };
        // Self-heal: a group whose retained state was ingested as a device before
        // this list arrived now resolves to a group — drop any such device entry.
        {
            let mut snap = self.light_snapshot.lock().unwrap();
            snap.retain(|id, _| !group_names.contains(id));
        }
        if self.tx.receiver_count() > 0 {
            let devices = self.get_light_snapshot();
            let groups = self.get_group_snapshot();
            let _ = self
                .tx
                .send(DashboardEvent::LightingUpdate { devices, groups });
        }
    }

    /// Return all known light device states — used to warm-start new WS clients.
    pub fn get_light_snapshot(&self) -> Vec<LightStateReport> {
        self.light_snapshot
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect()
    }

    /// Return all known group friendly names — used to warm-start new WS clients.
    pub fn get_group_snapshot(&self) -> Vec<String> {
        self.group_snapshot
            .lock()
            .unwrap()
            .keys()
            .cloned()
            .collect()
    }

    /// Return the node_id that last reported state for a given device — used to route commands.
    pub fn get_node_for_device(&self, device_id: &str) -> Option<String> {
        self.light_snapshot
            .lock()
            .unwrap()
            .get(device_id)
            .map(|r| r.node_id.clone())
    }

    /// Return the node_id responsible for a given group — used to route group commands.
    pub fn get_node_for_group(&self, name: &str) -> Option<String> {
        self.group_snapshot.lock().unwrap().get(name).cloned()
    }

    /// True if `name` is currently a known Zigbee group (not a device). Used to
    /// stop a group's base-topic state report from masquerading as a device.
    pub fn is_known_group(&self, name: &str) -> bool {
        self.group_snapshot.lock().unwrap().contains_key(name)
    }

    /// Store and broadcast the current rooms state.
    pub fn push_rooms_update(&self, rooms: Vec<RoomInfo>) {
        self.push_rooms_update_with_names(rooms, HashMap::new());
    }

    pub fn push_rooms_update_with_names(
        &self,
        rooms: Vec<RoomInfo>,
        device_names: HashMap<String, String>,
    ) {
        *self.room_snapshot.lock().unwrap() = rooms.clone();
        if self.tx.receiver_count() > 0 {
            let _ = self.tx.send(DashboardEvent::RoomsUpdate {
                rooms,
                device_names,
            });
        }
    }

    /// Return a point-in-time copy of the rooms snapshot for warm-starting new WS clients.
    pub fn get_room_snapshot(&self) -> Vec<RoomInfo> {
        self.room_snapshot.lock().unwrap().clone()
    }

    /// Store and broadcast the current scenes state.
    pub fn push_scenes_update(&self, scenes: Vec<SceneInfo>) {
        *self.scene_snapshot.lock().unwrap() = scenes.clone();
        if self.tx.receiver_count() > 0 {
            let _ = self.tx.send(DashboardEvent::ScenesUpdate { scenes });
        }
    }

    /// Return a point-in-time copy of the scenes snapshot for warm-starting new WS clients.
    pub fn get_scene_snapshot(&self) -> Vec<SceneInfo> {
        self.scene_snapshot.lock().unwrap().clone()
    }

    /// Update the per-room effect snapshot and broadcast `EffectUpdate`.
    /// Pass `effect_id = None` to clear the room's active effect.
    pub fn push_effect_update(
        &self,
        room_id: String,
        effect_id: Option<String>,
        params: serde_json::Value,
        overrides: Vec<String>,
    ) {
        {
            let mut snap = self.effect_snapshot.lock().unwrap();
            if effect_id.is_none() {
                snap.remove(&room_id);
            } else {
                snap.insert(
                    room_id.clone(),
                    RoomEffectInfo {
                        room_id: room_id.clone(),
                        effect_id: effect_id.clone(),
                        params: params.clone(),
                        overrides: overrides.clone(),
                    },
                );
            }
        }
        if self.tx.receiver_count() > 0 {
            let _ = self.tx.send(DashboardEvent::EffectUpdate {
                room_id,
                effect_id,
                params,
                overrides,
            });
        }
    }

    /// Snapshot of every room with an active effect — used by `ws.rs` to
    /// hydrate a new dashboard client.
    pub fn get_effect_snapshot(&self) -> Vec<RoomEffectInfo> {
        self.effect_snapshot
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect()
    }

    /// Broadcast zigbee bridge up/down status to all connected clients, and
    /// remember it so a dashboard connecting later (while still down) is told.
    pub fn push_zigbee_status(&self, online: bool) {
        *self.zigbee_status.lock().unwrap() = Some(online);
        let _ = self.tx.send(DashboardEvent::ZigbeeStatus {
            online: Some(online),
        });
    }

    /// Reset the zigbee bridge status to unknown (`None`) and tell clients. Called
    /// when the lighting node disconnects: we've lost our source of bridge truth,
    /// so we must not keep showing a stale "online" the node can no longer refute.
    pub fn reset_zigbee_status(&self) {
        *self.zigbee_status.lock().unwrap() = None;
        let _ = self.tx.send(DashboardEvent::ZigbeeStatus { online: None });
    }

    /// Last-known zigbee bridge status — used by `ws.rs` to hydrate a new client.
    /// `None` means no lighting node has reported yet (unknown).
    pub fn get_zigbee_status(&self) -> Option<bool> {
        *self.zigbee_status.lock().unwrap()
    }

    /// Broadcast the current solar position to all connected clients.
    pub fn push_solar_update(&self, azimuth: f64, elevation: f64) {
        if self.tx.receiver_count() > 0 {
            let _ = self
                .tx
                .send(DashboardEvent::SolarUpdate { azimuth, elevation });
        }
    }

    /// Build and broadcast a TopologyUpdate from a fresh node list.
    pub fn push_topology(&self, nodes: &[NodeRecordLite]) {
        // No receivers — nothing to do.
        if self.tx.receiver_count() == 0 {
            return;
        }
        let evt = DashboardEvent::TopologyUpdate {
            nodes: nodes.iter().map(node_dash_info).collect(),
        };
        let _ = self.tx.send(evt);
    }

    /// Append an error entry to the ring buffer and broadcast to connected clients.
    pub fn push_error(&self, entry: ErrorEntry) {
        let errors = {
            let mut log = self.error_log.lock().unwrap();
            log.push_back(entry);
            if log.len() > ERROR_RING_CAP {
                log.pop_front();
            }
            log.iter().rev().take(50).cloned().collect::<Vec<_>>()
        };
        if self.tx.receiver_count() > 0 {
            let _ = self.tx.send(DashboardEvent::ErrorUpdate { errors });
        }
    }

    /// Return the most recent error entries (newest first) for warm-starting new WS clients.
    pub fn get_error_snapshot(&self) -> Vec<ErrorEntry> {
        self.error_log
            .lock()
            .unwrap()
            .iter()
            .rev()
            .take(50)
            .cloned()
            .collect()
    }

    /// Append a security event to the ring buffer and broadcast to connected clients.
    pub fn push_security(&self, event: SecurityEvent) {
        let events = {
            let mut log = self.security_log.lock().unwrap();
            log.push_back(event);
            if log.len() > SECURITY_RING_CAP {
                log.pop_front();
            }
            log.iter().rev().take(50).cloned().collect::<Vec<_>>()
        };
        if self.tx.receiver_count() > 0 {
            let _ = self.tx.send(DashboardEvent::SecurityUpdate { events });
        }
    }

    /// Return the most recent security events (newest first) for warm-starting new WS clients.
    pub fn get_security_snapshot(&self) -> Vec<SecurityEvent> {
        self.security_log
            .lock()
            .unwrap()
            .iter()
            .rev()
            .take(50)
            .cloned()
            .collect()
    }

    /// Record a health sample for `node_id`, capped at HEALTH_WINDOW entries,
    /// then broadcast a HealthUpdate with the full window to connected clients.
    #[allow(clippy::too_many_arguments)]
    pub fn push_health(
        &self,
        node_id: &str,
        cpu_pct: f32,
        ram_used_gb: f32,
        ram_total_gb: f32,
        gpu_pct: Option<f32>,
        gpu_vram_used_gb: Option<f32>,
        gpu_vram_total_gb: Option<f32>,
        disk_free_gb: Option<f32>,
    ) {
        let ts_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let sample = HealthSample {
            ts_ms,
            cpu_pct,
            ram_used_gb,
            ram_total_gb,
            gpu_pct,
            gpu_vram_used_gb,
            gpu_vram_total_gb,
            disk_free_gb,
        };
        let samples = {
            let mut store = self.health_store.lock().unwrap();
            let window = store.entry(node_id.to_owned()).or_default();
            window.push_back(sample);
            if window.len() > HEALTH_WINDOW {
                window.pop_front();
            }
            window.iter().cloned().collect::<Vec<_>>()
        };
        if self.tx.receiver_count() == 0 {
            return;
        }
        let _ = self.tx.send(DashboardEvent::HealthUpdate {
            node_id: node_id.to_owned(),
            samples,
        });
    }

    /// Update the REAPER snapshot and broadcast if status changed.
    /// Always updates the snapshot (even when offline) so stale data is never frozen.
    pub fn push_reaper_status(&self, report: ReaperStatusReport) {
        let evt = {
            let mut snap = self.reaper_snapshot.lock().unwrap();
            let changed = snap.as_ref().is_none_or(|prev| {
                prev.reaper_online != report.reaper_online
                    || prev.play_state != report.play_state
                    || (prev.position - report.position).abs() >= 0.1
            });
            *snap = Some(report.clone());
            if !changed || self.tx.receiver_count() == 0 {
                return;
            }
            DashboardEvent::ReaperUpdate {
                online: report.reaper_online,
                play_state: report.play_state,
                position: report.position,
                tempo: report.tempo,
                ts_num: report.ts_num,
                ts_denom: report.ts_denom,
                last_command: None,
            }
        };
        let _ = self.tx.send(evt);
    }

    /// Broadcast a ReaperUpdate carrying a command result alongside the current snapshot.
    pub fn push_reaper_command_result(&self, action: String, ok: bool, message: String) {
        if self.tx.receiver_count() == 0 {
            return;
        }
        let snap = self.reaper_snapshot.lock().unwrap();
        let (online, play_state, position, tempo, ts_num, ts_denom) =
            snap.as_ref().map_or((false, 0, 0.0, 0.0, 0, 0), |s| {
                (
                    s.reaper_online,
                    s.play_state,
                    s.position,
                    s.tempo,
                    s.ts_num,
                    s.ts_denom,
                )
            });
        drop(snap);
        let _ = self.tx.send(DashboardEvent::ReaperUpdate {
            online,
            play_state,
            position,
            tempo,
            ts_num,
            ts_denom,
            last_command: Some((action, ok, message)),
        });
    }

    /// Return the current REAPER snapshot for WS warm-start and REST responses.
    pub fn get_reaper_snapshot(&self) -> Option<ReaperStatusReport> {
        self.reaper_snapshot.lock().unwrap().clone()
    }
}

fn node_dash_info(n: &NodeRecordLite) -> NodeDashInfo {
    let age_secs = (n.last_heartbeat_ms / 1000) as u64;
    let health = if age_secs < 10 {
        "green"
    } else if age_secs < 30 {
        "amber"
    } else {
        "red"
    };
    NodeDashInfo {
        id: n.id.clone(),
        name: n.hostname.clone(),
        role: role_label(&n.role),
        ip: n.ip.clone(),
        last_seen_secs: age_secs,
        health,
    }
}

fn role_label(role: &NodeRole) -> String {
    match role {
        NodeRole::Compute => "Compute".into(),
        NodeRole::Controller => "Controller".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::messages::NodeRecordLite;

    fn lite(last_heartbeat_ms: u128) -> NodeRecordLite {
        NodeRecordLite {
            id: "test-id".into(),
            hostname: "testhost".into(),
            ip: "10.0.0.1".into(),
            role: NodeRole::Compute,
            last_heartbeat_ms,
        }
    }

    // ── auth_ok ───────────────────────────────────────────────────────────────

    #[test]
    fn auth_ok_dev_mode_accepts_any_token() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        assert!(state.auth_ok(""));
        assert!(state.auth_ok("any-token"));
        assert!(state.auth_ok("completely-random"));
    }

    #[test]
    fn auth_ok_accepts_matching_token() {
        let state = DashboardState::new(
            Arc::new(vec!["secret".into()]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        assert!(state.auth_ok("secret"));
    }

    #[test]
    fn auth_ok_rejects_wrong_and_empty_token() {
        let state = DashboardState::new(
            Arc::new(vec!["secret".into()]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        assert!(!state.auth_ok("wrong"));
        assert!(!state.auth_ok(""));
    }

    #[test]
    fn auth_ok_accepts_any_configured_token() {
        let state = DashboardState::new(
            Arc::new(vec!["alpha".into(), "beta".into()]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        assert!(state.auth_ok("alpha"));
        assert!(state.auth_ok("beta"));
        assert!(!state.auth_ok("gamma"));
    }

    // ── health colour thresholds ──────────────────────────────────────────────

    #[test]
    fn health_green_under_10s() {
        let info = node_dash_info(&lite(5_000)); // 5 s
        assert_eq!(info.health, "green");
        assert_eq!(info.last_seen_secs, 5);
    }

    #[test]
    fn health_amber_10_to_29s() {
        let info = node_dash_info(&lite(10_000)); // 10 s — boundary
        assert_eq!(info.health, "amber");
        let info2 = node_dash_info(&lite(29_000)); // 29 s
        assert_eq!(info2.health, "amber");
    }

    #[test]
    fn health_red_at_30s_and_above() {
        let info = node_dash_info(&lite(30_000)); // exactly 30 s
        assert_eq!(info.health, "red");
        let info2 = node_dash_info(&lite(120_000)); // 2 min
        assert_eq!(info2.health, "red");
    }

    // ── push_topology ─────────────────────────────────────────────────────────

    #[test]
    fn push_topology_with_no_receivers_is_noop() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        // No panic, no side-effects — just verifies the early-return path.
        state.push_topology(&[]);
        state.push_topology(&[lite(1_000)]);
    }

    // ── push_health ───────────────────────────────────────────────────────────

    #[test]
    fn push_health_broadcasts_health_update() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        let mut rx = state.tx.subscribe();
        state.push_health("n1", 42.5, 6.1, 15.9, None, None, None, None);
        let evt = rx.try_recv().unwrap();
        match evt {
            DashboardEvent::HealthUpdate { node_id, samples } => {
                assert_eq!(node_id, "n1");
                assert_eq!(samples.len(), 1);
                assert_eq!(samples[0].cpu_pct, 42.5);
                assert_eq!(samples[0].ram_used_gb, 6.1);
                assert_eq!(samples[0].ram_total_gb, 15.9);
                assert!(samples[0].ts_ms > 0);
            }
            _ => panic!("expected HealthUpdate"),
        }
    }

    #[test]
    fn push_health_accumulates_samples() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        let mut rx = state.tx.subscribe();
        state.push_health("n1", 10.0, 1.0, 16.0, None, None, None, None);
        state.push_health("n1", 20.0, 2.0, 16.0, None, None, None, None);
        state.push_health("n1", 30.0, 3.0, 16.0, None, None, None, None);
        // Drain; last event has all 3 samples.
        let mut last = None;
        while let Ok(e) = rx.try_recv() {
            last = Some(e);
        }
        match last.unwrap() {
            DashboardEvent::HealthUpdate { samples, .. } => assert_eq!(samples.len(), 3),
            _ => panic!("expected HealthUpdate"),
        }
    }

    #[test]
    fn push_health_caps_window_at_60() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        let mut rx = state.tx.subscribe();
        for i in 0..=60u32 {
            state.push_health("n1", i as f32, 0.0, 16.0, None, None, None, None);
        }
        let mut last = None;
        while let Ok(e) = rx.try_recv() {
            last = Some(e);
        }
        match last.unwrap() {
            DashboardEvent::HealthUpdate { samples, .. } => {
                assert_eq!(samples.len(), 60);
                // cpu_pct 0.0 was the first (evicted); 1.0 is now oldest
                assert_eq!(samples[0].cpu_pct, 1.0);
                assert_eq!(samples[59].cpu_pct, 60.0);
            }
            _ => panic!("expected HealthUpdate"),
        }
    }

    #[test]
    fn push_health_independent_per_node() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        let mut rx = state.tx.subscribe();
        state.push_health("n1", 10.0, 1.0, 8.0, None, None, None, None);
        state.push_health("n2", 50.0, 4.0, 8.0, None, None, None, None);
        state.push_health("n1", 15.0, 1.5, 8.0, None, None, None, None);
        let mut events: Vec<DashboardEvent> = vec![];
        while let Ok(e) = rx.try_recv() {
            events.push(e);
        }
        // n1 was pushed twice; its last event has 2 samples
        let n1_last = events
            .iter()
            .filter_map(|e| match e {
                DashboardEvent::HealthUpdate { node_id, samples } if node_id == "n1" => {
                    Some(samples.len())
                }
                _ => None,
            })
            .next_back()
            .unwrap();
        assert_eq!(n1_last, 2);
    }

    #[test]
    fn push_health_with_no_receivers_is_noop() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        // No panic — store still updates, broadcast skipped
        state.push_health("n1", 0.0, 0.0, 0.0, None, None, None, None);
    }

    #[test]
    fn health_sample_serializes_expected_fields() {
        let s = HealthSample {
            ts_ms: 1_000_000,
            cpu_pct: 33.3,
            ram_used_gb: 4.0,
            ram_total_gb: 16.0,
            gpu_pct: None,
            gpu_vram_used_gb: None,
            gpu_vram_total_gb: None,
            disk_free_gb: None,
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"ts_ms\""));
        assert!(json.contains("\"cpu_pct\""));
        assert!(json.contains("\"ram_used_gb\""));
        assert!(json.contains("\"ram_total_gb\""));
    }

    #[test]
    fn health_sample_gpu_fields_omitted_when_none() {
        let s = HealthSample {
            ts_ms: 1_000,
            cpu_pct: 10.0,
            ram_used_gb: 1.0,
            ram_total_gb: 8.0,
            gpu_pct: None,
            gpu_vram_used_gb: None,
            gpu_vram_total_gb: None,
            disk_free_gb: None,
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(
            !json.contains("gpu_pct"),
            "None gpu_pct should be absent: {json}"
        );
        assert!(
            !json.contains("gpu_vram"),
            "None gpu_vram fields should be absent: {json}"
        );
    }

    #[test]
    fn health_sample_gpu_fields_present_when_some() {
        let s = HealthSample {
            ts_ms: 1_000,
            cpu_pct: 10.0,
            ram_used_gb: 1.0,
            ram_total_gb: 8.0,
            gpu_pct: Some(42.0),
            gpu_vram_used_gb: Some(3.7),
            gpu_vram_total_gb: Some(4.0),
            disk_free_gb: None,
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"gpu_pct\":42.0"), "gpu_pct missing: {json}");
        assert!(
            json.contains("\"gpu_vram_used_gb\""),
            "gpu_vram_used_gb missing: {json}"
        );
        assert!(
            json.contains("\"gpu_vram_total_gb\""),
            "gpu_vram_total_gb missing: {json}"
        );
    }

    #[test]
    fn push_health_with_gpu_data_broadcasts_gpu_fields() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        let mut rx = state.tx.subscribe();
        state.push_health(
            "n1",
            55.0,
            4.0,
            16.0,
            Some(42.0),
            Some(3.7),
            Some(4.0),
            None,
        );
        match rx.try_recv().unwrap() {
            DashboardEvent::HealthUpdate { samples, .. } => {
                assert_eq!(samples[0].gpu_pct, Some(42.0));
                assert_eq!(samples[0].gpu_vram_used_gb, Some(3.7));
                assert_eq!(samples[0].gpu_vram_total_gb, Some(4.0));
            }
            _ => panic!("expected HealthUpdate"),
        }
    }

    #[test]
    fn health_update_event_wire_format() {
        // Pins the exact JSON shape the dashboard JS expects.
        let evt = DashboardEvent::HealthUpdate {
            node_id: "n1".into(),
            samples: vec![HealthSample {
                ts_ms: 1_000,
                cpu_pct: 10.0,
                ram_used_gb: 2.0,
                ram_total_gb: 8.0,
                gpu_pct: None,
                gpu_vram_used_gb: None,
                gpu_vram_total_gb: None,
                disk_free_gb: None,
            }],
        };
        let json = serde_json::to_string(&evt).unwrap();
        assert!(
            json.contains("\"type\":\"HealthUpdate\""),
            "missing type tag: {json}"
        );
        assert!(
            json.contains("\"node_id\":\"n1\""),
            "missing node_id: {json}"
        );
        assert!(json.contains("\"samples\""), "missing samples: {json}");
        assert!(
            json.contains("\"ts_ms\""),
            "missing ts_ms in sample: {json}"
        );
    }

    #[test]
    fn get_all_health_snapshots_returns_all_nodes() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_health("n1", 10.0, 1.0, 8.0, None, None, None, None);
        state.push_health("n2", 20.0, 2.0, 8.0, None, None, None, None);
        state.push_health("n1", 15.0, 1.5, 8.0, None, None, None, None);
        let mut snaps = state.get_all_health_snapshots();
        snaps.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(snaps.len(), 2);
        assert_eq!(snaps[0].0, "n1");
        assert_eq!(snaps[0].1.len(), 2);
        assert_eq!(snaps[1].0, "n2");
        assert_eq!(snaps[1].1.len(), 1);
    }

    #[test]
    fn get_all_health_snapshots_empty_when_no_data() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        assert!(state.get_all_health_snapshots().is_empty());
    }

    #[test]
    fn get_all_health_snapshots_is_point_in_time_copy() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_health("n1", 10.0, 1.0, 8.0, None, None, None, None);
        let snap = state.get_all_health_snapshots();
        // Push more data after snapshot — snapshot must not change.
        state.push_health("n1", 99.0, 7.0, 8.0, None, None, None, None);
        assert_eq!(
            snap[0].1.len(),
            1,
            "snapshot should not reflect post-snapshot pushes"
        );
        assert_eq!(snap[0].1[0].cpu_pct, 10.0);
    }

    #[test]
    fn get_all_health_snapshots_preserves_sample_values() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_health("n1", 33.3, 4.5, 16.0, None, None, None, None);
        let snaps = state.get_all_health_snapshots();
        let s = &snaps[0].1[0];
        assert_eq!(s.cpu_pct, 33.3);
        assert_eq!(s.ram_used_gb, 4.5);
        assert_eq!(s.ram_total_gb, 16.0);
        assert!(s.ts_ms > 0);
    }

    #[test]
    fn get_all_health_snapshots_includes_single_sample_node() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_health("solo", 50.0, 2.0, 8.0, None, None, None, None);
        let snaps = state.get_all_health_snapshots();
        assert_eq!(snaps.len(), 1);
        assert_eq!(snaps[0].0, "solo");
        assert_eq!(snaps[0].1.len(), 1);
    }

    #[test]
    fn get_all_health_snapshots_preserves_sample_order() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_health("n1", 10.0, 1.0, 8.0, None, None, None, None);
        state.push_health("n1", 20.0, 2.0, 8.0, None, None, None, None);
        state.push_health("n1", 30.0, 3.0, 8.0, None, None, None, None);
        let snaps = state.get_all_health_snapshots();
        let samp = &snaps[0].1;
        assert_eq!(samp[0].cpu_pct, 10.0, "oldest first");
        assert_eq!(samp[1].cpu_pct, 20.0);
        assert_eq!(samp[2].cpu_pct, 30.0, "newest last");
    }

    // ── push_device_discovery ────────────────────────────────────────────────

    #[test]
    fn push_device_discovery_adds_placeholders() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_device_discovery("n1", vec!["new_bulb".into()], true);
        let snap = state.get_light_snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].device_id, "new_bulb");
        assert_eq!(snap[0].node_id, "n1");
        assert!(!snap[0].on);
        // Placeholders start offline — only a real LightState report marks a
        // device online (otherwise the dashboard shows controls for bulbs the
        // zigbee bridge has never actually reported).
        assert!(!snap[0].online);
    }

    #[test]
    fn zigbee_status_defaults_unknown_and_persists() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        // Defaults to None (unknown) — until a lighting node actually reports, we
        // must NOT claim the bridge is online. The on-connect replay (ws.rs) then
        // sends `null`, which the dashboard renders as an "Unknown" bridge card
        // rather than a misleading green "Online".
        assert_eq!(state.get_zigbee_status(), None);
        // push_zigbee_status must STORE the value (not just broadcast it) so a
        // dashboard connecting after a report is told the right state on connect.
        state.push_zigbee_status(false);
        assert_eq!(state.get_zigbee_status(), Some(false));
        state.push_zigbee_status(true);
        assert_eq!(state.get_zigbee_status(), Some(true));
        // Losing the lighting node drops us back to unknown, not a stale online.
        state.reset_zigbee_status();
        assert_eq!(state.get_zigbee_status(), None);
    }

    #[test]
    fn push_device_discovery_skips_existing_devices() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        // Add a real report first
        state.push_lighting_update(LightStateReport {
            node_id: "n1".into(),
            device_id: "known_bulb".into(),
            on: true,
            brightness: Some(100),
            color_xy: None,
            color_temp: None,
            online: true,
        });
        // Discover it again
        state.push_device_discovery("n1", vec!["known_bulb".into()], true);
        let snap = state.get_light_snapshot();
        assert_eq!(snap.len(), 1);
        assert!(
            snap[0].on,
            "Real state should not be overwritten by placeholder"
        );
    }

    // ── push_model_update / get_model_snapshot ───────────────────────────────

    fn make_node_model_info(node_id: &str) -> NodeModelInfo {
        NodeModelInfo {
            node_id: node_id.into(),
            hostname: "host".into(),
            role: "Compute".into(),
            ram_gb: 16.0,
            models: vec![ModelEntry {
                name: "qwen2.5:7b".into(),
                size_mb: 4000,
                state: "Ready".into(),
                reason: None,
            }],
        }
    }

    #[test]
    fn push_model_update_stores_snapshot() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_model_update(vec![make_node_model_info("n1")]);
        let snap = state.get_model_snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].node_id, "n1");
        assert_eq!(snap[0].models[0].name, "qwen2.5:7b");
    }

    #[test]
    fn push_model_update_broadcasts_event() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        let mut rx = state.tx.subscribe();
        state.push_model_update(vec![make_node_model_info("n1")]);
        match rx.try_recv().unwrap() {
            DashboardEvent::ModelUpdate { nodes } => {
                assert_eq!(nodes.len(), 1);
                assert_eq!(nodes[0].node_id, "n1");
                assert_eq!(nodes[0].ram_gb, 16.0);
            }
            _ => panic!("expected ModelUpdate"),
        }
    }

    #[test]
    fn push_model_update_with_no_receivers_stores_snapshot_anyway() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        // No subscriber — snapshot must still be stored for future connects.
        state.push_model_update(vec![make_node_model_info("n1")]);
        assert_eq!(state.get_model_snapshot().len(), 1);
    }

    #[test]
    fn get_model_snapshot_is_point_in_time_copy() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_model_update(vec![make_node_model_info("n1")]);
        let snap = state.get_model_snapshot();
        state.push_model_update(vec![make_node_model_info("n2")]);
        assert_eq!(
            snap.len(),
            1,
            "snapshot should not reflect post-snapshot updates"
        );
        assert_eq!(snap[0].node_id, "n1");
    }

    #[test]
    fn model_update_event_wire_format() {
        let evt = DashboardEvent::ModelUpdate {
            nodes: vec![NodeModelInfo {
                node_id: "abc".into(),
                hostname: "beelink1".into(),
                role: "Compute".into(),
                ram_gb: 32.0,
                models: vec![ModelEntry {
                    name: "qwen2.5:7b".into(),
                    size_mb: 4000,
                    state: "Ready".into(),
                    reason: None,
                }],
            }],
        };
        let json = serde_json::to_string(&evt).unwrap();
        assert!(
            json.contains("\"type\":\"ModelUpdate\""),
            "missing type tag: {json}"
        );
        assert!(
            json.contains("\"node_id\":\"abc\""),
            "missing node_id: {json}"
        );
        assert!(json.contains("\"ram_gb\":32.0"), "missing ram_gb: {json}");
        assert!(
            json.contains("\"state\":\"Ready\""),
            "missing state: {json}"
        );
        assert!(
            !json.contains("\"reason\""),
            "reason should be absent when None: {json}"
        );
    }

    // ── push_lighting_update / get_light_snapshot ────────────────────────────

    fn make_light_report(device_id: &str, on: bool) -> LightStateReport {
        LightStateReport {
            node_id: "lighting-node".into(),
            device_id: device_id.into(),
            on,
            brightness: Some(200),
            color_xy: None,
            color_temp: Some(370),
            online: true,
        }
    }

    #[test]
    fn push_lighting_update_stores_snapshot() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_lighting_update(make_light_report("bulb1", true));
        let snap = state.get_light_snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].device_id, "bulb1");
        assert!(snap[0].on);
    }

    #[test]
    fn push_lighting_update_broadcasts_event() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        let mut rx = state.tx.subscribe();
        state.push_lighting_update(make_light_report("bulb1", true));
        match rx.try_recv().unwrap() {
            DashboardEvent::LightingUpdate { devices, .. } => {
                assert_eq!(devices.len(), 1);
                assert_eq!(devices[0].device_id, "bulb1");
                assert!(devices[0].on);
            }
            _ => panic!("expected LightingUpdate"),
        }
    }

    #[test]
    fn push_lighting_update_with_no_receivers_stores_snapshot_anyway() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_lighting_update(make_light_report("bulb1", false));
        assert_eq!(state.get_light_snapshot().len(), 1);
    }

    #[test]
    fn push_lighting_update_overwrites_same_device() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_lighting_update(make_light_report("bulb1", true));
        state.push_lighting_update(make_light_report("bulb1", false));
        let snap = state.get_light_snapshot();
        assert_eq!(
            snap.len(),
            1,
            "same device should overwrite, not accumulate"
        );
        assert!(!snap[0].on, "latest state should win");
    }

    #[test]
    fn get_light_snapshot_returns_all_devices() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_lighting_update(make_light_report("bulb1", true));
        state.push_lighting_update(make_light_report("bulb2", false));
        let snap = state.get_light_snapshot();
        assert_eq!(snap.len(), 2);
    }

    #[test]
    fn lighting_update_event_wire_format() {
        let evt = DashboardEvent::LightingUpdate {
            devices: vec![LightStateReport {
                node_id: "n1".into(),
                device_id: "test_bulb".into(),
                on: true,
                brightness: Some(200),
                color_xy: Some((0.3, 0.3)),
                color_temp: Some(370),
                online: true,
            }],
            groups: vec!["all".into()],
        };
        let json = serde_json::to_string(&evt).unwrap();
        assert!(
            json.contains("\"type\":\"LightingUpdate\""),
            "missing type tag: {json}"
        );
        assert!(
            json.contains("\"device_id\":\"test_bulb\""),
            "missing device_id: {json}"
        );
        assert!(json.contains("\"on\":true"), "missing on field: {json}");
        assert!(
            json.contains("\"brightness\":200"),
            "missing brightness: {json}"
        );
        assert!(
            json.contains("\"color_temp\":370"),
            "missing color_temp: {json}"
        );
        assert!(json.contains("\"groups\""), "missing groups field: {json}");
        assert!(json.contains("\"all\""), "missing group name: {json}");
    }

    // ── push_sensor_update / get_sensor_snapshot ─────────────────────────────

    fn make_sensor_report(device_id: &str, temperature: Option<f32>) -> SensorReport {
        SensorReport {
            node_id: "pi1".into(),
            device_id: device_id.into(),
            temperature,
            humidity: None,
            battery: None,
            occupancy: None,
            contact: None,
            online: true,
        }
    }

    #[test]
    fn push_sensor_update_stores_and_broadcasts() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        let mut rx = state.tx.subscribe();
        state.push_sensor_update(make_sensor_report("office_climate", Some(21.4)));
        let snap = state.get_sensor_snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].device_id, "office_climate");
        match rx.try_recv().unwrap() {
            DashboardEvent::SensorUpdate { sensors } => {
                assert_eq!(sensors.len(), 1);
                assert_eq!(sensors[0].temperature, Some(21.4));
            }
            _ => panic!("expected SensorUpdate"),
        }
    }

    #[test]
    fn push_sensor_update_merges_partial_fields() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        let mut with_battery = make_sensor_report("office_climate", Some(21.4));
        with_battery.battery = Some(98);
        state.push_sensor_update(with_battery);
        // Later publish without battery — merged record must keep it.
        let merged = state.push_sensor_update(make_sensor_report("office_climate", Some(22.0)));
        assert_eq!(merged.temperature, Some(22.0), "new reading wins");
        assert_eq!(merged.battery, Some(98), "missing field keeps stored value");
        let snap = state.get_sensor_snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].battery, Some(98));
    }

    #[test]
    fn push_sensor_update_availability_only_keeps_readings() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_sensor_update(make_sensor_report("office_climate", Some(21.4)));
        // Offline flip with no readings (what the sensors capability sends).
        let mut offline = make_sensor_report("office_climate", None);
        offline.online = false;
        let merged = state.push_sensor_update(offline);
        assert!(!merged.online);
        assert_eq!(
            merged.temperature,
            Some(21.4),
            "readings survive availability flips"
        );
    }

    #[test]
    fn sensor_update_event_wire_format() {
        let evt = DashboardEvent::SensorUpdate {
            sensors: vec![make_sensor_report("office_climate", Some(21.4))],
        };
        let json = serde_json::to_string(&evt).unwrap();
        assert!(
            json.contains("\"type\":\"SensorUpdate\""),
            "missing type tag: {json}"
        );
        assert!(
            json.contains("\"device_id\":\"office_climate\""),
            "missing device_id: {json}"
        );
        assert!(
            json.contains("\"temperature\":21.4"),
            "missing temperature: {json}"
        );
        assert!(
            !json.contains("occupancy"),
            "None fields must be omitted: {json}"
        );
    }

    // ── push_group_update / get_group_snapshot ───────────────────────────────

    #[test]
    fn push_group_update_stores_groups() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_group_update("pi1", vec!["all".into(), "living_room".into()]);
        let snap = state.get_group_snapshot();
        assert_eq!(snap.len(), 2);
        assert!(snap.contains(&"all".to_string()));
        assert!(snap.contains(&"living_room".to_string()));
    }

    #[test]
    fn push_group_update_replaces_groups_for_same_node() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_group_update("pi1", vec!["all".into(), "old_group".into()]);
        state.push_group_update("pi1", vec!["all".into()]);
        let snap = state.get_group_snapshot();
        assert_eq!(
            snap.len(),
            1,
            "old_group should be replaced, not accumulated"
        );
        assert!(snap.contains(&"all".to_string()));
    }

    #[test]
    fn get_node_for_group_returns_node_id() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_group_update("pi1", vec!["all".into()]);
        assert_eq!(state.get_node_for_group("all"), Some("pi1".into()));
    }

    #[test]
    fn get_node_for_group_returns_none_for_unknown() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        assert!(state.get_node_for_group("nonexistent").is_none());
    }

    #[test]
    fn get_node_for_device_returns_node_id() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_lighting_update(make_light_report("bulb1", true));
        assert_eq!(
            state.get_node_for_device("bulb1"),
            Some("lighting-node".into())
        );
    }

    #[test]
    fn get_node_for_device_returns_none_for_unknown() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        assert!(state.get_node_for_device("no-such-bulb").is_none());
    }

    // ── push_rooms_update / get_room_snapshot ─────────────────────────────────

    fn make_room_info(id: &str, name: &str) -> RoomInfo {
        RoomInfo {
            id: id.into(),
            name: name.into(),
            position: 0,
            device_ids: vec!["bulb1".into()],
            orientation_degrees: 0.0,
            has_window: false,
            window_facing: None,
            width_m: 3.0,
            depth_m: 6.0,
            height_m: 2.5,
            origin_x: 0.5,
            origin_y: 0.5,
        }
    }

    #[test]
    fn push_rooms_update_stores_snapshot() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_rooms_update(vec![make_room_info("r1", "Living Room")]);
        let snap = state.get_room_snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].id, "r1");
        assert_eq!(snap[0].name, "Living Room");
    }

    #[test]
    fn push_rooms_update_replaces_snapshot() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_rooms_update(vec![make_room_info("r1", "Old")]);
        state.push_rooms_update(vec![make_room_info("r2", "New")]);
        let snap = state.get_room_snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].id, "r2");
    }

    #[test]
    fn push_rooms_update_broadcasts_event() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        let mut rx = state.tx.subscribe();
        state.push_rooms_update(vec![make_room_info("r1", "Bedroom")]);
        match rx.try_recv().unwrap() {
            DashboardEvent::RoomsUpdate { rooms, .. } => {
                assert_eq!(rooms.len(), 1);
                assert_eq!(rooms[0].name, "Bedroom");
            }
            _ => panic!("expected RoomsUpdate"),
        }
    }

    #[test]
    fn push_rooms_update_with_no_receivers_stores_snapshot_anyway() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_rooms_update(vec![make_room_info("r1", "Hall")]);
        assert_eq!(state.get_room_snapshot().len(), 1);
    }

    #[test]
    fn rooms_update_event_wire_format() {
        let evt = DashboardEvent::RoomsUpdate {
            rooms: vec![RoomInfo {
                id: "abc-123".into(),
                name: "Living Room".into(),
                position: 0,
                device_ids: vec!["test_bulb".into()],
                orientation_degrees: 0.0,
                has_window: false,
                window_facing: None,
                width_m: 3.0,
                depth_m: 6.0,
                height_m: 2.5,
                origin_x: 0.5,
                origin_y: 0.5,
            }],
            device_names: std::collections::HashMap::new(),
        };
        let json = serde_json::to_string(&evt).unwrap();
        assert!(
            json.contains("\"type\":\"RoomsUpdate\""),
            "missing type tag: {json}"
        );
        assert!(json.contains("\"id\":\"abc-123\""), "missing id: {json}");
        assert!(
            json.contains("\"name\":\"Living Room\""),
            "missing name: {json}"
        );
        assert!(
            json.contains("\"device_ids\""),
            "missing device_ids: {json}"
        );
    }

    // ── push_scenes_update / get_scene_snapshot ──────────────────────────────

    fn make_scene_info(id: &str, name: &str, room_id: Option<&str>) -> SceneInfo {
        SceneInfo {
            id: id.into(),
            name: name.into(),
            room_id: room_id.map(|s| s.into()),
            created_at: 1_000_000,
            position: 0,
            preview_color: None,
        }
    }

    #[test]
    fn push_scenes_update_stores_snapshot() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_scenes_update(vec![make_scene_info("s1", "Evening", Some("r1"))]);
        let snap = state.get_scene_snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].id, "s1");
        assert_eq!(snap[0].name, "Evening");
        assert_eq!(snap[0].room_id, Some("r1".into()));
    }

    #[test]
    fn push_scenes_update_replaces_snapshot() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_scenes_update(vec![make_scene_info("s1", "Old", None)]);
        state.push_scenes_update(vec![make_scene_info("s2", "New", None)]);
        let snap = state.get_scene_snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].id, "s2");
    }

    #[test]
    fn push_scenes_update_broadcasts_event() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        let mut rx = state.tx.subscribe();
        state.push_scenes_update(vec![make_scene_info("s1", "Night", Some("r1"))]);
        match rx.try_recv().unwrap() {
            DashboardEvent::ScenesUpdate { scenes } => {
                assert_eq!(scenes.len(), 1);
                assert_eq!(scenes[0].name, "Night");
            }
            _ => panic!("expected ScenesUpdate"),
        }
    }

    #[test]
    fn push_scenes_update_with_no_receivers_stores_snapshot_anyway() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_scenes_update(vec![make_scene_info("s1", "Morning", None)]);
        assert_eq!(state.get_scene_snapshot().len(), 1);
    }

    #[test]
    fn scenes_update_event_wire_format() {
        let evt = DashboardEvent::ScenesUpdate {
            scenes: vec![SceneInfo {
                id: "abc-123".into(),
                name: "Evening".into(),
                room_id: Some("room-456".into()),
                created_at: 1_700_000_000,
                position: 0,
                preview_color: None,
            }],
        };
        let json = serde_json::to_string(&evt).unwrap();
        assert!(
            json.contains("\"type\":\"ScenesUpdate\""),
            "missing type tag: {json}"
        );
        assert!(json.contains("\"id\":\"abc-123\""), "missing id: {json}");
        assert!(
            json.contains("\"name\":\"Evening\""),
            "missing name: {json}"
        );
        assert!(
            json.contains("\"room_id\":\"room-456\""),
            "missing room_id: {json}"
        );
    }

    #[test]
    fn scene_info_room_id_omitted_when_none() {
        let evt = DashboardEvent::ScenesUpdate {
            scenes: vec![SceneInfo {
                id: "s1".into(),
                name: "Global".into(),
                room_id: None,
                created_at: 0,
                position: 0,
                preview_color: None,
            }],
        };
        let json = serde_json::to_string(&evt).unwrap();
        assert!(
            !json.contains("\"room_id\""),
            "None room_id should be absent: {json}"
        );
    }

    // ── chaos: broadcast channel behaviours ──────────────────────────────────

    fn make_state() -> Arc<DashboardState> {
        DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        )
    }

    fn dummy_error(i: u64) -> ErrorEntry {
        ErrorEntry {
            ts_ms: i,
            level: "ERROR".into(),
            target: "coordinator::test".into(),
            message: format!("error {i}"),
        }
    }

    /// A slow receiver that never drains will get RecvError::Lagged once the
    /// sender fills the channel (capacity 128). handle_socket continues on Lagged
    /// rather than breaking — this test verifies the channel produces the error.
    #[tokio::test]
    async fn broadcast_receiver_gets_lagged_when_slow() {
        let state = make_state();
        let mut rx = state.tx.subscribe();

        // Send 200 events without draining — exceeds the 128-slot capacity.
        for i in 0..200u64 {
            state.push_error(dummy_error(i));
        }

        match rx.recv().await {
            Err(broadcast::error::RecvError::Lagged(n)) => {
                assert!(n > 0, "lagged count must be positive");
            }
            other => panic!("expected RecvError::Lagged, got {other:?}"),
        }
    }

    /// When the last Arc<DashboardState> is dropped the broadcast sender is
    /// released and waiting receivers get RecvError::Closed.  handle_socket
    /// breaks on Closed — this test verifies the channel produces the error.
    #[tokio::test]
    async fn broadcast_receiver_gets_closed_when_state_dropped() {
        let state = make_state();
        let mut rx = state.tx.subscribe();

        // Drop the sole owner of the state (and therefore of tx).
        drop(state);

        match rx.recv().await {
            Err(broadcast::error::RecvError::Closed) => {}
            other => panic!("expected RecvError::Closed, got {other:?}"),
        }
    }
}
