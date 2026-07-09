use crate::{
    HardwareSpec, HeartbeatPayload, ModelLifecycleState, NodeCapabilities, NodeRole, VersionInfo,
};
use serde::{Deserialize, Serialize};

pub const WIRE_VERSION: u32 = 10;

fn default_wire_version() -> u32 {
    WIRE_VERSION
}

/// Role of a single chat turn. Wire strings match the OpenAI role names
/// (`"system"` / `"user"` / `"assistant"`) so inbound OpenAI requests and the
/// outbound llama-server / cloud-provider calls serialize identically.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    System,
    User,
    Assistant,
}

/// One turn of a chat conversation, passed to the model verbatim so
/// llama-server can apply the model's chat template per role.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatTurn {
    pub role: ChatRole,
    pub content: String,
}

impl ChatTurn {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::System,
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::User,
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: ChatRole::Assistant,
            content: content.into(),
        }
    }
}

/// Sent by the coordinator to a Compute node to run inference.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InferenceRequest {
    pub request_id: String,
    /// Caller-supplied target node. `None` means "let the scheduler decide".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    pub model_name: String,
    /// Full conversation, forwarded to the model as-is (system prompt included).
    pub messages: Vec<ChatTurn>,
    /// When true the agent streams `ModelInferenceChunk` messages as tokens
    /// arrive, then sends the usual `ModelInferenceResult` as the terminator.
    pub stream: bool,
    pub max_tokens: u32,
    /// Sampling temperature. `None` lets the agent use its default (0.8).
    /// Set to `0.0` for greedy/deterministic output (faster, better for JSON).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default = "default_wire_version")]
    pub wire_version: u32,
}

/// One streamed token batch, sent by a Compute node while a streaming
/// inference is in flight. The stream is terminated by the usual
/// `InferenceResult` (which carries totals and any error).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InferenceChunk {
    pub request_id: String,
    pub node_id: String,
    /// Incremental output text (one or more tokens).
    pub delta: String,
    #[serde(default = "default_wire_version")]
    pub wire_version: u32,
}

/// Returned by a Compute node after completing (or failing) inference.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InferenceResult {
    pub request_id: String,
    pub node_id: String,
    pub model_name: String,
    pub output: String,
    pub tokens_generated: u32,
    /// Prompt tokens consumed, as reported by the serving backend (`usage.prompt_tokens`).
    #[serde(default)]
    pub prompt_tokens: u32,
    pub duration_ms: u64,
    #[serde(default)]
    pub prompt_eval_ms: u64,
    pub error: Option<String>,
    #[serde(default = "default_wire_version")]
    pub wire_version: u32,
}

/// Coordinator instructs a node to load a model into memory.
/// `node_id` is optional — if absent the coordinator picks the best-fit node automatically.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelLoadRequest {
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    pub model_name: String,
    pub model_size_mb: u64,
    #[serde(default = "default_wire_version")]
    pub wire_version: u32,
}

/// Coordinator instructs a node to evict a model from memory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelUnloadRequest {
    pub request_id: String,
    pub node_id: String,
    pub model_name: String,
    #[serde(default = "default_wire_version")]
    pub wire_version: u32,
}

