use crate::capabilities::detect_capabilities;
use crate::config::AgentConfig;
use crate::hardware::detect_hardware;
use crate::identity::detect_identity;
use shared::{MeshMessage, NodeIdentity, NodeRole};
use tokio::sync::mpsc::Sender;
use tokio::time::{Duration, sleep};

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

    /// Send one startup burst (heartbeat + optional hardware/capabilities), no loop.
    pub async fn start_once(&self) -> Result<(), AgentError> {
        self.tx
            .send(MeshMessage::Heartbeat(self.identity.clone()))
            .await
            .unwrap();

        if self.config.role == NodeRole::Compute {
            let hardware = detect_hardware()?;
            let capabilities = detect_capabilities()?;
            self.tx
                .send(MeshMessage::HardwareReport(hardware))
                .await
                .unwrap();
            self.tx
                .send(MeshMessage::Capabilities(capabilities))
                .await
                .unwrap();
        }

        Ok(())
    }

    pub async fn run(&self) -> Result<(), AgentError> {
        self.start_once().await?;

        loop {
            sleep(Duration::from_secs(self.config.heartbeat_interval_secs)).await;
            self.tx
                .send(MeshMessage::Heartbeat(self.identity.clone()))
                .await
                .unwrap();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentConfig;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn controller_mode_sends_only_heartbeat() {
        let config = AgentConfig {
            role: NodeRole::Controller,
            heartbeat_interval_secs: 5,
        };
        let (tx, mut rx) = mpsc::channel(16);
        let agent = Agent::new_with_config(config, tx);
        agent.start_once().await.unwrap();

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
        agent.start_once().await.unwrap();

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
