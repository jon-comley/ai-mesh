use crate::capabilities::detect_capabilities;
use crate::config::AgentConfig;
use crate::hardware::detect_hardware;
use crate::identity::detect_identity;
use shared::{HeartbeatPayload, MeshMessage, NodeIdentity, NodeRole};
use tokio::sync::mpsc::Sender;
use tokio::time::{Duration, sleep};
use tracing::{info, warn};

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("Failed to detect identity: {0}")]
    Identity(#[from] crate::identity::IdentityError),

    #[error("Failed to detect hardware: {0}")]
    Hardware(#[from] crate::hardware::HardwareError),

    #[error("Failed to detect capabilities: {0}")]
    Capabilities(#[from] crate::capabilities::CapabilityError),
}

pub struct Agent {
    config: AgentConfig,
    // Identity is detected once at construction so the same node_id is used
    // for Heartbeats and for any outbound ModelStatus messages stamped by main.rs.
    identity: NodeIdentity,
    tx: Sender<MeshMessage>,
}

impl Agent {
    pub fn new(heartbeat_interval_secs: u64, tx: Sender<MeshMessage>) -> Self {
        let config = AgentConfig {
            role: NodeRole::Compute,
            heartbeat_interval_secs,
        };
        Self::new_with_config(config, tx)
    }

    pub fn new_with_config(config: AgentConfig, tx: Sender<MeshMessage>) -> Self {
        let identity = detect_identity(config.role.clone()).unwrap_or_else(|_| NodeIdentity {
            id: "unknown".into(),
            hostname: "unknown".into(),
            ip: "unknown".into(),
            role: config.role.clone(),
        });
        Self {
            config,
            identity,
            tx,
        }
    }

    pub fn node_id(&self) -> &str {
        &self.identity.id
    }

    fn heartbeat_payload(&self) -> HeartbeatPayload {
        HeartbeatPayload {
            identity: self.identity.clone(),
            auth_token: std::env::var("MESH_AUTH_TOKEN")
                .unwrap_or_default()
                .trim()
                .to_string(),
            cpu_usage_pct: None,
            ram_used_gb: None,
            ram_total_gb: None,
        }
    }

    /// Send one startup burst (heartbeat + optional hardware/capabilities), no loop.
    /// Returns Ok(false) if the outbound channel is closed (connection dropped).
    pub async fn start_once(&self) -> Result<bool, AgentError> {
        info!(node_id = %self.identity.id, "sending heartbeat");
        if self
            .tx
            .send(MeshMessage::Heartbeat(self.heartbeat_payload()))
            .await
            .is_err()
        {
            return Ok(false); // channel closed — connection already gone
        }

        if self.config.role == NodeRole::Compute {
            info!("detecting hardware and capabilities");
            let hardware = detect_hardware().map_err(|e| {
                warn!(error = %e, "hardware detection failed");
                e
            })?;
            let capabilities = detect_capabilities()?;
            // If the channel closed while we were detecting hardware, exit cleanly.
            if self
                .tx
                .send(MeshMessage::HardwareReport(hardware))
                .await
                .is_err()
            {
                return Ok(false);
            }
            if self
                .tx
                .send(MeshMessage::Capabilities(capabilities))
                .await
                .is_err()
            {
                return Ok(false);
            }
            info!("startup sequence complete");
        }

        Ok(true)
    }

    pub async fn run(&self) -> Result<(), AgentError> {
        if !self.start_once().await? {
            return Ok(()); // connection dropped before we could start
        }

        loop {
            sleep(Duration::from_secs(self.config.heartbeat_interval_secs)).await;
            if self
                .tx
                .send(MeshMessage::Heartbeat(self.heartbeat_payload()))
                .await
                .is_err()
            {
                break; // channel closed — connection dropped
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentConfig;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn start_once_returns_false_on_closed_channel() {
        let config = AgentConfig {
            role: NodeRole::Controller,
            heartbeat_interval_secs: 5,
        };
        let (tx, rx) = mpsc::channel(16);
        drop(rx);
        let agent = Agent::new_with_config(config, tx);
        assert!(!agent.start_once().await.unwrap());
    }

    #[tokio::test]
    async fn run_exits_cleanly_on_closed_channel() {
        let config = AgentConfig {
            role: NodeRole::Controller,
            heartbeat_interval_secs: 5,
        };
        let (tx, rx) = mpsc::channel(16);
        drop(rx);
        let agent = Agent::new_with_config(config, tx);
        assert!(agent.run().await.is_ok());
    }

    #[tokio::test]
    async fn controller_mode_sends_only_heartbeat() {
        let config = AgentConfig {
            role: NodeRole::Controller,
            heartbeat_interval_secs: 5,
        };
        let (tx, mut rx) = mpsc::channel(16);
        let agent = Agent::new_with_config(config, tx);
        assert!(agent.start_once().await.unwrap());

        let mut msgs = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            msgs.push(msg);
        }

        assert_eq!(msgs.len(), 1);
        assert!(matches!(msgs[0], MeshMessage::Heartbeat(_)));
    }

    #[tokio::test]
    async fn compute_mode_sends_full_startup_sequence() {
        let config = AgentConfig {
            role: NodeRole::Compute,
            heartbeat_interval_secs: 5,
        };
        let (tx, mut rx) = mpsc::channel(16);
        let agent = Agent::new_with_config(config, tx);
        assert!(agent.start_once().await.unwrap());

        let mut msgs = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            msgs.push(msg);
        }

        assert!(msgs.len() >= 3);
        assert!(msgs.iter().any(|m| matches!(m, MeshMessage::Heartbeat(_))));
        assert!(
            msgs.iter()
                .any(|m| matches!(m, MeshMessage::HardwareReport(_)))
        );
        assert!(
            msgs.iter()
                .any(|m| matches!(m, MeshMessage::Capabilities(_)))
        );
    }
}