/// Agent reports current model lifecycle state to the coordinator.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelStatusReport {
    pub node_id: String,
    pub model_name: String,
    pub size_mb: u64,
    pub state: ModelLifecycleState,
    #[serde(default = "default_wire_version")]
    pub wire_version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NodeRecordLite {
    pub id: String,
    pub hostname: String,
    pub ip: String,
    pub role: NodeRole,
    pub last_heartbeat_ms: u128,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelAllocationFull {
    pub model_name: String,
    pub size_mb: u64,
    pub state: ModelLifecycleState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NodeRecordFull {
    pub id: String,
    pub hostname: String,
    pub ip: String,
    pub role: NodeRole,
    pub last_heartbeat_ms: u128,
    pub hardware: Option<HardwareSpec>,
    pub capabilities: Option<NodeCapabilities>,
    pub models: Vec<ModelAllocationFull>,
}

// ── Lighting types ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum LightTarget {
    Group(String),
    Device(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum LightAction {
    On,
    Off,
    Toggle,
    Brightness(u8),
    /// Brightness with a hardware transition time (seconds). Bulb interpolates smoothly.
    BrightnessTransition {
        value: u8,
        transition_secs: f32,
    },
    ColorXY {
        x: f32,
        y: f32,
    },
    ColorTemp(u16), // mireds
    ColorTempTransition {
        value: u16,
        transition_secs: f32,
    },
    ColorXYTransition {
        x: f32,
        y: f32,
        transition_secs: f32,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LightCommandRequest {
    pub request_id: String,
    pub target: LightTarget,
    pub command: LightAction,
}

// ── Audio output (Phase 2/3/6 of plans/audio-output-integration.md) ────────────

/// Coordinator → a specific audio-capable node: fetch `url` and play it on
/// whatever local sink that node is configured for (Bluetooth speaker or
/// HDMI out — see `capabilities/audio`). Dumb and node-specific, mirrors
/// `LightCommandRequest`; target resolution (which node, if any) happens
/// coordinator-side before this is sent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AudioPlayRequest {
    pub request_id: String,
    pub url: String,
    /// Which of the node's configured backends to play through — a node
    /// can run more than one at once (e.g. HDMI to a TV *and* Bluetooth to
    /// a room speaker on the same Pi), so this disambiguates. `None` uses
    /// that node's default (its first configured `AUDIO_BACKENDS` entry).
    pub sink: Option<String>,
}

/// Node → coordinator: whether an `AudioPlayRequest` actually played.
/// Without this, the coordinator only knows the message reached a
/// connected node — not whether `aplay`/`paplay` succeeded — so a
/// misconfigured or unpaired sink reports false "delivered" and the
/// voice pipeline's puck fallback never fires. See `coordinator::audio`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AudioPlayResult {
    pub request_id: String,
    pub success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Any agent → coordinator: "play this clip somewhere," letting the
/// coordinator resolve *where* (it holds the room/sink registry an agent
/// doesn't have direct access to). `room: None` with `broadcast: false` is
/// invalid and treated as a no-op — callers must pick one.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AudioAnnounceRequest {
    pub request_id: String,
    pub url: String,
    /// Route to this room's configured sink (registry preference
    /// `room-audio-sink:<room>`). Ignored when `broadcast` is true.
    pub room: Option<String>,
    /// Send to every node advertising `Feature::Audio` at once, ignoring
    /// `room`. The mechanism this exists for: alerts/announcements that
    /// should reach the whole house, not one room.
    pub broadcast: bool,
}

/// Coordinator → a voice-capable node (the one running Piper): synthesize
/// `text` and hand back a fetchable URL, the same way the voice pipeline's
/// own replies are produced — this is what lets a *coordinator-initiated*
/// announcement (not one that started as a spoken request) get a voice at
/// all. Mirrors `IntentRequest`/`IntentResponse`'s pending-request-id
/// pattern.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TtsRequest {
    pub request_id: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TtsResponse {
    pub request_id: String,
    pub url: Option<String>,
    pub error: Option<String>,
}

/// Coordinator → the requesting node: whether an `AudioAnnounceRequest`
/// actually reached a connected sink. Lets a caller like the voice
/// pipeline fall back to a different sink (the puck's own speaker) when
/// its preferred room sink turns out to be configured but unreachable,
/// instead of the reply silently going nowhere.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AudioAnnounceResult {
    pub request_id: String,
    pub delivered: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LightStateReport {
    pub node_id: String,
    pub device_id: String,
    pub on: bool,
    pub brightness: Option<u8>,
    pub color_xy: Option<(f32, f32)>,
    pub color_temp: Option<u16>,
    /// `false` when the Zigbee device has gone offline (power-cycled or out of range).
    /// Defaults to `true` so existing serialised messages without this field are treated as online.
    #[serde(default = "default_true")]
    pub online: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SceneLoadRequest {
    pub request_id: String,
    pub scene_name: String,
    pub transition_ms: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SceneLoadedReport {
    pub request_id: String,
    pub scene_name: String,
    pub success: bool,
    pub error: Option<String>,
}

/// Device class, classified from z2m's `definition.exposes` at discovery.
/// Decides which capability crate handles the device and which widget the
/// dashboard renders — never the room it lives in.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum DeviceType {
    Light,
    Sensor,
    Cover,
    Climate,
    Switch,
    Unknown,
}

impl DeviceType {
    /// The wire/storage string — identical to the serde representation.
    pub fn as_str(&self) -> &'static str {
        match self {
            DeviceType::Light => "light",
            DeviceType::Sensor => "sensor",
            DeviceType::Cover => "cover",
            DeviceType::Climate => "climate",
            DeviceType::Switch => "switch",
            DeviceType::Unknown => "unknown",
        }
    }

    pub fn parse(s: &str) -> DeviceType {
        match s {
            "light" => DeviceType::Light,
            "sensor" => DeviceType::Sensor,
            "cover" => DeviceType::Cover,
            "climate" => DeviceType::Climate,
            "switch" => DeviceType::Switch,
            _ => DeviceType::Unknown,
        }
    }
}

/// One discovered device: friendly name + its classified type.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeviceEntry {
    pub id: String,
    pub device_type: DeviceType,
    /// The device's declared z2m action vocabulary (its `action` enum's
    /// `values` from `definition.exposes`) — every button press / dial
    /// gesture this model can ever emit, e.g. `["on_press", "dial_rotate_
    /// left_step", …]`. Only switches have these; empty for everything
    /// else. Lets the dashboard offer a real pick-list for bindings
    /// instead of making the user press buttons to discover names.
    #[serde(default)]
    pub actions: Vec<String>,
}

/// One sensor's latest readings, pushed by a sensors node whenever the
/// device publishes. All measurement fields optional — devices carry
/// different subsets (a temp/humidity sensor has no occupancy; a motion
/// sensor has no temperature).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SensorReport {
    pub node_id: String,
    pub device_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub humidity: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub battery: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occupancy: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contact: Option<bool>,
    /// Ambient light in lux (motion sensors with a light sensor, e.g. the
    /// SNZB-03P R2 — its base-model sibling reports a dim/bright enum
    /// instead and is not covered here).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub illuminance: Option<f32>,
    pub online: bool,
}

/// Coordinator asks the Zigbee bridge to open its pairing window (bridge-wide
/// — Zigbee pairing is not device-type specific). Fire-and-forget: feedback
/// arrives as `ZigbeeJoin` events while the window is open.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PermitJoinRequest {
    pub request_id: String,
    /// Window length; z2m caps at 254 s.
    pub seconds: u8,
}

/// Coordinator asks the bridge to remove a device from the Zigbee network.
/// Without this a "deleted" device re-announces and reappears in the next
/// `bridge/devices` publish. Fire-and-forget: z2m republishes the device
/// list after removal, which flows back as the usual `DeviceList`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeviceRemoveRequest {
    pub request_id: String,
    pub device_id: String,
}

/// Coordinator asks a specific audio-capable node to open a live Bluetooth
/// discovery window — unlike Zigbee's bridge-wide permit-join, this is
/// per-node since Bluetooth capability is tied to whichever Pi has
/// `bluetooth` in its `AUDIO_BACKENDS`. Feedback streams back as
/// `BluetoothDeviceFound` while the window is open.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BluetoothScanRequest {
    pub request_id: String,
    pub seconds: u8,
}

