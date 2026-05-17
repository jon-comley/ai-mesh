use shared::hardware::NodeRole;

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub role: NodeRole,
    pub heartbeat_interval_secs: u64,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            role: NodeRole::Compute,
            heartbeat_interval_secs: 5,
        }
    }
}
