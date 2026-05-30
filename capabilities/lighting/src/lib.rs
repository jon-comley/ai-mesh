use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use capability_core::{Capability, ToolSchema};
use capability_zigbee::{ZigbeeClient, ZigbeeError, ZigbeeEvent};
use shared::{
    LightAction, LightDeviceListReport, LightStateReport, LightTarget, MeshMessage,
    SceneLoadedReport,
};
use tokio::sync::{OnceCell, mpsc::Sender};
use tracing::{info, warn};

pub struct LightingCapability {
    zigbee: Arc<OnceCell<Arc<ZigbeeClient>>>,
    coordinator_tx: Arc<Mutex<Option<Sender<MeshMessage>>>>,
    node_id: String,
}

impl LightingCapability {
    pub fn new(node_id: impl Into<String>) -> Self {
        Self {
            zigbee: Arc::new(OnceCell::new()),
            coordinator_tx: Arc::new(Mutex::new(None)),
            node_id: node_id.into(),
        }
    }
}

#[async_trait]
impl Capability for LightingCapability {
    fn name(&self) -> &'static str {
        "lighting"
    }

    fn handles(&self, msg: &MeshMessage) -> bool {
        matches!(
            msg,
            MeshMessage::LightCommand(_) | MeshMessage::SceneLoad(_)
        )
    }

    async fn start(&self, tx: Sender<MeshMessage>) -> Result<(), String> {
        // Update the active coordinator sender for the background task.
        *self.coordinator_tx.lock().unwrap() = Some(tx);

        let Ok(host) = std::env::var("MQTT_HOST") else {
            info!("lighting: MQTT_HOST not set — running as stub");
            return Ok(());
        };
        let port: u16 = std::env::var("MQTT_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1883);
        let node_id = self.node_id.clone();

        // If already initialized, push the current device list to the new connection.
        if let Some(client) = self.zigbee.get() {
            let devices = client.devices();
            if !devices.is_empty() {
                let report = LightDeviceListReport {
                    node_id: node_id.clone(),
                    devices,
                    groups: vec![], // Groups will be updated by the next MQTT event
                };
                let _ = self
                    .send_to_coordinator(MeshMessage::LightDeviceList(report))
                    .await;
            }
        }

        // Initialize Zigbee client if not already done.
        if self.zigbee.get().is_none() {
            let (client, mut events) = ZigbeeClient::connect(&host, port, node_id.clone())
                .await
                .map_err(|e: ZigbeeError| format!("zigbee connect: {e}"))?;
            let client = Arc::new(client);

            let initial_devices = client.devices();
            if !initial_devices.is_empty() {
                let report = LightDeviceListReport {
                    node_id: node_id.clone(),
                    devices: initial_devices,
                    groups: vec![],
                };
                let _ = self
                    .send_to_coordinator(MeshMessage::LightDeviceList(report))
                    .await;
            }

            let known_devices: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(vec![]));
            let known_groups: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(vec![]));
            let kd = Arc::clone(&known_devices);
            let kg = Arc::clone(&known_groups);
            let ctx = Arc::clone(&self.coordinator_tx);
            // Clone the client Arc so the background task can send commands
            // directly (e.g. warm-white restore on device power-on) without
            // going back through the coordinator round-trip.
            let client_bg = Arc::clone(&client);

            tokio::spawn(async move {
                loop {
                    match events.recv().await {
                        Ok(ZigbeeEvent::StateChanged(report)) => {
                            let _ = Self::send_via_ctx(&ctx, MeshMessage::LightState(report)).await;
                        }
                        Ok(ZigbeeEvent::DeviceAvailability { device_id, online }) => {
                            if online {
                                // Bulb just powered on — restore to warm white (2700 K).
                                // Any active room effect will override this within its next tick.
                                let target = LightTarget::Device(device_id.clone());
                                let _ = client_bg.send_command(&target, &LightAction::On).await;
                                let _ = client_bg
                                    .send_command(
                                        &target,
                                        &LightAction::ColorTempTransition {
                                            value: 370,
                                            transition_secs: 1.0,
                                        },
                                    )
                                    .await;
                                let _ = client_bg
                                    .send_command(
                                        &target,
                                        &LightAction::BrightnessTransition {
                                            value: 200,
                                            transition_secs: 1.0,
                                        },
                                    )
                                    .await;
                            }
                            if !online {
                                // Notify the coordinator so the dashboard card shows the
                                // offline state. We only send for offline — when the device
                                // comes back online the warm-white commands trigger a real
                                // state report via the normal Zigbee → MQTT → state path,
                                // which overwrites this record without any null-field clobbering.
                                let report = LightStateReport {
                                    node_id: node_id.clone(),
                                    device_id,
                                    on: false,
                                    online: false,
                                    brightness: None,
                                    color_xy: None,
                                    color_temp: None,
                                };
                                let _ =
                                    Self::send_via_ctx(&ctx, MeshMessage::LightState(report)).await;
                            }
                        }
                        Ok(ZigbeeEvent::DeviceListUpdated(names)) => {
                            info!(
                                count = names.len(),
                                "lighting: sending updated device list to coordinator"
                            );
                            *kd.lock().unwrap() = names.clone();
                            let report = LightDeviceListReport {
                                node_id: node_id.clone(),
                                devices: names,
                                groups: kg.lock().unwrap().clone(),
                            };
                            let _ = Self::send_via_ctx(&ctx, MeshMessage::LightDeviceList(report))
                                .await;
                        }
                        Ok(ZigbeeEvent::GroupListUpdated(names)) => {
                            *kg.lock().unwrap() = names.clone();
                            let report = LightDeviceListReport {
                                node_id: node_id.clone(),
                                devices: kd.lock().unwrap().clone(),
                                groups: names,
                            };
                            let _ = Self::send_via_ctx(&ctx, MeshMessage::LightDeviceList(report))
                                .await;
                        }
                        Ok(ZigbeeEvent::ConnectionLost) => {
                            warn!("zigbee: connection lost");
                        }
                        Ok(ZigbeeEvent::ConnectionRestored) => {
                            info!("zigbee: connection restored");
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            warn!("zigbee: event receiver lagged by {n} messages");
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            });

            self.zigbee
                .set(client)
                .map_err(|_| "race during zigbee init")?;
        }

        info!("lighting: MQTT connected to {host}:{port}");
        Ok(())
    }

    async fn handle(&self, msg: MeshMessage, tx: Sender<MeshMessage>) {
        match msg {
            MeshMessage::LightCommand(req) => match self.zigbee.get() {
                Some(client) => {
                    if let Err(e) = client.send_command(&req.target, &req.command).await {
                        warn!(request_id = %req.request_id, "light command failed: {e}");
                    } else {
                        info!(request_id = %req.request_id, "light command sent");
                    }
                }
                None => {
                    warn!(request_id = %req.request_id, "LightCommand received but MQTT not connected");
                }
            },
            MeshMessage::SceneLoad(req) => {
                info!(request_id = %req.request_id, scene = %req.scene_name, "SceneLoad received (scenes not yet implemented)");
                let report = SceneLoadedReport {
                    request_id: req.request_id,
                    scene_name: req.scene_name,
                    success: false,
                    error: Some("scenes not yet implemented".into()),
                };
                let _ = tx.send(MeshMessage::SceneLoaded(report)).await;
            }
            _ => {}
        }
    }

    fn tools(&self) -> Vec<ToolSchema> {
        vec![
            ToolSchema {
                name: "light_command".into(),
                description: "Turn lights on/off, set brightness, colour temperature, or colour"
                    .into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "target": {
                            "type": "string",
                            "description": "Room or device name, e.g. 'living_room'"
                        },
                        "action": {
                            "type": "string",
                            "enum": ["on", "off", "toggle", "brightness", "color_temp", "color"]
                        },
                        "value": {
                            "type": "number",
                            "description": "Brightness 0–255 or colour temp in Kelvin (for brightness/color_temp actions)"
                        },
                        "color": {
                            "type": "string",
                            "description": "CSS colour for the color action: hex (#FF0000) or named (red, green, blue, yellow, orange, purple, pink, cyan, white…)"
                        }
                    },
                    "required": ["target", "action"]
                }),
            },
            ToolSchema {
                name: "scene_load".into(),
                description: "Load a named lighting scene (e.g. 'cozy', 'bright', 'movie')".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "scene": { "type": "string" },
                        "room": {
                            "type": "string",
                            "description": "Optional — omit to apply everywhere"
                        },
                        "transition_ms": { "type": "integer" }
                    },
                    "required": ["scene"]
                }),
            },
        ]
    }
}

