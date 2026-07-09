use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HardwareSpec {
    pub cpu_model: String,
    pub cpu_cores: u32,
    pub cpu_threads: u32,
    pub ram_gb: f32,
    pub os: String,
    pub arch: String,
    pub gpu: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NodeIdentity {
    pub id: String,
    pub hostname: String,
    pub ip: String,
    pub role: NodeRole,
}

/// Heartbeat payload — node identity plus an auth token for per-message
/// defence-in-depth (supplements the connection-level AuthToken first-frame check).
/// Always serialised; agents without a token send an empty string.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HeartbeatPayload {
    #[serde(flatten)]
    pub identity: NodeIdentity,
    pub auth_token: String,
    /// All-core average CPU utilisation, 0.0–100.0.
    pub cpu_usage_pct: f32,
    /// RAM currently in use on the node, in gibibytes.
    pub ram_used_gb: f32,
    /// Total physical RAM on the node, in gibibytes.
    pub ram_total_gb: f32,
    /// GPU utilisation 0.0–100.0; None on CPU-only nodes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_usage_pct: Option<f32>,
    /// GPU VRAM in use, gibibytes; None on CPU-only nodes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_vram_used_gb: Option<f32>,
    /// Total GPU VRAM, gibibytes; None on CPU-only nodes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_vram_total_gb: Option<f32>,
    /// Free disk space on the model storage filesystem, gibibytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_free_gb: Option<f32>,
}