/// One device seen (or updated) during a Bluetooth scan, forwarded by the
/// scanning node — drives the dashboard's live device list with signal
/// bars. Sent once per new device and again whenever `bluetoothctl`
/// reports an updated RSSI for one already seen.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BluetoothDeviceInfo {
    pub node_id: String,
    pub mac: String,
    pub name: String,
    /// dBm; `None` until BlueZ reports one for this device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rssi: Option<i32>,
}

/// Coordinator asks a node to pair, trust, and connect a specific
/// previously-scanned MAC, and adopt it as that node's bluetooth sink.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BluetoothPairRequest {
    pub request_id: String,
    pub mac: String,
}

/// Node → coordinator: the outcome of a `BluetoothPairRequest`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BluetoothPairResult {
    pub node_id: String,
    pub mac: String,
    pub name: String,
    pub success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The resolved PipeWire/PulseAudio sink name on success (e.g.
    /// `bluez_sink.AA_BB_CC_DD_EE_FF.a2dp_sink`) — persisted node-side and
    /// used for playback instead of relying on the OS default sink.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sink_name: Option<String>,
}

/// One z2m `bridge/event` during pairing, forwarded by the zigbee-owning
/// node: drives the dashboard's live "joined: <model>" feed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ZigbeeJoinEvent {
    pub node_id: String,
    /// "device_joined" | "device_interview_started" |
    /// "device_interview_successful" | "device_interview_failed" |
    /// "device_announce" | "device_leave"
    pub event: String,
    pub device_id: String,
    /// Model name from the interview definition (only on interview success).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// One button press / dial rotation from a Switch-class device (e.g. the Hue
/// Tap Dial), forwarded by the zigbee-owning node. Purely a transient UI
/// indicator — Switch devices have no persisted state, so this is broadcast
/// and forgotten rather than stored in a snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SwitchActionReport {
    pub node_id: String,
    pub device_id: String,
    /// Raw z2m `action` value, e.g. "button_1_press" or "1_rotate_left".
    pub action: String,
}

/// Full device inventory for a node's Zigbee bridge, sent on every MQTT
/// (re)connect. Typed per device; `groups` stays lighting-specific (Z2M
/// groups are a lighting concept).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeviceListReport {
    pub node_id: String,
    pub devices: Vec<DeviceEntry>,
    /// Friendly names of Z2M groups (e.g. "all").
    pub groups: Vec<String>,
}

// ── REAPER capability types ───────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReaperCommandRequest {
    pub request_id: String,
    /// Named transport action ("play" | "stop" | "pause" | "record" | "rewind")
    /// or a numeric action ID as a string ("40075") or a named SWS action ("_SWS_ABOUT").
    /// "seek" is reserved for future use and is not exposed via the tool schema.
    pub action: String,
    /// Sparse extra params. Currently unused; reserved for future "seek" support.
    pub params: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReaperCommandResult {
    pub request_id: String,
    pub ok: bool,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReaperStatusReport {
    pub node_id: String,
    pub reaper_online: bool,
    /// 0=stopped, 1=playing, 2=paused, 5=recording
    pub play_state: u8,
    pub position: f64,
    pub tempo: f64,
    pub ts_num: u32,
    pub ts_denom: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReaperScriptRequest {
    pub request_id: String,
    /// Lua code to execute inside REAPER's scripting environment.
    pub code: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReaperScriptResult {
    pub request_id: String,
    pub ok: bool,
    pub message: String,
}

// ── Frame TV art-display capability types ─────────────────────────────────────
// v1 (ArtShow/ArtStatus) was deliberately minimal (see
// plans/frame-tv-art-display.md §10): just enough to prove the physical
// chain (coordinator → node → TV) works. Coordinator-driven rotation
// (search/curate/auto-advance, all via repeated ArtShow) followed the same
// day. ArtBatch is the next step: a *general* slideshow (no specific
// search) hands the node a whole list up front so it can cycle locally —
// no coordinator round-trip per image, survives a coordinator restart —
// while a specific search still overrides via plain ArtShow and the
// coordinator reverts to general mode (re-sending ArtBatch) after an idle
// timeout. Still no ArtMode — that needs the TV-control channel, which
// doesn't exist yet; adding it ahead of a real consumer would be
// speculative.

/// Coordinator → art node: here's a whole set of images to cycle through
/// *locally*, one every `interval_secs`, wrapping at the end — no further
/// coordinator involvement needed per image. Used for the general/default
/// slideshow (see `docs/frame-tv-setup.md`); a specific search still uses
/// plain `ArtShow` messages, which take precedence until the coordinator
/// decides (after an idle timeout) to send a fresh `ArtBatch` and hand
/// control back to the node's own loop.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ArtBatchRequest {
    pub request_id: String,
    pub image_urls: Vec<String>,
    pub interval_secs: u64,
}

/// Coordinator → art node: fetch `image_url` and display it fullscreen,
/// replacing whatever's currently shown.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ArtShowRequest {
    pub request_id: String,
    pub image_url: String,
}

/// Art node → coordinator: result of the most recent `ArtShow`, and whether
/// the fullscreen viewer process is currently running at all.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ArtStatusReport {
    pub node_id: String,
    pub viewer_running: bool,
    /// The image currently on screen, if any (mirrors what was last shown —
    /// absent right after boot, before the first ArtShow arrives).
    pub current_url: Option<String>,
    /// Set when the most recent ArtShow failed (download or viewer-launch
    /// error) — the previous image (if any) is left on screen rather than
    /// blanking the display.
    pub error: Option<String>,
}

