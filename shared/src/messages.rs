use crate::{
    HardwareSpec, ModelLifecycleState, NodeCapabilities, NodeIdentity, NodeRole, VersionInfo,
};
use serde::{Deserialize, Serialize};

pub const WIRE_VERSION: u32 = 1;

fn default_wire_version() -> u32 {
    WIRE_VERSION
}

/// Sent by the coordinator to a Compute node to run inference.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InferenceRequest {
    pub request_id: String,
    /// Caller-supplied target node. `None` means "let the scheduler decide".
    #[serde(default)]
    pub node_id: Option<String>,
    pub model_name: String,
    pub prompt: String,
    pub max_tokens: u32,
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelLoadRequest {
    pub request_id: String,
    pub node_id: String,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum MeshMessage {
    Heartbeat(NodeIdentity),
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
    // Admin messages (Phase 6+)
    Admin(AdminMessage),
    // Coordinator → caller: generic error response
    Error(String),
}

/// Structured admin messages for coordinator control.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AdminMessage {
    ResetRegistry,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NodeRole;

    #[test]
    fn test_serialize_heartbeat() {
        let identity = NodeIdentity {
            id: "node-1".into(),
            hostname: "test-host".into(),
            ip: "192.168.1.16".into(),
            role: NodeRole::Compute,
        };

        let msg = MeshMessage::Heartbeat(identity.clone());
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: MeshMessage = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded, MeshMessage::Heartbeat(identity));
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
            hostname: "OmniBook7".into(),
            ip: "172.20.107.210".into(),
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
        };

        let rec = NodeRecordFull {
            id: "node-1".into(),
            hostname: "OmniBook7".into(),
            ip: "172.20.107.210".into(),
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
            hostname: "OmniBook7".into(),
            ip: "172.20.107.210".into(),
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
                assert_eq!(info.hostname, "OmniBook7");
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
            prompt: "hello".into(),
            max_tokens: 128,
            wire_version: WIRE_VERSION,
        };
        let json = serde_json::to_string(&req).unwrap();
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
            node_id: "node-1".into(),
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
}
