use crate::{
    HardwareSpec, HeartbeatPayload, ModelLifecycleState, NodeCapabilities, NodeRole, VersionInfo,
};
use serde::{Deserialize, Serialize};

pub const WIRE_VERSION: u32 = 2;

fn default_wire_version() -> u32 {
    WIRE_VERSION
}

/// Sent by the coordinator to a Compute node to run inference.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InferenceRequest {
    pub request_id: String,
    /// Caller-supplied target node. `None` means "let the scheduler decide".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    pub model_name: String,
    /// Optional system prompt. When set the agent passes it in the system role
    /// and `prompt` goes in the user role. When absent, only a user role is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    pub prompt: String,
    pub max_tokens: u32,
    /// Sampling temperature. `None` lets the agent use its default (0.8).
    /// Set to `0.0` for greedy/deterministic output (faster, better for JSON).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
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
    pub duration_ms: u64,
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

/// Sent by a lighting node to inform the coordinator of known Z2M devices and groups.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LightDeviceListReport {
    pub node_id: String,
    /// Friendly names of individual Zigbee devices (e.g. "test_bulb").
    pub devices: Vec<String>,
    /// Friendly names of Z2M groups (e.g. "all").
    pub groups: Vec<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IntentRequest {
    pub request_id: String,
    pub text: String,
    pub model_name: Option<String>,
    pub context: Vec<IntentTurn>,
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
    pub text: Option<String>,
    pub tool_calls: Vec<ToolCallRecord>,
    pub error: Option<String>,
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
    LightDeviceList(LightDeviceListReport),
    SceneLoad(SceneLoadRequest),
    SceneLoaded(SceneLoadedReport),
    // Intent routing
    IntentRequest(IntentRequest),
    IntentResponse(IntentResponse),
    // Phase 10 — auth: sent as the first message on every new connection
    AuthToken(String),
    // Phase 11C — coordinator pushes a new heartbeat interval to a specific node
    SetHeartbeatInterval { secs: u64 },
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
            features: vec!["llm".into()],
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
        let json = r#"{"request_id":"r1","node_id":"n1","model_name":"llama","prompt":"hi","max_tokens":64}"#;
        let req: InferenceRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.wire_version, WIRE_VERSION);
    }

    #[test]
    fn inference_request_roundtrip() {
        let req = InferenceRequest {
            request_id: "req-1".into(),
            node_id: Some("node-1".into()),
            model_name: "llama3".into(),
            system_prompt: None,
            prompt: "hello".into(),
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
    fn inference_request_with_system_prompt_roundtrip() {
        let req = InferenceRequest {
            request_id: "req-2".into(),
            node_id: None,
            model_name: "qwen2.5:7b".into(),
            system_prompt: Some("You are a controller.".into()),
            prompt: "turn light on".into(),
            max_tokens: 128,
            temperature: Some(0.0),
            wire_version: WIRE_VERSION,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("system_prompt"));
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
            duration_ms: 42,
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
    fn light_device_list_roundtrip() {
        let msg = MeshMessage::LightDeviceList(LightDeviceListReport {
            node_id: "pi1".into(),
            devices: vec!["test_bulb".into(), "desk_lamp".into()],
            groups: vec!["all".into()],
        });
        let json = serde_json::to_string(&msg).unwrap();
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
        });
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(serde_json::from_str::<MeshMessage>(&json).unwrap(), msg);
    }

    #[test]
    fn intent_response_with_tool_call_roundtrip() {
        let msg = MeshMessage::IntentResponse(IntentResponse {
            request_id: "ir-1".into(),
            node_id: "pi1".into(),
            text: Some("Done — living room set to cozy.".into()),
            tool_calls: vec![ToolCallRecord {
                tool: "scene_load".into(),
                args: serde_json::json!({"scene": "cozy", "transition_ms": 2000}),
                result: Some("ok".into()),
            }],
            error: None,
        });
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(serde_json::from_str::<MeshMessage>(&json).unwrap(), msg);
    }

    #[test]
    fn intent_response_free_text_roundtrip() {
        let msg = MeshMessage::IntentResponse(IntentResponse {
            request_id: "ir-2".into(),
            node_id: "beelink1".into(),
            text: Some("TCP keepalive is a mechanism that...".into()),
            tool_calls: vec![],
            error: None,
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