// ── Intent routing types ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum IntentRole {
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IntentTurn {
    pub role: IntentRole,
    pub content: String,
}

/// Where an intent originated. Lets the coordinator route the *response*
/// per source — voice exchanges are broadcast to dashboard consumers (chat
/// window today, a TTS/speaker output sink later) while CLI/dashboard
/// requests already return to their caller.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum IntentSource {
    Voice,
    Cli,
    Dashboard,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IntentRequest {
    pub request_id: String,
    pub text: String,
    pub model_name: Option<String>,
    pub context: Vec<IntentTurn>,
    pub source: IntentSource,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCallRecord {
    pub tool: String,
    pub args: serde_json::Value,
    pub result: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IntentResponse {
    pub request_id: String,
    pub node_id: String,
    pub model_name: String,
    pub text: Option<String>,
    pub tool_calls: Vec<ToolCallRecord>,
    pub error: Option<String>,
    /// Token-generation (decode) time reported by the node — not the whole request.
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default)]
    pub tokens_generated: u32,
    /// Prompt-prefill time reported by the node.
    #[serde(default)]
    pub prompt_eval_ms: u64,
    /// End-to-end wall time measured by the coordinator: inference dispatch +
    /// generation + tool execution + parsing. The number that matches what a
    /// client actually waited for.
    #[serde(default)]
    pub total_ms: u64,
    /// Whether prompt compression was actually applied to the context (Phase A:
    /// measured in shadow — the local prompt is unchanged).
    #[serde(default)]
    pub compression_applied: bool,
    /// Estimated context tokens before compression (0 when not measured).
    #[serde(default)]
    pub prompt_tokens_before: u32,
    /// Estimated context tokens after compression (0 when not measured).
    #[serde(default)]
    pub prompt_tokens_after: u32,
}

// ── MeshMessage ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum MeshMessage {
    Heartbeat(HeartbeatPayload),
    Capabilities(NodeCapabilities),
    HardwareReport(HardwareSpec),
    UpdateAvailable(VersionInfo),
    RequestUpdate,
    Ping,
    Acknowledge,
    RequestNodes,
    NodeList(Vec<NodeRecordLite>),
    RequestNodeInfo(String),
    NodeInfo(NodeRecordFull),
    // Phase 6 — model scheduling
    RequestModelInference(InferenceRequest),
    ModelInferenceResult(InferenceResult),
    ModelInferenceChunk(InferenceChunk),
    /// Coordinator → Compute node: abort an in-flight (streaming) inference.
    /// Sent when the consumer of a stream is gone (client hang-up, emitter
    /// timeout, buffer overflow) so the node frees its inference slot instead
    /// of generating to completion for nobody.
    CancelInference {
        request_id: String,
    },
    ModelLoad(ModelLoadRequest),
    ModelUnload(ModelUnloadRequest),
    ModelStatus(ModelStatusReport),
    // Admin messages
    Admin(AdminMessage),
    // Coordinator → caller: generic error response
    Error(String),
    // Lighting capability messages
    LightCommand(LightCommandRequest),
    LightState(LightStateReport),
    DeviceList(DeviceListReport),
    SensorState(SensorReport),
    PermitJoin(PermitJoinRequest),
    DeviceRemove(DeviceRemoveRequest),
    ZigbeeJoin(ZigbeeJoinEvent),
    BluetoothScan(BluetoothScanRequest),
    BluetoothDeviceFound(BluetoothDeviceInfo),
    BluetoothPair(BluetoothPairRequest),
    BluetoothPairResult(BluetoothPairResult),
    SwitchAction(SwitchActionReport),
    SceneLoad(SceneLoadRequest),
    SceneLoaded(SceneLoadedReport),
    // Intent routing
    IntentRequest(IntentRequest),
    IntentResponse(IntentResponse),
    // Audio output (Phase 2/3/6 — plans/audio-output-integration.md)
    AudioPlay(AudioPlayRequest),
    AudioPlayResult(AudioPlayResult),
    AudioAnnounce(AudioAnnounceRequest),
    AudioAnnounceResult(AudioAnnounceResult),
    TtsRequest(TtsRequest),
    TtsResponse(TtsResponse),
    // Phase 10 — auth: sent as the first message on every new connection
    AuthToken(String),
    // Phase 11C — coordinator pushes a new heartbeat interval to a specific node
    SetHeartbeatInterval {
        secs: u64,
    },
    // Zigbee bridge up/down — emitted by the lighting capability when MQTT
    // connection to zigbee2mqtt is lost or restored
    ZigbeeStatus {
        online: bool,
    },
    // REAPER DAW capability messages
    ReaperCommand(ReaperCommandRequest),
    ReaperCommandResult(ReaperCommandResult),
    ReaperStatus(ReaperStatusReport),
    ReaperScript(ReaperScriptRequest),
    ReaperScriptResult(ReaperScriptResult),
    // Frame TV art-display capability messages (see plans/frame-tv-art-display.md)
    ArtShow(ArtShowRequest),
    ArtStatus(ArtStatusReport),
    ArtBatch(ArtBatchRequest),
}

