use crate::registry::{DeviceSnapshot, RoomRecord};
use serde::Serialize;
use shared::MeshMessage;
use shared::hardware::NodeRole;
use shared::messages::{
    ArtStatusReport, LightAction, LightStateReport, NodeRecordLite, ReaperStatusReport,
    SensorReport,
};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::AtomicU64;
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
const JOIN_FEED_CAP: usize = 20;

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

/// One executed tool call inside a [`DashboardEvent::VoiceExchange`] —
/// the chat window renders these the same way it renders typed chat's
/// tool rows.
#[derive(Clone, Debug, Serialize)]
pub struct VoiceToolCall {
    pub tool: String,
    pub result: String,
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
    /// Presence-only inventory for device classes with no dedicated live-state
    /// pipeline yet (Cover/Climate/Switch) — just enough for the Devices tab
    /// to list them and assign a room. Unlike LightingUpdate/SensorUpdate this
    /// carries no state fields, since none of these classes report any yet.
    DeviceInventoryUpdate {
        devices: Vec<shared::DeviceEntry>,
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
    /// One z2m bridge event during pairing. The last few are replayed on WS
    /// connect: the realistic pairing flow is phone-driven (tap Pair, walk to
    /// the device, screen locks, WS drops) so events must survive a reconnect.
    ZigbeeJoinEvent {
        ts_ms: u64,
        event: String,
        device_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    /// A Switch-class device (button remote, dial) fired — purely a
    /// transient UI indicator, not persisted state. See `push_switch_action`.
    SwitchAction {
        device_id: String,
        action: String,
        ts_ms: u64,
    },
    /// One device seen (or RSSI-updated) during a live Bluetooth scan —
    /// transient, drives the dashboard's live scan list. Not replayed on
    /// connect: a scan is tied to the dashboard session that started it.
    BluetoothDeviceFound {
        node_id: String,
        mac: String,
        name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        rssi: Option<i32>,
    },
    /// The outcome of a dashboard-initiated pair request.
    BluetoothPairResult {
        node_id: String,
        mac: String,
        name: String,
        success: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// The outcome of a dashboard-initiated "clear cache" request.
    BluetoothClearCacheResult {
        node_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cleared: Option<usize>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// The outcome of a dashboard-initiated unpair request.
    BluetoothUnpairResult {
        node_id: String,
        mac: String,
        success: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// The outcome of a dashboard-initiated volume-set request.
    BluetoothVolumeResult {
        node_id: String,
        success: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        volume_pct: Option<u8>,
    },
    /// The outcome of a dashboard-initiated mute/unmute request.
    BluetoothMuteResult {
        node_id: String,
        success: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// A live connected/unavailable change for a node's currently-paired
    /// Bluetooth device — pushed only on change, not a heartbeat. Not
    /// replayed on connect: `GET /api/av-devices`' `bluetooth_paired` field
    /// (backed by `DashboardState::bluetooth_paired_status`) is the
    /// source of truth a freshly-connected dashboard reads instead.
    BluetoothStatusUpdate {
        node_id: String,
        mac: String,
        name: String,
        connected: bool,
    },
    /// One completed voice-assistant exchange (spoken transcript in, intent
    /// response out). Transient, broadcast-only — the chat window renders it
    /// when its "show voice commands" preference is on; a future TTS/speaker
    /// output sink is expected to consume the same event. Not replayed on
    /// connect, matching typed chat's own ephemeral semantics.
    VoiceExchange {
        transcript: String,
        response: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        tool_calls: Vec<VoiceToolCall>,
        node_id: String,
        model_name: String,
        total_ms: u64,
        ts_ms: u64,
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
    /// A new listing surfaced for a hunt (see `plans/ebay-bargain-finder.md`).
    /// Not replayed on connect — the Hunts tab loads its initial feed via
    /// `GET /api/ebay/finds`, matching how `VoiceExchange` relies on a
    /// separate snapshot fetch rather than WS replay.
    EbayFind {
        hunt_id: String,
        hunt_name: String,
        find: crate::registry::EbayFindRecord,
    },
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

/// A room group's wire representation - see `RoomGroupRecord`'s doc comment
/// in `registry/mod.rs` for why this is not the same thing as a Zigbee/z2m
/// group.
#[derive(Clone, Debug, Serialize)]
pub struct GroupInfo {
    pub id: String,
    pub name: String,
    pub position: i64,
    pub device_ids: Vec<String>,
}

impl From<crate::registry::RoomGroupRecord> for GroupInfo {
    fn from(g: crate::registry::RoomGroupRecord) -> Self {
        Self {
            id: g.id,
            name: g.name,
            position: g.position,
            device_ids: g.device_ids,
        }
    }
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
    pub groups: Vec<GroupInfo>,
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
            groups: r.groups.into_iter().map(GroupInfo::from).collect(),
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
    /// Per-device saved values, so the client can detect when a device's
    /// live state has diverged from its room's active scene (a chat
    /// command, a physical switch, another client) without the coordinator
    /// needing to track "active scene per room" itself — the frontend
    /// already gets every live state change over the wire and can compare.
    /// When `effect_id` is set, this holds only the devices overridden out
    /// of the effect — everything else is driven by the effect on recall.
    pub states: Vec<DeviceSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effect_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effect_params: Option<serde_json::Value>,
    /// Set when this scene targets one room-group's members rather than
    /// the whole room.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
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
    /// Presence-only inventory for Cover/Climate/Switch devices, keyed by
    /// device id → (owning node_id, the entry itself — type plus the
    /// declared action vocabulary switches carry). See `push_other_devices`.
    other_device_snapshot: Mutex<HashMap<String, (String, shared::DeviceEntry)>>,
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
    /// The node that owns the Zigbee bridge (last node to send a DeviceList /
    /// ZigbeeStatus) — target for bridge-wide admin commands (permit-join,
    /// device removal). Single-bridge assumption, same as the status field.
    zigbee_node: Mutex<Option<String>>,
    /// Recent pairing-feed events, replayed to new WS clients (see
    /// `DashboardEvent::ZigbeeJoinEvent`).
    join_feed: Mutex<VecDeque<DashboardEvent>>,
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
    /// Last-known Frame TV art-display status. No WS broadcast yet (see
    /// push_art_status) — v1 has no dashboard panel to consume one.
    art_snapshot: Mutex<Option<ArtStatusReport>>,
    /// The active art slideshow, if any — a list of images built from a
    /// search (see `api::art::search_art`) plus which one is currently
    /// showing. `generation` increments on every new search and lets the
    /// background auto-advance task (also spawned by `search_art`) detect
    /// it's been superseded and stop on its own rather than racing a newer
    /// rotation — see `advance_art_rotation_if_current`.
    art_rotation: Mutex<Option<ArtRotationState>>,
    /// The general/default slideshow's cached image list — built once
    /// (Met highlights, see `api::art::build_general_batch`) and reused on
    /// every revert-to-general rather than re-querying the Met API each
    /// time a specific search goes idle.
    general_art_batch: Mutex<Option<Vec<ArtRotationItem>>>,
    /// Currently-paired Bluetooth device per node, keyed by node_id — the
    /// live source `GET /api/av-devices` reads for its `bluetooth_paired`
    /// field. Set on a successful pair or `BluetoothStatusUpdate`, cleared
    /// on a successful unpair. In-memory only: a coordinator restart loses
    /// it until the next pair or status change (see
    /// `capability_audio::bluetooth_status_loop`'s doc comment).
    bluetooth_status: Mutex<HashMap<String, BluetoothPairedStatus>>,
    /// Per-hunt timer generation (see `plans/ebay-bargain-finder.md`), keyed
    /// by hunt id. Bumped on any create/update/delete so a stale background
    /// timer loop notices at its next wake and exits instead of racing a
    /// freshly (re-)spawned one — same self-cancel trick as `art_rotation`'s
    /// single generation counter, but keyed since hunts run concurrently.
    ebay_hunt_generations: Mutex<HashMap<String, Arc<AtomicU64>>>,
}

/// A node's currently-paired Bluetooth device and whether it's actually
/// connected right now. See `DashboardState::bluetooth_paired_status`.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct BluetoothPairedStatus {
    pub mac: String,
    pub name: String,
    pub connected: bool,
    /// The volume (0-100) this node's sink was last set to — `None` if
    /// never established (see `capabilities/audio/src/bluetooth.rs`'s
    /// `DEFAULT_INITIAL_VOLUME_PCT`). Set fresh on a new pair; preserved
    /// across a mere `BluetoothStatusUpdate` (see that handler in
    /// `server.rs`) and updated in place by `set_bluetooth_volume` on a
    /// successful `BluetoothVolumeResult` — neither of those otherwise
    /// knows the rest of this struct, so overwriting the whole record on
    /// either would silently forget the other's information.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub volume_pct: Option<u8>,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct ArtRotationItem {
    pub image_url: String,
    pub title: String,
    pub artist: String,
    pub date: String,
    /// The Met's free-text `artistDisplayBio` field (e.g. "French,
    /// 1840–1926") — nationality plus birth/death years when known, used to
    /// ground the spoken narration (see `art::narrate_artwork`) without
    /// inventing unverifiable biographical claims.
    pub artist_bio: String,
}

struct ArtRotationState {
    query: String,
    items: Vec<ArtRotationItem>,
    index: usize,
    generation: u64,
    /// Refreshed on a new search and on a manual `/api/art/next` — an
    /// explicit sign someone's actually engaging with this specific
    /// rotation. Deliberately *not* refreshed by the rotation's own
    /// auto-advance ticks, or a search would never go idle on its own no
    /// matter how long ago it was actually asked for. The auto-advance
    /// timer (`api::art::spawn_art_rotation_timer`) uses this to decide
    /// when to give up and revert to the general slideshow.
    last_engaged_at: std::time::Instant,
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
            other_device_snapshot: Mutex::new(HashMap::new()),
            group_snapshot: Mutex::new(HashMap::new()),
            room_snapshot: Mutex::new(Vec::new()),
            scene_snapshot: Mutex::new(Vec::new()),
            effect_snapshot: Mutex::new(HashMap::new()),
            zigbee_status: Mutex::new(None),
            zigbee_node: Mutex::new(None),
            join_feed: Mutex::new(VecDeque::new()),
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
            art_snapshot: Mutex::new(None),
            art_rotation: Mutex::new(None),
            general_art_batch: Mutex::new(None),
            bluetooth_status: Mutex::new(HashMap::new()),
            ebay_hunt_generations: Mutex::new(HashMap::new()),
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

    /// Broadcast a new hunt find to connected dashboards (no-op if none).
    pub fn push_ebay_find(
        &self,
        hunt_id: &str,
        hunt_name: &str,
        find: crate::registry::EbayFindRecord,
    ) {
        if self.tx.receiver_count() > 0 {
            let _ = self.tx.send(DashboardEvent::EbayFind {
                hunt_id: hunt_id.to_owned(),
                hunt_name: hunt_name.to_owned(),
                find,
            });
        }
    }

    /// The shared generation counter for `hunt_id`'s background timer,
    /// creating one (starting at 0) if this is the first time it's asked
    /// for. A running timer loop captures the current value before each
    /// sleep and checks it again on waking; if it no longer matches, some
    /// other call bumped it (see `bump_ebay_hunt_generation`) and the loop
    /// exits rather than running a stale cycle.
    pub fn ebay_hunt_generation(&self, hunt_id: &str) -> Arc<AtomicU64> {
        self.ebay_hunt_generations
            .lock()
            .unwrap()
            .entry(hunt_id.to_owned())
            .or_insert_with(|| Arc::new(AtomicU64::new(0)))
            .clone()
    }

    /// Invalidate `hunt_id`'s current background timer (if any) by bumping
    /// its generation counter. Call on every update/delete/re-arm so an
    /// outstanding sleep from a superseded timer becomes a no-op instead of
    /// running with stale timeslots/terms. Returns the new generation.
    pub fn bump_ebay_hunt_generation(&self, hunt_id: &str) -> u64 {
        self.ebay_hunt_generation(hunt_id)
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1
    }

    /// Drop `hunt_id`'s generation counter entirely — call on hunt deletion
    /// so this map doesn't grow by one entry for every hunt ever created
    /// over a coordinator's lifetime. Safe even if a timer loop is mid-cycle:
    /// deletion is independently detected on that loop's next wake via
    /// `Registry::get_hunt` returning `None`, and a fresh lookup after
    /// removal starts back at generation 0, which a real in-flight loop's
    /// captured generation (always >= 1, see `bump_ebay_hunt_generation`)
    /// can never match — so it's still correctly treated as stale.
    pub fn remove_ebay_hunt_generation(&self, hunt_id: &str) {
        self.ebay_hunt_generations.lock().unwrap().remove(hunt_id);
    }

    /// Broadcast one completed voice exchange to connected dashboards
    /// (no-op if none). See [`DashboardEvent::VoiceExchange`].
    pub fn push_voice_exchange(&self, transcript: String, resp: &shared::IntentResponse) {
        if self.tx.receiver_count() == 0 {
            return;
        }
        let ts_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let _ = self.tx.send(DashboardEvent::VoiceExchange {
            transcript,
            response: resp.text.clone().unwrap_or_default(),
            error: resp.error.clone(),
            tool_calls: resp
                .tool_calls
                .iter()
                .map(|t| VoiceToolCall {
                    tool: t.tool.clone(),
                    result: t.result.clone().unwrap_or_default(),
                })
                .collect(),
            node_id: resp.node_id.clone(),
            model_name: resp.model_name.clone(),
            total_ms: resp.total_ms,
            ts_ms,
        });
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
                    illuminance: report.illuminance.or(existing.illuminance),
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

    /// Store this node's Cover/Climate/Switch devices (replacing its previous
    /// rows, same full-snapshot-per-node semantics as `push_group_update`) and
    /// broadcast the merged inventory. Light/Sensor/Unknown are skipped —
    /// those already have their own live-state pipelines (or, for Unknown,
    /// nothing useful to show).
    pub fn push_other_devices(&self, node_id: &str, devices: &[shared::DeviceEntry]) {
        use shared::DeviceType;
        {
            let mut snap = self.other_device_snapshot.lock().unwrap();
            snap.retain(|_, (nid, _)| nid != node_id);
            for d in devices {
                if matches!(
                    d.device_type,
                    DeviceType::Cover | DeviceType::Climate | DeviceType::Switch
                ) {
                    snap.insert(d.id.clone(), (node_id.to_owned(), d.clone()));
                }
            }
        }
        if self.tx.receiver_count() > 0 {
            let _ = self.tx.send(DashboardEvent::DeviceInventoryUpdate {
                devices: self.get_other_device_snapshot(),
            });
        }
    }

    /// Return the current Cover/Climate/Switch inventory — used to warm-start
    /// new WS clients (see `push_other_devices`).
    pub fn get_other_device_snapshot(&self) -> Vec<shared::DeviceEntry> {
        self.other_device_snapshot
            .lock()
            .unwrap()
            .values()
            .map(|(_, entry)| entry.clone())
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

    /// Remember which node owns the Zigbee bridge (targets bridge-wide admin
    /// commands). Set whenever a node identifies itself by sending a
    /// DeviceList or ZigbeeStatus.
    pub fn set_zigbee_node(&self, node_id: &str) {
        *self.zigbee_node.lock().unwrap() = Some(node_id.to_string());
    }

    pub fn get_zigbee_node(&self) -> Option<String> {
        self.zigbee_node.lock().unwrap().clone()
    }

    /// Record + broadcast one pairing-feed event. The last `JOIN_FEED_CAP`
    /// are kept for replay-on-connect: pairing is phone-driven and the phone
    /// screen locking mid-window drops the WS.
    pub fn push_join_event(&self, event: String, device_id: String, model: Option<String>) {
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let evt = {
            let mut feed = self.join_feed.lock().unwrap();
            // Strictly monotonic timestamps: the client dedupes replayed
            // events by ts, so two events in the same millisecond (joined →
            // interview started) must not share one.
            let last_ts = feed
                .back()
                .and_then(|e| match e {
                    DashboardEvent::ZigbeeJoinEvent { ts_ms, .. } => Some(*ts_ms),
                    _ => None,
                })
                .unwrap_or(0);
            let evt = DashboardEvent::ZigbeeJoinEvent {
                ts_ms: now_ms.max(last_ts + 1),
                event,
                device_id,
                model,
            };
            feed.push_back(evt.clone());
            while feed.len() > JOIN_FEED_CAP {
                feed.pop_front();
            }
            evt
        };
        if self.tx.receiver_count() > 0 {
            let _ = self.tx.send(evt);
        }
    }

    /// Recent pairing-feed events for warm-starting new WS clients, oldest first.
    pub fn get_join_feed(&self) -> Vec<DashboardEvent> {
        self.join_feed.lock().unwrap().iter().cloned().collect()
    }

    /// Immediately mark a device offline (light and/or sensor snapshot,
    /// whichever it's actually known in — a device is never both, checking
    /// both just avoids the caller needing to know its type). Called on a
    /// z2m `device_leave` pairing event, which is a much stronger, faster
    /// signal than waiting on z2m's own availability timeout (~10 min for
    /// an active/routered device, ~25h for a battery/passive one) — a
    /// device that's genuinely left the network shouldn't keep showing
    /// stale "online" readings for that whole window. Preserves the last
    /// known reading, same as a normal availability-flip-to-offline update
    /// already does — this only changes *when* that happens, not the
    /// semantics of what an offline device looks like.
    pub fn mark_device_offline(&self, device_id: &str) {
        let light_node = self
            .light_snapshot
            .lock()
            .unwrap()
            .get(device_id)
            .map(|r| r.node_id.clone());
        if let Some(node_id) = light_node {
            self.push_lighting_update(LightStateReport {
                node_id,
                device_id: device_id.to_string(),
                on: false,
                brightness: None,
                color_xy: None,
                color_temp: None,
                online: false,
            });
        }

        let sensor_node = self
            .sensor_snapshot
            .lock()
            .unwrap()
            .get(device_id)
            .map(|r| r.node_id.clone());
        if let Some(node_id) = sensor_node {
            self.push_sensor_update(SensorReport {
                node_id,
                device_id: device_id.to_string(),
                temperature: None,
                humidity: None,
                battery: None,
                occupancy: None,
                contact: None,
                illuminance: None,
                online: false,
            });
        }
    }

    /// Purge a deleted device from every in-memory snapshot it might be in
    /// (light, sensor, or the Cover/Climate/Switch presence list) and
    /// re-broadcast the now-smaller full snapshot for whichever one
    /// actually held it — the same full-snapshot broadcast every
    /// `push_*_update` already sends, so connected clients drop the row
    /// exactly like any other update. Called from `DELETE /api/lights/
    /// {device}`: the registry row is deleted there, but registry deletion
    /// alone is invisible to already-connected clients — without this, the
    /// device just sits showing "Unassigned" forever, since nothing else
    /// ever re-broadcasts a snapshot that no longer includes it.
    pub fn remove_device(&self, device_id: &str) {
        let remaining_lights = {
            let mut snap = self.light_snapshot.lock().unwrap();
            snap.remove(device_id)
                .map(|_| snap.values().cloned().collect::<Vec<_>>())
        };
        if let Some(devices) = remaining_lights
            && self.tx.receiver_count() > 0
        {
            let groups = self.get_group_snapshot();
            let _ = self
                .tx
                .send(DashboardEvent::LightingUpdate { devices, groups });
        }

        let remaining_sensors = {
            let mut snap = self.sensor_snapshot.lock().unwrap();
            snap.remove(device_id)
                .map(|_| snap.values().cloned().collect::<Vec<_>>())
        };
        if let Some(sensors) = remaining_sensors
            && self.tx.receiver_count() > 0
        {
            let _ = self.tx.send(DashboardEvent::SensorUpdate { sensors });
        }

        let removed_other = self
            .other_device_snapshot
            .lock()
            .unwrap()
            .remove(device_id)
            .is_some();
        if removed_other && self.tx.receiver_count() > 0 {
            let _ = self.tx.send(DashboardEvent::DeviceInventoryUpdate {
                devices: self.get_other_device_snapshot(),
            });
        }
    }

    /// Broadcast a Switch-class button press / dial rotation. Purely a
    /// transient UI indicator (a device row briefly flashes) — unlike the
    /// join feed, there's no replay buffer: a client not connected when it
    /// fires just misses the flash, which is fine for what this is.
    pub fn push_switch_action(&self, device_id: String, action: String) {
        if self.tx.receiver_count() == 0 {
            return;
        }
        let ts_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let _ = self.tx.send(DashboardEvent::SwitchAction {
            device_id,
            action,
            ts_ms,
        });
    }

    /// Broadcast one device seen (or RSSI-updated) during a live Bluetooth
    /// scan. Transient like `push_switch_action` — no replay buffer, a
    /// scan is tied to whichever dashboard session started it.
    pub fn push_bluetooth_device_found(&self, info: shared::BluetoothDeviceInfo) {
        if self.tx.receiver_count() == 0 {
            tracing::info!(
                mac = %info.mac,
                "bluetooth: device found but no dashboard WS subscribers — dropping"
            );
            return;
        }
        let _ = self.tx.send(DashboardEvent::BluetoothDeviceFound {
            node_id: info.node_id,
            mac: info.mac,
            name: info.name,
            rssi: info.rssi,
        });
    }

    /// Broadcast the outcome of a dashboard-initiated Bluetooth pair.
    pub fn push_bluetooth_pair_result(&self, result: shared::BluetoothPairResult) {
        if self.tx.receiver_count() == 0 {
            return;
        }
        let _ = self.tx.send(DashboardEvent::BluetoothPairResult {
            node_id: result.node_id,
            mac: result.mac,
            name: result.name,
            success: result.success,
            error: result.error,
        });
    }

    /// Broadcast the outcome of a dashboard-initiated Bluetooth cache clear.
    pub fn push_bluetooth_clear_cache_result(&self, result: shared::BluetoothClearCacheResult) {
        if self.tx.receiver_count() == 0 {
            return;
        }
        let _ = self.tx.send(DashboardEvent::BluetoothClearCacheResult {
            node_id: result.node_id,
            cleared: result.cleared,
            error: result.error,
        });
    }

    /// Broadcast the outcome of a dashboard-initiated Bluetooth unpair.
    pub fn push_bluetooth_unpair_result(&self, result: shared::BluetoothUnpairResult) {
        if self.tx.receiver_count() == 0 {
            return;
        }
        let _ = self.tx.send(DashboardEvent::BluetoothUnpairResult {
            node_id: result.node_id,
            mac: result.mac,
            success: result.success,
            error: result.error,
        });
    }

    /// Broadcast the outcome of a dashboard-initiated Bluetooth volume-set.
    pub fn push_bluetooth_volume_result(&self, result: shared::BluetoothVolumeResult) {
        if self.tx.receiver_count() == 0 {
            return;
        }
        let _ = self.tx.send(DashboardEvent::BluetoothVolumeResult {
            node_id: result.node_id,
            success: result.success,
            error: result.error,
            volume_pct: result.volume_pct,
        });
    }

    /// Broadcast the outcome of a dashboard-initiated Bluetooth mute/unmute.
    pub fn push_bluetooth_mute_result(&self, result: shared::BluetoothMuteResult) {
        if self.tx.receiver_count() == 0 {
            return;
        }
        let _ = self.tx.send(DashboardEvent::BluetoothMuteResult {
            node_id: result.node_id,
            success: result.success,
            error: result.error,
        });
    }

    /// Broadcast a live connected/unavailable change for a node's paired
    /// Bluetooth device.
    pub fn push_bluetooth_status_update(&self, update: shared::BluetoothStatusUpdate) {
        if self.tx.receiver_count() == 0 {
            return;
        }
        let _ = self.tx.send(DashboardEvent::BluetoothStatusUpdate {
            node_id: update.node_id,
            mac: update.mac,
            name: update.name,
            connected: update.connected,
        });
    }

    /// Records this node's currently-paired Bluetooth device — called on a
    /// successful pair and on every `BluetoothStatusUpdate`.
    pub fn set_bluetooth_paired(&self, node_id: &str, status: BluetoothPairedStatus) {
        self.bluetooth_status
            .lock()
            .unwrap()
            .insert(node_id.to_string(), status);
    }

    /// Forgets this node's paired-device status — called on a successful
    /// unpair.
    pub fn clear_bluetooth_paired(&self, node_id: &str) {
        self.bluetooth_status.lock().unwrap().remove(node_id);
    }

    /// This node's currently-paired Bluetooth device, if any — read by
    /// `GET /api/av-devices` for its bluetooth-transport rows.
    pub fn bluetooth_paired_status(&self, node_id: &str) -> Option<BluetoothPairedStatus> {
        self.bluetooth_status.lock().unwrap().get(node_id).cloned()
    }

    /// Updates just the volume field of a node's already-known paired-device
    /// record — a `BluetoothVolumeResult` only ever reports the node_id and
    /// the new volume, not the rest of the device's identity, so this
    /// leaves mac/name/connected untouched. No-op if the node has no known
    /// paired device yet (shouldn't happen in practice: a volume request is
    /// only ever sent for a node with something paired).
    pub fn set_bluetooth_volume(&self, node_id: &str, volume_pct: u8) {
        if let Some(status) = self.bluetooth_status.lock().unwrap().get_mut(node_id) {
            status.volume_pct = Some(volume_pct);
        }
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

    /// Record the art node's latest status. Plain overwrite, no change-detection
    /// or broadcast (unlike push_reaper_status) — v1 has no dashboard panel
    /// polling/subscribing to this yet, just the REST snapshot below.
    pub fn push_art_status(&self, report: ArtStatusReport) {
        *self.art_snapshot.lock().unwrap() = Some(report);
    }

    /// Return the current art-display snapshot for REST responses.
    pub fn get_art_snapshot(&self) -> Option<ArtStatusReport> {
        self.art_snapshot.lock().unwrap().clone()
    }

    /// Replace the active slideshow with a freshly-built list (a new search
    /// always starts at index 0) and return its generation — the caller uses
    /// this to spawn an auto-advance task that stops itself once superseded.
    pub fn set_art_rotation(&self, query: String, items: Vec<ArtRotationItem>) -> u64 {
        let mut guard = self.art_rotation.lock().unwrap();
        let generation = guard.as_ref().map_or(1, |r| r.generation + 1);
        *guard = Some(ArtRotationState {
            query,
            items,
            index: 0,
            generation,
            last_engaged_at: std::time::Instant::now(),
        });
        generation
    }

    /// The item at the current index, if a rotation exists and isn't empty.
    pub fn art_rotation_current_item(&self) -> Option<ArtRotationItem> {
        let guard = self.art_rotation.lock().unwrap();
        guard.as_ref().and_then(|r| r.items.get(r.index).cloned())
    }

    /// Advance to the next item (wrapping) only if `generation` still
    /// matches the live rotation — used by the background auto-advance
    /// task, which should quietly stop rather than fight a newer search.
    /// Returns `None` both when superseded and when there's nothing to show.
    pub fn advance_art_rotation_if_current(&self, generation: u64) -> Option<ArtRotationItem> {
        let mut guard = self.art_rotation.lock().unwrap();
        let r = guard.as_mut()?;
        if r.generation != generation || r.items.is_empty() {
            return None;
        }
        r.index = (r.index + 1) % r.items.len();
        r.items.get(r.index).cloned()
    }

    /// Advance to the next item (wrapping) unconditionally — for
    /// `POST /api/art/next`, which should always affect whatever the
    /// current live rotation is, not be scoped to a particular generation.
    pub fn manual_advance_art_rotation(&self) -> Option<ArtRotationItem> {
        let mut guard = self.art_rotation.lock().unwrap();
        let r = guard.as_mut()?;
        if r.items.is_empty() {
            return None;
        }
        r.last_engaged_at = std::time::Instant::now();
        r.index = (r.index + 1) % r.items.len();
        r.items.get(r.index).cloned()
    }

    /// Has it been at least `idle_timeout` since anyone last searched for or
    /// manually advanced the active specific rotation? Used by the
    /// auto-advance timer to decide it's time to give up and revert to the
    /// general slideshow — `None` (no rotation at all) counts as not idle,
    /// since there's nothing to revert *from*.
    pub fn art_rotation_idle_for(&self, idle_timeout: std::time::Duration) -> bool {
        let guard = self.art_rotation.lock().unwrap();
        guard
            .as_ref()
            .is_some_and(|r| r.last_engaged_at.elapsed() >= idle_timeout)
    }

    /// Clear the active specific rotation — called when reverting to the
    /// general slideshow, so a stale generation's auto-advance tick (if one
    /// is still mid-sleep) finds nothing and stops cleanly next time it
    /// wakes, same as being superseded by a newer search.
    pub fn clear_art_rotation(&self) {
        *self.art_rotation.lock().unwrap() = None;
    }

    /// Cache the general-slideshow batch so reverting to it later (after a
    /// specific search goes idle) doesn't need a fresh Met API round-trip.
    pub fn set_general_art_batch(&self, items: Vec<ArtRotationItem>) {
        *self.general_art_batch.lock().unwrap() = Some(items);
    }

    pub fn get_general_art_batch(&self) -> Option<Vec<ArtRotationItem>> {
        self.general_art_batch.lock().unwrap().clone()
    }

    /// `GET /api/art/current` payload — query, position, and the current
    /// item's metadata, or `None` if no rotation has ever been started.
    pub fn art_rotation_status(&self) -> Option<serde_json::Value> {
        let guard = self.art_rotation.lock().unwrap();
        let r = guard.as_ref()?;
        let item = r.items.get(r.index)?;
        Some(serde_json::json!({
            "query": r.query,
            "index": r.index,
            "count": r.items.len(),
            "title": item.title,
            "artist": item.artist,
            "date": item.date,
            "image_url": item.image_url,
        }))
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
            illuminance: None,
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
    fn push_sensor_update_merges_illuminance() {
        // SNZB-03P R2 shape: occupancy/illuminance/battery, no temperature.
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        let mut first = make_sensor_report("hall_motion", None);
        first.occupancy = Some(true);
        first.illuminance = Some(120.0);
        state.push_sensor_update(first);
        // A later occupancy-only publish (illuminance only updates on detection
        // per the device's own behaviour) must not drop the last lux reading.
        let mut later = make_sensor_report("hall_motion", None);
        later.occupancy = Some(false);
        let merged = state.push_sensor_update(later);
        assert_eq!(merged.occupancy, Some(false), "new reading wins");
        assert_eq!(
            merged.illuminance,
            Some(120.0),
            "missing field keeps stored value"
        );
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

    // ── push_join_event / get_join_feed ──────────────────────────────────────

    #[test]
    fn join_feed_caps_and_replays_in_order() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        for i in 0..25 {
            state.push_join_event("device_joined".into(), format!("dev{i}"), None);
        }
        let feed = state.get_join_feed();
        assert_eq!(feed.len(), 20, "feed capped at JOIN_FEED_CAP");
        match &feed[0] {
            DashboardEvent::ZigbeeJoinEvent { device_id, .. } => {
                assert_eq!(device_id, "dev5", "oldest events evicted first");
            }
            _ => panic!("expected ZigbeeJoinEvent"),
        }
    }

    #[test]
    fn join_feed_timestamps_strictly_increase() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        // Pushed in a tight loop — wall clock won't tick between them.
        state.push_join_event("device_joined".into(), "d".into(), None);
        state.push_join_event("device_interview_started".into(), "d".into(), None);
        let feed = state.get_join_feed();
        let ts: Vec<u64> = feed
            .iter()
            .map(|e| match e {
                DashboardEvent::ZigbeeJoinEvent { ts_ms, .. } => *ts_ms,
                _ => panic!("expected ZigbeeJoinEvent"),
            })
            .collect();
        assert!(
            ts[1] > ts[0],
            "client dedupes by ts — must be strict: {ts:?}"
        );
    }

    // ── mark_device_offline ──────────────────────────────────────────────────

    #[test]
    fn mark_device_offline_flips_a_known_light_preserving_readings() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_lighting_update(make_light_report("bulb1", true));
        state.mark_device_offline("bulb1");
        let snap = state.get_light_snapshot();
        let bulb = snap.iter().find(|l| l.device_id == "bulb1").unwrap();
        assert!(!bulb.online);
        // make_light_report sets brightness Some(200) — must survive.
        assert_eq!(bulb.brightness, Some(200));
    }

    #[test]
    fn mark_device_offline_flips_a_known_sensor_preserving_readings() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_sensor_update(make_sensor_report("sensor1", Some(21.4)));
        state.mark_device_offline("sensor1");
        let snap = state.get_sensor_snapshot();
        let sensor = snap.iter().find(|s| s.device_id == "sensor1").unwrap();
        assert!(!sensor.online);
        assert_eq!(sensor.temperature, Some(21.4));
    }

    #[test]
    fn mark_device_offline_is_a_noop_for_an_unknown_device() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        // Must not panic, and must not insert a phantom entry into either
        // snapshot for a device that was never actually known.
        state.mark_device_offline("never-seen");
        assert!(state.get_light_snapshot().is_empty());
        assert!(state.get_sensor_snapshot().is_empty());
    }

    #[test]
    fn mark_device_offline_does_not_touch_an_unrelated_device() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_lighting_update(make_light_report("bulb1", true));
        state.push_lighting_update(make_light_report("bulb2", true));
        state.mark_device_offline("bulb1");
        let snap = state.get_light_snapshot();
        let bulb2 = snap.iter().find(|l| l.device_id == "bulb2").unwrap();
        assert!(bulb2.online, "unrelated device must be untouched");
    }

    // ── remove_device ─────────────────────────────────────────────────────────

    #[test]
    fn remove_device_purges_a_known_light_and_broadcasts_the_rest() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_lighting_update(make_light_report("bulb1", true));
        state.push_lighting_update(make_light_report("bulb2", true));
        let mut rx = state.tx.subscribe();
        state.remove_device("bulb1");

        let snap = state.get_light_snapshot();
        assert!(
            !snap.iter().any(|l| l.device_id == "bulb1"),
            "deleted device must be gone, not just unassigned"
        );
        assert!(snap.iter().any(|l| l.device_id == "bulb2"));
        match rx.try_recv().unwrap() {
            DashboardEvent::LightingUpdate { devices, .. } => {
                assert!(!devices.iter().any(|l| l.device_id == "bulb1"));
            }
            other => panic!("expected LightingUpdate, got {other:?}"),
        }
    }

    #[test]
    fn remove_device_purges_a_known_sensor() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_sensor_update(make_sensor_report("sensor1", Some(21.4)));
        state.remove_device("sensor1");
        assert!(state.get_sensor_snapshot().is_empty());
    }

    #[test]
    fn remove_device_purges_a_known_cover_climate_or_switch() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_other_devices(
            "pi1",
            &[shared::DeviceEntry {
                id: "blind1".into(),
                device_type: shared::DeviceType::Cover,
                actions: vec![],
            }],
        );
        state.remove_device("blind1");
        assert!(state.get_other_device_snapshot().is_empty());
    }

    #[test]
    fn remove_device_is_a_noop_for_an_unknown_device() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        // Must not panic and must not insert a phantom entry anywhere.
        state.remove_device("never-seen");
        assert!(state.get_light_snapshot().is_empty());
        assert!(state.get_sensor_snapshot().is_empty());
        assert!(state.get_other_device_snapshot().is_empty());
    }

    // ── push_switch_action ────────────────────────────────────────────────────

    #[test]
    fn push_switch_action_broadcasts_event() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        let mut rx = state.tx.subscribe();
        state.push_switch_action("tap_dial".into(), "button_1_press".into());
        match rx.try_recv().unwrap() {
            DashboardEvent::SwitchAction {
                device_id, action, ..
            } => {
                assert_eq!(device_id, "tap_dial");
                assert_eq!(action, "button_1_press");
            }
            _ => panic!("expected SwitchAction"),
        }
    }

    #[test]
    fn push_switch_action_with_no_receivers_is_noop() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        // Must not panic with zero subscribers (no persisted snapshot to update either).
        state.push_switch_action("tap_dial".into(), "button_1_press".into());
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
    fn push_other_devices_stores_cover_climate_switch_only() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_other_devices(
            "pi1",
            &[
                shared::DeviceEntry {
                    id: "blind1".into(),
                    device_type: shared::DeviceType::Cover,
                    actions: vec![],
                },
                shared::DeviceEntry {
                    id: "trv1".into(),
                    device_type: shared::DeviceType::Climate,
                    actions: vec![],
                },
                shared::DeviceEntry {
                    id: "tap_dial".into(),
                    device_type: shared::DeviceType::Switch,
                    actions: vec![],
                },
                shared::DeviceEntry {
                    id: "bulb1".into(),
                    device_type: shared::DeviceType::Light,
                    actions: vec![],
                },
                shared::DeviceEntry {
                    id: "temp1".into(),
                    device_type: shared::DeviceType::Sensor,
                    actions: vec![],
                },
                shared::DeviceEntry {
                    id: "mystery".into(),
                    device_type: shared::DeviceType::Unknown,
                    actions: vec![],
                },
            ],
        );
        let snap = state.get_other_device_snapshot();
        let ids: std::collections::HashSet<&str> = snap.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(snap.len(), 3, "only Cover/Climate/Switch should be kept");
        assert!(ids.contains("blind1"));
        assert!(ids.contains("trv1"));
        assert!(ids.contains("tap_dial"));
    }

