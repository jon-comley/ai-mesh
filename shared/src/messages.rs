use crate::{HardwareSpec, NodeCapabilities, NodeIdentity, NodeRole, VersionInfo};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NodeRecordLite {
    pub id: String,
    pub hostname: String,
    pub ip: String,
    pub role: NodeRole,
    pub last_heartbeat_ms: u128,
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
}
