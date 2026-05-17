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
}

impl Default for NodeCapabilities {
    fn default() -> Self {
        Self {
            cpu_inference: false,
            gpu_inference: false,
            ane_inference: false,
            max_model_size_gb: 0.0,
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
}