impl From<NodeIdentity> for HeartbeatPayload {
    fn from(identity: NodeIdentity) -> Self {
        HeartbeatPayload {
            identity,
            auth_token: String::new(),
            cpu_usage_pct: 0.0,
            ram_used_gb: 0.0,
            ram_total_gb: 0.0,
            gpu_usage_pct: None,
            gpu_vram_used_gb: None,
            gpu_vram_total_gb: None,
            disk_free_gb: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum NodeRole {
    Controller,
    Compute,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NodeCapabilities {
    pub cpu_inference: bool,
    pub gpu_inference: bool,
    pub ane_inference: bool,
    pub max_model_size_gb: f32,
    /// Active Cargo feature capabilities on this node. Populated from
    /// compile-time feature flags; used by the coordinator to route
    /// capability-specific messages (e.g. LightCommand) to the right node.
    pub features: Vec<Feature>,
}

/// A node capability, 1:1 with the agent's compile-time Cargo features.
/// Wire strings are the historical lowercase feature names.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Feature {
    Llm,
    Lighting,
    Reaper,
    Sensors,
    Art,
    Voice,
    Audio,
}

impl Default for NodeCapabilities {
    fn default() -> Self {
        Self {
            cpu_inference: false,
            gpu_inference: false,
            ane_inference: false,
            max_model_size_gb: 0.0,
            features: vec![],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VersionInfo {
    pub agent_version: String,
    pub model_version: Option<String>,
    pub update_channel: UpdateChannel,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum UpdateChannel {
    Stable,
    Beta,
    Canary,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UpdateManifest {
    pub version: String,
    pub download_url: String,
    pub checksum: String,
}

/// Runtime serialisable model lifecycle state.
/// Used by the registry and wire protocol. The compile-time typestate
/// (`ModelHandle<S>`) lives in coordinator and is not serialised.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelLifecycleState {
    Unloaded,
    Loading,
    Ready,
    Failed { reason: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_role_roundtrip() {
        let role = NodeRole::Compute;
        let json = serde_json::to_string(&role).unwrap();
        let back: NodeRole = serde_json::from_str(&json).unwrap();
        assert_eq!(role, back);
    }

    #[test]
    fn model_lifecycle_state_roundtrip() {
        let cases = [
            ModelLifecycleState::Unloaded,
            ModelLifecycleState::Loading,
            ModelLifecycleState::Ready,
            ModelLifecycleState::Failed {
                reason: "oom".into(),
            },
        ];
        for state in cases {
            let json = serde_json::to_string(&state).unwrap();
            let back: ModelLifecycleState = serde_json::from_str(&json).unwrap();
            assert_eq!(state, back);
        }
    }

    #[test]
    fn node_capabilities_with_features_roundtrip() {
        let caps = NodeCapabilities {
            cpu_inference: true,
            gpu_inference: true,
            ane_inference: false,
            max_model_size_gb: 8.0,
            features: vec![Feature::Llm, Feature::Lighting],
        };
        let json = serde_json::to_string(&caps).unwrap();
        let back: NodeCapabilities = serde_json::from_str(&json).unwrap();
        assert_eq!(back, caps);
        assert_eq!(back.features, vec![Feature::Llm, Feature::Lighting]);
    }

    #[test]
    fn node_capabilities_empty_features_roundtrip() {
        let caps = NodeCapabilities::default();
        let json = serde_json::to_string(&caps).unwrap();
        let back: NodeCapabilities = serde_json::from_str(&json).unwrap();
        assert!(back.features.is_empty());
    }

    #[test]
    fn test_hardware_spec_roundtrip() {
        let hw = HardwareSpec {
            cpu_model: "Intel i7".into(),
            cpu_cores: 8,
            cpu_threads: 16,
            ram_gb: 32.0,
            os: "Linux".into(),
            arch: "x86_64".into(),
            gpu: Some("NVIDIA RTX 3080".into()),
        };

        let json = serde_json::to_string(&hw).unwrap();
        let decoded: HardwareSpec = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded, hw);
    }

    #[test]
    fn heartbeat_payload_from_identity_has_empty_token() {
        let identity = NodeIdentity {
            id: "n1".into(),
            hostname: "host".into(),
            ip: "10.0.0.1".into(),
            role: NodeRole::Compute,
        };
        let payload = HeartbeatPayload::from(identity.clone());
        assert_eq!(payload.identity, identity);
        assert_eq!(payload.auth_token, "");
    }

    #[test]
    fn heartbeat_payload_roundtrip_with_token() {
        let payload = HeartbeatPayload {
            identity: NodeIdentity {
                id: "n2".into(),
                hostname: "secure".into(),
                ip: "10.0.0.22".into(),
                role: NodeRole::Controller,
            },
            auth_token: "tok123".into(),
            cpu_usage_pct: 0.0,
            ram_used_gb: 0.0,
            ram_total_gb: 0.0,
            gpu_usage_pct: None,
            gpu_vram_used_gb: None,
            gpu_vram_total_gb: None,
            disk_free_gb: None,
        };
        let json = serde_json::to_string(&payload).unwrap();
        let back: HeartbeatPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(back, payload);
        assert!(json.contains("auth_token"));
    }

    #[test]
    fn heartbeat_payload_health_fields_roundtrip() {
        let payload = HeartbeatPayload {
            identity: NodeIdentity {
                id: "n3".into(),
                hostname: "beelink1".into(),
                ip: "192.168.1.14".into(),
                role: NodeRole::Compute,
            },
            auth_token: "tok".into(),
            cpu_usage_pct: 42.5,
            ram_used_gb: 6.1,
            ram_total_gb: 15.9,
            gpu_usage_pct: None,
            gpu_vram_used_gb: None,
            gpu_vram_total_gb: None,
            disk_free_gb: None,
        };
        let json = serde_json::to_string(&payload).unwrap();
        let back: HeartbeatPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(back, payload);
        assert!(json.contains("cpu_usage_pct"));
        assert!(json.contains("ram_used_gb"));
        assert!(json.contains("ram_total_gb"));
    }

    #[test]
    fn heartbeat_payload_gpu_fields_roundtrip_with_data() {
        let payload = HeartbeatPayload {
            identity: NodeIdentity {
                id: "g1".into(),
                hostname: "beelink1".into(),
                ip: "192.168.1.14".into(),
                role: NodeRole::Compute,
            },
            auth_token: String::new(),
            cpu_usage_pct: 30.0,
            ram_used_gb: 8.0,
            ram_total_gb: 16.0,
            gpu_usage_pct: Some(75.0),
            gpu_vram_used_gb: Some(3.5),
            gpu_vram_total_gb: Some(8.0),
            disk_free_gb: None,
        };
        let json = serde_json::to_string(&payload).unwrap();
        let back: HeartbeatPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(back, payload);
        assert_eq!(back.gpu_usage_pct, Some(75.0));
        assert_eq!(back.gpu_vram_used_gb, Some(3.5));
        assert_eq!(back.gpu_vram_total_gb, Some(8.0));
    }

    #[test]
    fn heartbeat_payload_gpu_fields_absent_deserializes_as_none() {
        // A CPU-only node (or pre-C7 agent) omits the GPU fields entirely.
        let json = r#"{
            "id":"pi1","hostname":"pi1","ip":"192.168.1.11",
            "role":"Compute","auth_token":"",
            "cpu_usage_pct":10.0,"ram_used_gb":1.0,"ram_total_gb":8.0
        }"#;
        let payload: HeartbeatPayload = serde_json::from_str(json).unwrap();
        assert_eq!(payload.gpu_usage_pct, None);
        assert_eq!(payload.gpu_vram_used_gb, None);
        assert_eq!(payload.gpu_vram_total_gb, None);
    }

    #[test]
    fn heartbeat_payload_gpu_fields_none_omitted_from_json() {
        let payload = HeartbeatPayload::from(NodeIdentity {
            id: "pi1".into(),
            hostname: "pi1".into(),
            ip: "192.168.1.11".into(),
            role: NodeRole::Compute,
        });
        let json = serde_json::to_string(&payload).unwrap();
        assert!(
            !json.contains("gpu_usage_pct"),
            "None GPU fields should be omitted"
        );
    }

    #[test]
    fn heartbeat_payload_missing_health_fields_fails_to_deserialize() {
        // Pre-Phase-C agents no longer accepted — fields are now required.
        let json = r#"{
            "id": "old", "hostname": "legacy", "ip": "10.0.0.16",
            "role": "Compute", "auth_token": ""
        }"#;
        assert!(serde_json::from_str::<HeartbeatPayload>(json).is_err());
    }

    #[test]
    fn heartbeat_payload_health_fields_always_serialized() {
        let payload = HeartbeatPayload {
            identity: NodeIdentity {
                id: "n4".into(),
                hostname: "pi1".into(),
                ip: "192.168.1.11".into(),
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
        let json = serde_json::to_string(&payload).unwrap();
        assert!(json.contains("cpu_usage_pct"));
        assert!(json.contains("ram_used_gb"));
        assert!(json.contains("ram_total_gb"));
    }
}