/// Structured admin messages for coordinator control.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AdminMessage {
    ResetRegistry,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NodeIdentity, NodeRole};

    #[test]
    fn test_serialize_heartbeat() {
        let identity = NodeIdentity {
            id: "node-1".into(),
            hostname: "test-host".into(),
            ip: "192.168.1.16".into(),
            role: NodeRole::Compute,
        };
        let payload = HeartbeatPayload {
            identity: identity.clone(),
            auth_token: String::new(),
            cpu_usage_pct: 0.0,
            ram_used_gb: 0.0,
            ram_total_gb: 0.0,
            gpu_usage_pct: None,
            gpu_vram_used_gb: None,
            gpu_vram_total_gb: None,
            disk_free_gb: None,
        };

        let msg = MeshMessage::Heartbeat(payload.clone());
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: MeshMessage = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded, MeshMessage::Heartbeat(payload));
    }

    #[test]
    fn test_heartbeat_with_auth_token_roundtrip() {
        let payload = HeartbeatPayload {
            identity: NodeIdentity {
                id: "node-2".into(),
                hostname: "secure-host".into(),
                ip: "10.0.0.1".into(),
                role: NodeRole::Compute,
            },
            auth_token: "secret-token".into(),
            cpu_usage_pct: 0.0,
            ram_used_gb: 0.0,
            ram_total_gb: 0.0,
            gpu_usage_pct: None,
            gpu_vram_used_gb: None,
            gpu_vram_total_gb: None,
            disk_free_gb: None,
        };
        let msg = MeshMessage::Heartbeat(payload.clone());
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: MeshMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, MeshMessage::Heartbeat(payload));
    }

    #[test]
    fn test_heartbeat_token_always_serialized() {
        let payload = HeartbeatPayload {
            identity: NodeIdentity {
                id: "node-3".into(),
                hostname: "host".into(),
                ip: "10.0.0.22".into(),
                role: NodeRole::Compute,
            },
            auth_token: String::new(),
            cpu_usage_pct: 0.0,
            ram_used_gb: 0.0,
            ram_total_gb: 0.0,
            gpu_usage_pct: None,
            gpu_vram_used_gb: None,
            gpu_vram_total_gb: None,
            disk_free_gb: None,
        };
        let json = serde_json::to_string(&MeshMessage::Heartbeat(payload)).unwrap();
        assert!(
            json.contains("auth_token"),
            "auth_token must always appear in JSON"
        );
    }

    #[test]
    fn test_node_record_lite_roundtrip() {
        let record = NodeRecordLite {
            id: "node-1".into(),
            hostname: "host-1".into(),
            ip: "10.0.0.1".into(),
            role: NodeRole::Compute,
            last_heartbeat_ms: 42,
        };

        let json = serde_json::to_string(&record).unwrap();
        let decoded: NodeRecordLite = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded, record);
    }

    #[test]
    fn test_node_record_full_roundtrip() {
        let record = NodeRecordFull {
            id: "node-2".into(),
            hostname: "host-2".into(),
            ip: "10.0.0.22".into(),
            role: NodeRole::Controller,
            last_heartbeat_ms: 100,
            hardware: None,
            capabilities: None,
            models: vec![],
        };

        let json = serde_json::to_string(&record).unwrap();
        let decoded: NodeRecordFull = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded, record);
    }

    #[test]
    fn test_request_node_info_roundtrip() {
        let msg = MeshMessage::RequestNodeInfo("node-3".into());
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: MeshMessage = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded, MeshMessage::RequestNodeInfo("node-3".into()));
    }

    #[test]
    fn test_node_info_roundtrip() {
        let record = NodeRecordFull {
            id: "node-4".into(),
            hostname: "host-4".into(),
            ip: "10.0.0.23".into(),
            role: NodeRole::Compute,
            last_heartbeat_ms: 200,
            hardware: None,
            capabilities: None,
            models: vec![],
        };

        let msg = MeshMessage::NodeInfo(record.clone());
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: MeshMessage = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded, MeshMessage::NodeInfo(record));
    }

    #[test]
    fn node_record_lite_roundtrip() {
        let rec = NodeRecordLite {
            id: "node-1".into(),
            hostname: "test-node".into(),
            ip: "10.0.0.1".into(),
            role: NodeRole::Compute,
            last_heartbeat_ms: 1234,
        };

        let json = serde_json::to_string(&rec).unwrap();
        let back: NodeRecordLite = serde_json::from_str(&json).unwrap();

        assert_eq!(back.id, rec.id);
        assert_eq!(back.hostname, rec.hostname);
        assert_eq!(back.ip, rec.ip);
        assert_eq!(back.last_heartbeat_ms, rec.last_heartbeat_ms);
    }

    #[test]
    fn node_record_full_roundtrip() {
        let hw = HardwareSpec {
            cpu_model: "AMD Ryzen AI 5 340 w/ Radeon 840M".into(),
            cpu_cores: 12,
            cpu_threads: 12,
            ram_gb: 7.39,
            os: "linux".into(),
            arch: "x86_64".into(),
            gpu: None,
        };

        let caps = NodeCapabilities {
            cpu_inference: true,
            gpu_inference: false,
            ane_inference: false,
            max_model_size_gb: 3.69,
            features: vec![crate::Feature::Llm],
            audio_backends: vec![],
        };

        let rec = NodeRecordFull {
            id: "node-1".into(),
            hostname: "test-node".into(),
            ip: "10.0.0.1".into(),
            role: NodeRole::Compute,
            last_heartbeat_ms: 500,
            hardware: Some(hw),
            capabilities: Some(caps),
            models: vec![],
        };

        let json = serde_json::to_string(&rec).unwrap();
        let back: NodeRecordFull = serde_json::from_str(&json).unwrap();

        assert_eq!(back.id, rec.id);
        assert_eq!(back.hostname, rec.hostname);
        assert_eq!(back.ip, rec.ip);
        assert_eq!(back.role, rec.role);
        assert_eq!(back.last_heartbeat_ms, rec.last_heartbeat_ms);
        assert!(back.hardware.is_some());
        assert!(back.capabilities.is_some());
    }

    #[test]
    fn request_node_info_roundtrip() {
        let msg = MeshMessage::RequestNodeInfo("node-1".into());

        let json = serde_json::to_string(&msg).unwrap();
        let back: MeshMessage = serde_json::from_str(&json).unwrap();

        match back {
            MeshMessage::RequestNodeInfo(id) => assert_eq!(id, "node-1"),
            other => panic!("Unexpected variant: {:?}", other),
        }
    }

    #[test]
    fn node_info_roundtrip() {
        let rec = NodeRecordFull {
            id: "node-1".into(),
            hostname: "test-node".into(),
            ip: "10.0.0.1".into(),
            role: NodeRole::Compute,
            last_heartbeat_ms: 500,
            hardware: None,
            capabilities: None,
            models: vec![],
        };

        let msg = MeshMessage::NodeInfo(rec);

        let json = serde_json::to_string(&msg).unwrap();
        let back: MeshMessage = serde_json::from_str(&json).unwrap();

        match back {
            MeshMessage::NodeInfo(info) => {
                assert_eq!(info.id, "node-1");
                assert_eq!(info.hostname, "test-node");
            }
            other => panic!("Unexpected variant: {:?}", other),
        }
    }

    #[test]
    fn error_roundtrip() {
        let msg = MeshMessage::Error("no node ready".into());
        let json = serde_json::to_string(&msg).unwrap();
        let back: MeshMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back, MeshMessage::Error("no node ready".into()));
    }

    #[test]
    fn admin_reset_registry_roundtrip() {
        let msg = MeshMessage::Admin(AdminMessage::ResetRegistry);
        let json = serde_json::to_string(&msg).unwrap();
        let back: MeshMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn wire_version_defaults_when_field_absent() {
        // Simulate an older agent that sends InferenceRequest without wire_version.
        let json = r#"{"request_id":"r1","node_id":"n1","model_name":"llama","messages":[{"role":"user","content":"hi"}],"stream":false,"max_tokens":64}"#;
        let req: InferenceRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.wire_version, WIRE_VERSION);
    }

    #[test]
    fn inference_request_without_stream_fails_fast() {
        // `stream` is deliberately required (no serde default): a v3 frame
        // missing it must fail to deserialize rather than default silently.
        let json = r#"{"request_id":"r1","model_name":"llama","messages":[{"role":"user","content":"hi"}],"max_tokens":64}"#;
        assert!(serde_json::from_str::<InferenceRequest>(json).is_err());
    }

    #[test]
    fn cancel_inference_roundtrip() {
        let msg = MeshMessage::CancelInference {
            request_id: "chatcmpl-1".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(serde_json::from_str::<MeshMessage>(&json).unwrap(), msg);
    }

    #[test]
    fn inference_chunk_roundtrip() {
        let msg = MeshMessage::ModelInferenceChunk(InferenceChunk {
            request_id: "chatcmpl-1".into(),
            node_id: "node-1".into(),
            delta: "hel".into(),
            wire_version: WIRE_VERSION,
        });
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(serde_json::from_str::<MeshMessage>(&json).unwrap(), msg);
    }

    #[test]
    fn inference_request_roundtrip() {
        let req = InferenceRequest {
            request_id: "req-1".into(),
            node_id: Some("node-1".into()),
            model_name: "llama3".into(),
            messages: vec![ChatTurn::user("hello")],
            stream: false,
            max_tokens: 128,
            temperature: None,
            wire_version: WIRE_VERSION,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(
            serde_json::from_str::<InferenceRequest>(&json).unwrap(),
            req
        );
    }

    #[test]
    fn inference_request_multi_turn_roundtrip() {
        let req = InferenceRequest {
            request_id: "req-2".into(),
            node_id: None,
            model_name: "qwen2.5:7b".into(),
            messages: vec![
                ChatTurn::system("You are a controller."),
                ChatTurn::user("turn light on"),
                ChatTurn::assistant("done"),
                ChatTurn::user("and the other one"),
            ],
            stream: true,
            max_tokens: 128,
            temperature: Some(0.0),
            wire_version: WIRE_VERSION,
        };
        let json = serde_json::to_string(&req).unwrap();
        // Role strings must match the OpenAI names on the wire.
        assert!(json.contains(r#""role":"system""#));
        assert!(json.contains(r#""role":"user""#));
        assert!(json.contains(r#""role":"assistant""#));
        assert!(json.contains("temperature"));
        assert!(
            !json.contains("node_id"),
            "node_id should be omitted when None"
        );
        assert_eq!(
            serde_json::from_str::<InferenceRequest>(&json).unwrap(),
            req
        );
    }

    #[test]
    fn inference_result_roundtrip() {
        let res = InferenceResult {
            request_id: "req-1".into(),
            node_id: "node-1".into(),
            model_name: "llama3".into(),
            output: "world".into(),
            tokens_generated: 1,
            prompt_tokens: 12,
            duration_ms: 42,
            prompt_eval_ms: 0,
            error: None,
            wire_version: WIRE_VERSION,
        };
        let json = serde_json::to_string(&res).unwrap();
        assert_eq!(serde_json::from_str::<InferenceResult>(&json).unwrap(), res);
    }

    #[test]
    fn model_load_request_roundtrip() {
        let req = ModelLoadRequest {
            request_id: "req-2".into(),
            node_id: Some("node-1".into()),
            model_name: "llama3".into(),
            model_size_mb: 4096,
            wire_version: WIRE_VERSION,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(
            serde_json::from_str::<ModelLoadRequest>(&json).unwrap(),
            req
        );
    }

    #[test]
    fn model_load_request_no_node_id_roundtrip() {
        let req = ModelLoadRequest {
            request_id: "req-auto".into(),
            node_id: None,
            model_name: "qwen2.5:7b".into(),
            model_size_mb: 4096,
            wire_version: WIRE_VERSION,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(!json.contains("node_id"));
        assert_eq!(
            serde_json::from_str::<ModelLoadRequest>(&json).unwrap(),
            req
        );
    }

    #[test]
    fn model_unload_request_roundtrip() {
        let req = ModelUnloadRequest {
            request_id: "req-3".into(),
            node_id: "node-1".into(),
            model_name: "llama3".into(),
            wire_version: WIRE_VERSION,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(
            serde_json::from_str::<ModelUnloadRequest>(&json).unwrap(),
            req
        );
    }

    #[test]
    fn model_status_report_roundtrip() {
        use crate::ModelLifecycleState;
        let rep = ModelStatusReport {
            node_id: "node-1".into(),
            model_name: "llama3".into(),
            size_mb: 4096,
            state: ModelLifecycleState::Ready,
            wire_version: WIRE_VERSION,
        };
        let json = serde_json::to_string(&rep).unwrap();
        assert_eq!(
            serde_json::from_str::<ModelStatusReport>(&json).unwrap(),
            rep
        );
    }

    // ── Lighting type tests ───────────────────────────────────────────────────

    #[test]
    fn light_command_on_roundtrip() {
        let msg = MeshMessage::LightCommand(LightCommandRequest {
            request_id: "lc-1".into(),
            target: LightTarget::Group("all".into()),
            command: LightAction::On,
        });
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(serde_json::from_str::<MeshMessage>(&json).unwrap(), msg);
    }

    #[test]
    fn light_command_color_xy_roundtrip() {
        let msg = MeshMessage::LightCommand(LightCommandRequest {
            request_id: "lc-2".into(),
            target: LightTarget::Device("0x00158d0001".into()),
            command: LightAction::ColorXY {
                x: 0.3127,
                y: 0.3290,
            },
        });
        let json = serde_json::to_string(&msg).unwrap();
        let back = serde_json::from_str::<MeshMessage>(&json).unwrap();
        assert_eq!(back, msg);
        // verify float precision survives the round-trip
        if let MeshMessage::LightCommand(req) = back {
            if let LightAction::ColorXY { x, y } = req.command {
                assert!((x - 0.3127_f32).abs() < f32::EPSILON);
                assert!((y - 0.3290_f32).abs() < f32::EPSILON);
            } else {
                panic!("wrong variant");
            }
        }
    }

    #[test]
    fn light_command_brightness_roundtrip() {
        let msg = MeshMessage::LightCommand(LightCommandRequest {
            request_id: "lc-3".into(),
            target: LightTarget::Group("living_room".into()),
            command: LightAction::Brightness(128),
        });
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(serde_json::from_str::<MeshMessage>(&json).unwrap(), msg);
    }

    #[test]
    fn light_state_report_roundtrip() {
        let msg = MeshMessage::LightState(LightStateReport {
            node_id: "pi1".into(),
            device_id: "bulb-1".into(),
            on: true,
            brightness: Some(200),
            color_xy: Some((0.3127, 0.3290)),
            color_temp: None,
            online: true,
        });
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(serde_json::from_str::<MeshMessage>(&json).unwrap(), msg);
    }

    #[test]
    fn scene_load_roundtrip() {
        let msg = MeshMessage::SceneLoad(SceneLoadRequest {
            request_id: "sl-1".into(),
            scene_name: "cozy".into(),
            transition_ms: 2000,
        });
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(serde_json::from_str::<MeshMessage>(&json).unwrap(), msg);
    }

    #[test]
    fn scene_loaded_roundtrip() {
        let msg = MeshMessage::SceneLoaded(SceneLoadedReport {
            request_id: "sl-1".into(),
            scene_name: "cozy".into(),
            success: true,
            error: None,
        });
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(serde_json::from_str::<MeshMessage>(&json).unwrap(), msg);
    }

    #[test]
    fn scene_loaded_with_error_roundtrip() {
        let msg = MeshMessage::SceneLoaded(SceneLoadedReport {
            request_id: "sl-2".into(),
            scene_name: "disco".into(),
            success: false,
            error: Some("unknown scene".into()),
        });
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(serde_json::from_str::<MeshMessage>(&json).unwrap(), msg);
    }

    #[test]
    fn sensor_state_roundtrip() {
        let msg = MeshMessage::SensorState(SensorReport {
            node_id: "pi1".into(),
            device_id: "office_temp".into(),
            temperature: Some(21.4),
            humidity: Some(47.0),
            battery: Some(98),
            occupancy: None,
            contact: None,
            illuminance: None,
            online: true,
        });
        let json = serde_json::to_string(&msg).unwrap();
        assert!(
            !json.contains("occupancy"),
            "None fields omitted on the wire"
        );
        assert_eq!(serde_json::from_str::<MeshMessage>(&json).unwrap(), msg);
    }

    #[test]
    fn sensor_state_illuminance_roundtrip() {
        // SNZB-03P R2 shape: occupancy + illuminance + battery, no temp/humidity/contact.
        let msg = MeshMessage::SensorState(SensorReport {
            node_id: "pi1".into(),
            device_id: "hall_motion".into(),
            temperature: None,
            humidity: None,
            battery: Some(100),
            occupancy: Some(true),
            contact: None,
            illuminance: Some(123.0),
            online: true,
        });
        let json = serde_json::to_string(&msg).unwrap();
        assert!(
            json.contains("\"illuminance\":123.0"),
            "missing illuminance: {json}"
        );
        assert!(!json.contains("temperature"), "None fields omitted: {json}");
        assert_eq!(serde_json::from_str::<MeshMessage>(&json).unwrap(), msg);
    }

    #[test]
    fn permit_join_roundtrip() {
        let msg = MeshMessage::PermitJoin(PermitJoinRequest {
            request_id: "r1".into(),
            seconds: 254,
        });
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(serde_json::from_str::<MeshMessage>(&json).unwrap(), msg);
    }

    #[test]
    fn device_remove_roundtrip() {
        let msg = MeshMessage::DeviceRemove(DeviceRemoveRequest {
            request_id: "r1".into(),
            device_id: "old_bulb".into(),
        });
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(serde_json::from_str::<MeshMessage>(&json).unwrap(), msg);
    }

    #[test]
    fn zigbee_join_roundtrip() {
        let msg = MeshMessage::ZigbeeJoin(ZigbeeJoinEvent {
            node_id: "pi1".into(),
            event: "device_interview_successful".into(),
            device_id: "0xabcdef".into(),
            model: Some("WSDCGQ11LM".into()),
        });
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(serde_json::from_str::<MeshMessage>(&json).unwrap(), msg);
        // model=None is omitted on the wire
        let msg = MeshMessage::ZigbeeJoin(ZigbeeJoinEvent {
            node_id: "pi1".into(),
            event: "device_joined".into(),
            device_id: "0xabcdef".into(),
            model: None,
        });
        let json = serde_json::to_string(&msg).unwrap();
        assert!(!json.contains("model"));
        assert_eq!(serde_json::from_str::<MeshMessage>(&json).unwrap(), msg);
    }

    #[test]
    fn switch_action_roundtrip() {
        let msg = MeshMessage::SwitchAction(SwitchActionReport {
            node_id: "pi1".into(),
            device_id: "tap_dial".into(),
            action: "button_1_press".into(),
        });
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(serde_json::from_str::<MeshMessage>(&json).unwrap(), msg);
    }

    #[test]
    fn device_list_roundtrip() {
        let msg = MeshMessage::DeviceList(DeviceListReport {
            node_id: "pi1".into(),
            devices: vec![
                DeviceEntry {
                    id: "test_bulb".into(),
                    device_type: DeviceType::Light,
                    actions: vec![],
                },
                DeviceEntry {
                    id: "hall_motion".into(),
                    device_type: DeviceType::Sensor,
                    actions: vec![],
                },
            ],
            groups: vec!["all".into()],
        });
        let json = serde_json::to_string(&msg).unwrap();
        // Types serialize as lowercase strings on the wire.
        assert!(json.contains(r#""device_type":"light""#));
        assert!(json.contains(r#""device_type":"sensor""#));
        assert_eq!(serde_json::from_str::<MeshMessage>(&json).unwrap(), msg);
    }

    // ── Intent type tests ─────────────────────────────────────────────────────

    #[test]
    fn intent_request_roundtrip() {
        let msg = MeshMessage::IntentRequest(IntentRequest {
            request_id: "ir-1".into(),
            text: "dim the living room lights".into(),
            model_name: None,
            context: vec![IntentTurn {
                role: IntentRole::User,
                content: "hello".into(),
            }],
            source: IntentSource::Voice,
        });
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(serde_json::from_str::<MeshMessage>(&json).unwrap(), msg);
    }

    #[test]
    fn intent_response_with_tool_call_roundtrip() {
        let msg = MeshMessage::IntentResponse(IntentResponse {
            request_id: "ir-1".into(),
            node_id: "pi1".into(),
            model_name: "qwen3:8b".into(),
            text: Some("Done — living room set to cozy.".into()),
            tool_calls: vec![ToolCallRecord {
                tool: "scene_load".into(),
                args: serde_json::json!({"scene": "cozy", "transition_ms": 2000}),
                result: Some("ok".into()),
            }],
            error: None,
            duration_ms: 1234,
            tokens_generated: 10,
            prompt_eval_ms: 0,
            total_ms: 1300,
            compression_applied: false,
            prompt_tokens_before: 0,
            prompt_tokens_after: 0,
        });
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(serde_json::from_str::<MeshMessage>(&json).unwrap(), msg);
    }

    #[test]
    fn intent_response_free_text_roundtrip() {
        let msg = MeshMessage::IntentResponse(IntentResponse {
            request_id: "ir-2".into(),
            node_id: "beelink1".into(),
            model_name: "qwen3:8b".into(),
            text: Some("TCP keepalive is a mechanism that...".into()),
            tool_calls: vec![],
            error: None,
            duration_ms: 800,
            tokens_generated: 42,
            prompt_eval_ms: 0,
            total_ms: 850,
            compression_applied: false,
            prompt_tokens_before: 0,
            prompt_tokens_after: 0,
        });
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(serde_json::from_str::<MeshMessage>(&json).unwrap(), msg);
    }

    #[test]
    fn set_heartbeat_interval_roundtrip() {
        let msg = MeshMessage::SetHeartbeatInterval { secs: 10 };
        let json = serde_json::to_string(&msg).unwrap();
        let back: MeshMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back, msg);
        assert!(json.contains("SetHeartbeatInterval"));
        assert!(json.contains("10"));
    }

    #[test]
    fn set_heartbeat_interval_wire_format() {
        // Pin the exact JSON shape so the agent parser never silently drifts.
        let msg = MeshMessage::SetHeartbeatInterval { secs: 30 };
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(json, r#"{"SetHeartbeatInterval":{"secs":30}}"#);
    }

    #[test]
    fn set_heartbeat_interval_boundary_values() {
        for secs in [0u64, 1, 300, u64::MAX] {
            let msg = MeshMessage::SetHeartbeatInterval { secs };
            let json = serde_json::to_string(&msg).unwrap();
            assert_eq!(serde_json::from_str::<MeshMessage>(&json).unwrap(), msg);
        }
    }
}
