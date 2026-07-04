//! Sensors capability — the read-only sibling of lighting.
//!
//! Consumes the node's shared [`ZigbeeClient`] event stream and forwards
//! sensor-domain events to the coordinator as `MeshMessage::SensorState`.
//! Parsing lives in capability-zigbee (`parse_sensor_report`, beside the
//! light parser); this crate is pure forwarding. Sensors take no commands,
//! so `handles()` is always false. Intent tools for sensors (`get_climate`)
//! are answered from the coordinator's snapshot — no node round-trip — so
//! `tools()` stays empty here (see plans/multi-domain-home.md Phase C).

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use capability_core::Capability;
use capability_zigbee::{ZigbeeClient, ZigbeeEvent, service};
use shared::{DeviceType, MeshMessage, SensorReport};
use tokio::sync::{OnceCell, mpsc::Sender};
use tracing::{info, warn};

pub struct SensorsCapability {
    zigbee: Arc<OnceCell<Arc<ZigbeeClient>>>,
    coordinator_tx: Arc<Mutex<Option<Sender<MeshMessage>>>>,
    node_id: String,
}

impl SensorsCapability {
    pub fn new(node_id: impl Into<String>) -> Self {
        Self {
            zigbee: Arc::new(OnceCell::new()),
            coordinator_tx: Arc::new(Mutex::new(None)),
            node_id: node_id.into(),
        }
    }
}

#[async_trait]
impl Capability for SensorsCapability {
    fn name(&self) -> &'static str {
        "sensors"
    }

    fn handles(&self, _msg: &MeshMessage) -> bool {
        false
    }

    async fn start(&self, tx: Sender<MeshMessage>) -> Result<(), String> {
        // Update the active coordinator sender for the background task.
        *self.coordinator_tx.lock().unwrap() = Some(tx);

        // Shared per-node client (one MQTT connection fanned out to every
        // zigbee-backed capability). Bridge status (ZigbeeStatus, stub-mode
        // offline report) is lighting's to send — a co-resident lighting
        // capability shares this exact client, and duplicating the stream
        // per domain would just double every status message.
        let Some(client) = service::shared_client(&self.node_id)
            .await
            .map_err(|e| format!("zigbee connect: {e}"))?
        else {
            info!("sensors: MQTT_HOST not set — running as stub");
            return Ok(());
        };

        // Spawn the event-forwarding task once. Nothing to replay on
        // reconnect: sensors push periodically, so the coordinator's
        // snapshot repopulates on its own within one report interval.
        if self.zigbee.get().is_none() {
            let mut events = client.subscribe();
            let registry = client.device_registry();
            let ctx = Arc::clone(&self.coordinator_tx);
            let node_id = self.node_id.clone();

            tokio::spawn(async move {
                loop {
                    match events.recv().await {
                        Ok(ZigbeeEvent::SensorChanged(report)) => {
                            let _ =
                                Self::send_via_ctx(&ctx, MeshMessage::SensorState(report)).await;
                        }
                        Ok(ZigbeeEvent::DeviceAvailability { device_id, online }) => {
                            // Forward availability flips for sensor devices as an
                            // all-None report; the coordinator merges it over the
                            // last readings (values stay, online flag updates).
                            // Lights' availability is lighting's domain.
                            let is_sensor = registry
                                .get_by_name(&device_id)
                                .is_some_and(|d| d.device_type == DeviceType::Sensor);
                            if is_sensor {
                                let report = SensorReport {
                                    node_id: node_id.clone(),
                                    device_id,
                                    temperature: None,
                                    humidity: None,
                                    battery: None,
                                    occupancy: None,
                                    contact: None,
                                    illuminance: None,
                                    online,
                                };
                                let _ = Self::send_via_ctx(&ctx, MeshMessage::SensorState(report))
                                    .await;
                            }
                        }
                        // Lighting-domain and bridge-status events — not ours.
                        Ok(_) => {}
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            warn!("sensors: event receiver lagged by {n} messages");
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            });

            self.zigbee
                .set(client)
                .map_err(|_| "race during zigbee init")?;
        }

        info!("sensors: consuming shared zigbee client");
        Ok(())
    }

    async fn handle(&self, _msg: MeshMessage, _tx: Sender<MeshMessage>) {}
}

impl SensorsCapability {
    async fn send_via_ctx(ctx: &Arc<Mutex<Option<Sender<MeshMessage>>>>, msg: MeshMessage) {
        let tx = ctx.lock().unwrap().clone();
        if let Some(tx) = tx {
            let _ = tx.send(msg).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::{
        HeartbeatPayload, LightAction, LightCommandRequest, LightTarget, NodeIdentity, NodeRole,
    };
    use tokio::sync::mpsc;

    #[test]
    fn name_is_sensors() {
        assert_eq!(SensorsCapability::new("test-node").name(), "sensors");
    }

    #[test]
    fn handles_nothing() {
        let cap = SensorsCapability::new("test-node");
        let light = MeshMessage::LightCommand(LightCommandRequest {
            request_id: "r1".into(),
            target: LightTarget::Group("all".into()),
            command: LightAction::On,
        });
        let heartbeat = MeshMessage::Heartbeat(HeartbeatPayload {
            identity: NodeIdentity {
                id: "n1".into(),
                hostname: "h".into(),
                ip: "127.0.0.1".into(),
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
        });
        assert!(!cap.handles(&light));
        assert!(!cap.handles(&heartbeat));
    }

    #[test]
    fn no_tools() {
        assert!(SensorsCapability::new("test-node").tools().is_empty());
    }

    #[tokio::test]
    async fn start_returns_ok_in_stub_mode() {
        // MQTT_HOST not set → stub mode
        // SAFETY: single-threaded test, no concurrent env access
        unsafe { std::env::remove_var("MQTT_HOST") };
        let (tx, _rx) = mpsc::channel(8);
        assert!(SensorsCapability::new("test-node").start(tx).await.is_ok());
    }
}
