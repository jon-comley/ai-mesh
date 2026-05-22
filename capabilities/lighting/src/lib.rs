use std::sync::Arc;

use async_trait::async_trait;
use capability_core::{Capability, ToolSchema};
use capability_zigbee::{ZigbeeClient, ZigbeeError, ZigbeeEvent};
use shared::{MeshMessage, SceneLoadedReport};
use tokio::sync::{OnceCell, mpsc::Sender};
use tracing::{info, warn};

pub struct LightingCapability {
    zigbee: Arc<OnceCell<Arc<ZigbeeClient>>>,
}

impl LightingCapability {
    pub fn new() -> Self {
        Self {
            zigbee: Arc::new(OnceCell::new()),
        }
    }
}

impl Default for LightingCapability {
    fn default() -> Self {
        Self::new()
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
        let Ok(host) = std::env::var("MQTT_HOST") else {
            info!("lighting: MQTT_HOST not set — running as stub");
            return Ok(());
        };
        let port: u16 = std::env::var("MQTT_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1883);
        let node_id = std::env::var("NODE_ID").unwrap_or_else(|_| "unknown".into());

        let client = ZigbeeClient::connect(&host, port, node_id)
            .await
            .map_err(|e: ZigbeeError| format!("zigbee connect: {e}"))?;
        let client = Arc::new(client);

        let mut events = client.subscribe();
        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(ZigbeeEvent::StateChanged(report)) => {
                        let _ = tx.send(MeshMessage::LightState(report)).await;
                    }
                    Ok(ZigbeeEvent::ConnectionLost) => {
                        warn!("zigbee: connection lost");
                    }
                    Ok(ZigbeeEvent::ConnectionRestored) => {
                        info!("zigbee: connection restored");
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        warn!("zigbee: event receiver lagged by {n} messages");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        self.zigbee
            .set(client)
            .map_err(|_| String::from("zigbee already initialized"))?;
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
                if tx.send(MeshMessage::SceneLoaded(report)).await.is_err() {
                    warn!("lighting: failed to send SceneLoaded — channel closed");
                }
            }
            _ => {}
        }
    }

    fn tools(&self) -> Vec<ToolSchema> {
        vec![
            ToolSchema {
                name: "light_command".into(),
                description: "Turn lights on/off, set brightness or colour temperature".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "target": {
                            "type": "string",
                            "description": "Room or device name, e.g. 'living_room'"
                        },
                        "action": {
                            "type": "string",
                            "enum": ["on", "off", "toggle", "brightness", "color_temp"]
                        },
                        "value": {
                            "type": "number",
                            "description": "Brightness 0–255 or colour temp in Kelvin"
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

#[cfg(test)]
mod tests {
    use super::*;
    use shared::{LightAction, LightCommandRequest, LightTarget, SceneLoadRequest};
    use tokio::sync::mpsc;

    fn light_command() -> MeshMessage {
        MeshMessage::LightCommand(LightCommandRequest {
            request_id: "r1".into(),
            target: LightTarget::Group(1),
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
        assert_eq!(LightingCapability::new().name(), "lighting");
    }

    #[test]
    fn handles_light_command() {
        assert!(LightingCapability::new().handles(&light_command()));
    }

    #[test]
    fn handles_scene_load() {
        assert!(LightingCapability::new().handles(&scene_load()));
    }

    #[test]
    fn does_not_handle_heartbeat() {
        use shared::{NodeIdentity, NodeRole};
        let msg = MeshMessage::Heartbeat(NodeIdentity {
            id: "n1".into(),
            hostname: "h".into(),
            ip: "127.0.0.1".into(),
            role: NodeRole::Compute,
        });
        assert!(!LightingCapability::new().handles(&msg));
    }

    #[tokio::test]
    async fn start_returns_ok() {
        // MQTT_HOST not set → stub mode
        // SAFETY: single-threaded test, no concurrent env access
        unsafe { std::env::remove_var("MQTT_HOST") };
        let (tx, _rx) = mpsc::channel(8);
        assert!(LightingCapability::new().start(tx).await.is_ok());
    }

    #[test]
    fn tools_returns_two_schemas() {
        let tools = LightingCapability::new().tools();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "light_command");
        assert_eq!(tools[1].name, "scene_load");
    }

    #[test]
    fn tool_parameters_are_valid_json_schema() {
        let tools = LightingCapability::new().tools();
        for tool in &tools {
            assert!(tool.parameters.get("type").is_some());
            assert!(tool.parameters.get("properties").is_some());
            assert!(tool.parameters.get("required").is_some());
        }
    }

    #[tokio::test]
    async fn scene_load_handle_sends_scene_loaded() {
        let (tx, mut rx) = mpsc::channel(8);
        LightingCapability::new().handle(scene_load(), tx).await;
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
        LightingCapability::new().handle(light_command(), tx).await;
        // no panic = pass
    }
}
