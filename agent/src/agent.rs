use crate::capabilities::detect_capabilities;
use crate::config::AgentConfig;
use crate::gpu::read_gpu_sample;
use crate::hardware::detect_hardware;
use crate::identity::detect_identity;
use shared::{HeartbeatPayload, MeshMessage, NodeIdentity, NodeRole};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use sysinfo::{Disks, System};
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
    sys: Arc<Mutex<System>>,
    // Coordinator can push SetHeartbeatInterval to change this at runtime.
    interval_secs: Arc<AtomicU64>,
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
        let interval_secs = Arc::new(AtomicU64::new(config.heartbeat_interval_secs));
        Self {
            config,
            identity,
            tx,
            sys: Arc::new(Mutex::new(System::new())),
            interval_secs,
        }
    }

    pub fn node_id(&self) -> &str {
        &self.identity.id
    }

    /// Returns a handle the reader task can use to update the heartbeat interval.
    pub fn interval_handle(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.interval_secs)
    }

    fn heartbeat_payload(&self) -> HeartbeatPayload {
        let (cpu_usage_pct, ram_used_gb, ram_total_gb) = {
            let mut sys = self.sys.lock().unwrap();
            sys.refresh_cpu_usage();
            sys.refresh_memory();
            let cpu = sys.global_cpu_usage();
            let used = sys.used_memory() as f32 / 1_073_741_824.0;
            let total = sys.total_memory() as f32 / 1_073_741_824.0;
            (cpu, used, total)
        };
        let gpu = read_gpu_sample();

        // Free space on the model storage filesystem.
        let model_dir = dirs::home_dir()
            .unwrap_or_default()
            .join(".ai-mesh")
            .join("models");
        let disk_free_gb = Disks::new_with_refreshed_list()
            .iter()
            .filter(|d| model_dir.starts_with(d.mount_point()))
            .max_by_key(|d| d.mount_point().as_os_str().len())
            .map(|d| d.available_space() as f32 / 1_073_741_824.0);

        HeartbeatPayload {
            identity: self.identity.clone(),
            auth_token: std::env::var("MESH_AUTH_TOKEN")
                .unwrap_or_default()
                .trim()
                .to_string(),
            cpu_usage_pct,
            ram_used_gb,
            ram_total_gb,
            gpu_usage_pct: gpu.as_ref().map(|g| g.usage_pct),
            gpu_vram_used_gb: gpu.as_ref().map(|g| g.vram_used_gb),
            gpu_vram_total_gb: gpu.map(|g| g.vram_total_gb),
            disk_free_gb,
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
            sleep(Duration::from_secs(
                self.interval_secs.load(Ordering::Relaxed),
            ))
            .await;
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