impl LightingCapability {
    async fn send_to_coordinator(&self, msg: MeshMessage) {
        Self::send_via_ctx(&self.coordinator_tx, msg).await;
    }

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
    use shared::{LightAction, LightCommandRequest, LightTarget, SceneLoadRequest};
    use tokio::sync::mpsc;

    fn light_command() -> MeshMessage {
        MeshMessage::LightCommand(LightCommandRequest {
            request_id: "r1".into(),
            target: LightTarget::Group("all".into()),
            command: LightAction::On,
        })
    }

    fn scene_load() -> MeshMessage {
        MeshMessage::SceneLoad(SceneLoadRequest {
            request_id: "r2".into(),
            scene_name: "cozy".into(),
            transition_ms: 2000,
        })
    }

    #[test]
    fn name_is_lighting() {
        assert_eq!(LightingCapability::new("test-node").name(), "lighting");
    }

    #[test]
    fn handles_light_command() {
        assert!(LightingCapability::new("test-node").handles(&light_command()));
    }

    #[test]
    fn handles_scene_load() {
        assert!(LightingCapability::new("test-node").handles(&scene_load()));
    }

    #[test]
    fn does_not_handle_heartbeat() {
        use shared::{HeartbeatPayload, NodeIdentity, NodeRole};
        let msg = MeshMessage::Heartbeat(HeartbeatPayload {
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
        });
        assert!(!LightingCapability::new("test-node").handles(&msg));
    }