    #[test]
    fn push_other_devices_preserves_declared_actions() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_other_devices(
            "pi1",
            &[shared::DeviceEntry {
                id: "tap_dial".into(),
                device_type: shared::DeviceType::Switch,
                actions: vec!["button_1_press".into(), "dial_rotate_left_step".into()],
            }],
        );
        let snap = state.get_other_device_snapshot();
        assert_eq!(
            snap[0].actions,
            vec![
                "button_1_press".to_string(),
                "dial_rotate_left_step".to_string()
            ]
        );
    }

    #[test]
    fn push_other_devices_replaces_for_same_node() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_other_devices(
            "pi1",
            &[shared::DeviceEntry {
                id: "old_blind".into(),
                device_type: shared::DeviceType::Cover,
                actions: vec![],
            }],
        );
        state.push_other_devices(
            "pi1",
            &[shared::DeviceEntry {
                id: "new_blind".into(),
                device_type: shared::DeviceType::Cover,
                actions: vec![],
            }],
        );
        let snap = state.get_other_device_snapshot();
        assert_eq!(
            snap.len(),
            1,
            "old_blind should be replaced, not accumulated"
        );
        assert_eq!(snap[0].id, "new_blind");
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
            groups: vec![],
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
                groups: vec![],
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
            states: vec![],
            effect_id: None,
            effect_params: None,
            group_id: None,
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
                states: vec![],
                effect_id: None,
                effect_params: None,
                group_id: None,
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
                states: vec![],
                effect_id: None,
                effect_params: None,
                group_id: None,
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

    // ── art rotation: idle-timeout revert + general-batch cache ─────────────

    #[test]
    fn art_rotation_idle_for_false_immediately_after_engagement() {
        let state = make_state();
        state.set_art_rotation("q".into(), vec![]);
        assert!(!state.art_rotation_idle_for(std::time::Duration::from_secs(600)));
    }

    #[test]
    fn art_rotation_idle_for_true_with_zero_timeout() {
        let state = make_state();
        state.set_art_rotation("q".into(), vec![]);
        assert!(state.art_rotation_idle_for(std::time::Duration::ZERO));
    }

    #[test]
    fn art_rotation_idle_for_false_when_no_rotation_exists() {
        let state = make_state();
        assert!(!state.art_rotation_idle_for(std::time::Duration::ZERO));
    }

    #[tokio::test]
    async fn manual_advance_resets_idle_clock() {
        let state = make_state();
        state.set_art_rotation(
            "q".into(),
            vec![
                ArtRotationItem {
                    image_url: "1".into(),
                    title: "".into(),
                    artist: "".into(),
                    date: "".into(),
                    artist_bio: "".into(),
                },
                ArtRotationItem {
                    image_url: "2".into(),
                    title: "".into(),
                    artist: "".into(),
                    date: "".into(),
                    artist_bio: "".into(),
                },
            ],
        );
        let short = std::time::Duration::from_millis(40);
        tokio::time::sleep(short * 2).await;
        assert!(
            state.art_rotation_idle_for(short),
            "should read idle before any manual engagement"
        );
        state.manual_advance_art_rotation();
        assert!(
            !state.art_rotation_idle_for(short),
            "manual advance should have reset the idle clock"
        );
    }

    #[test]
    fn clear_art_rotation_removes_current_item() {
        let state = make_state();
        state.set_art_rotation(
            "q".into(),
            vec![ArtRotationItem {
                image_url: "1".into(),
                title: "".into(),
                artist: "".into(),
                date: "".into(),
                artist_bio: "".into(),
            }],
        );
        assert!(state.art_rotation_current_item().is_some());
        state.clear_art_rotation();
        assert!(state.art_rotation_current_item().is_none());
    }

    #[test]
    fn general_art_batch_cache_roundtrip() {
        let state = make_state();
        assert!(state.get_general_art_batch().is_none());
        state.set_general_art_batch(vec![ArtRotationItem {
            image_url: "1".into(),
            title: "".into(),
            artist: "".into(),
            date: "".into(),
            artist_bio: "".into(),
        }]);
        assert_eq!(state.get_general_art_batch().unwrap().len(), 1);
    }

    // ── Bluetooth paired-status volume tracking ──────────────────────────────

    #[test]
    fn set_bluetooth_volume_updates_only_the_volume_field() {
        let state = make_state();
        state.set_bluetooth_paired(
            "pi2",
            BluetoothPairedStatus {
                mac: "AA:BB:CC:DD:EE:FF".into(),
                name: "Fishman PA".into(),
                connected: true,
                volume_pct: Some(20),
            },
        );
        state.set_bluetooth_volume("pi2", 45);
        let status = state.bluetooth_paired_status("pi2").unwrap();
        assert_eq!(status.mac, "AA:BB:CC:DD:EE:FF");
        assert_eq!(status.name, "Fishman PA");
        assert!(status.connected);
        assert_eq!(status.volume_pct, Some(45));
    }

    #[test]
    fn set_bluetooth_volume_is_a_noop_when_nothing_paired() {
        let state = make_state();
        state.set_bluetooth_volume("pi2", 45);
        assert!(state.bluetooth_paired_status("pi2").is_none());
    }

    // ── ebay_hunt_generation / bump / remove ─────────────────────────────────

    #[test]
    fn bump_ebay_hunt_generation_returns_the_new_value_not_the_old_one() {
        let state = make_state();
        assert_eq!(state.bump_ebay_hunt_generation("h1"), 1);
        assert_eq!(state.bump_ebay_hunt_generation("h1"), 2);
    }

    #[test]
    fn ebay_hunt_generation_is_independent_per_hunt() {
        let state = make_state();
        state.bump_ebay_hunt_generation("h1");
        state.bump_ebay_hunt_generation("h1");
        assert_eq!(
            state
                .ebay_hunt_generation("h1")
                .load(std::sync::atomic::Ordering::SeqCst),
            2
        );
        assert_eq!(
            state
                .ebay_hunt_generation("h2")
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
    }

    #[test]
    fn remove_ebay_hunt_generation_resets_a_fresh_lookup_to_zero() {
        let state = make_state();
        state.bump_ebay_hunt_generation("h1");
        state.bump_ebay_hunt_generation("h1");
        state.remove_ebay_hunt_generation("h1");
        // A stale timer's captured generation (>= 1) can never match a
        // freshly re-created counter's 0 — the leak fix can't silently
        // reintroduce a false "still current" match.
        assert_eq!(
            state
                .ebay_hunt_generation("h1")
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
    }
}