    #[tokio::test]
    async fn start_returns_ok() {
        // MQTT_HOST not set → stub mode
        // SAFETY: single-threaded test, no concurrent env access
        unsafe { std::env::remove_var("MQTT_HOST") };
        let (tx, _rx) = mpsc::channel(8);
        assert!(LightingCapability::new("test-node").start(tx).await.is_ok());
    }

    #[test]
    fn tools_returns_two_schemas() {
        let tools = LightingCapability::new("test-node").tools();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "light_command");
        assert_eq!(tools[1].name, "scene_load");
    }

    #[test]
    fn tool_parameters_are_valid_json_schema() {
        let tools = LightingCapability::new("test-node").tools();
        for tool in &tools {
            assert!(tool.parameters.get("type").is_some());
            assert!(tool.parameters.get("properties").is_some());
            assert!(tool.parameters.get("required").is_some());
        }
    }

    #[tokio::test]
    async fn scene_load_handle_sends_scene_loaded() {
        let (tx, mut rx) = mpsc::channel(8);
        LightingCapability::new("test-node")
            .handle(scene_load(), tx)
            .await;
        let reply = rx.recv().await.expect("should receive SceneLoaded");
        match reply {
            MeshMessage::SceneLoaded(r) => {
                assert_eq!(r.request_id, "r2");
                assert_eq!(r.scene_name, "cozy");
                assert!(!r.success);
            }
            _ => panic!("expected SceneLoaded"),
        }
    }

    #[tokio::test]
    async fn light_command_without_mqtt_logs_warning() {
        // ZigbeeClient not initialized → handle should not panic
        // SAFETY: single-threaded test, no concurrent env access
        unsafe { std::env::remove_var("MQTT_HOST") };
        let (tx, _rx) = mpsc::channel(8);
        LightingCapability::new("test-node")
            .handle(light_command(), tx)
            .await;
        // no panic = pass
    }
}
