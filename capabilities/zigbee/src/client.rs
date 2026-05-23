use std::sync::Arc;
use std::time::Duration;

use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS};
use tokio::sync::broadcast;
use tracing::{info, warn};

use shared::{LightAction, LightStateReport, LightTarget};

use crate::command::{action_payload, target_topic};
use crate::discovery::DeviceRegistry;
use crate::error::ZigbeeError;

#[derive(Debug, Clone)]
pub enum ZigbeeEvent {
    StateChanged(LightStateReport),
    /// Fires once after `bridge/devices` is parsed — full list of device friendly names.
    DeviceListUpdated(Vec<String>),
    /// Fires once after `bridge/groups` is parsed — full list of group friendly names.
    GroupListUpdated(Vec<String>),
    ConnectionLost,
    ConnectionRestored,
}

pub struct ZigbeeClient {
    mqtt: AsyncClient,
    registry: Arc<DeviceRegistry>,
    events: broadcast::Sender<ZigbeeEvent>,
}

impl ZigbeeClient {
    /// Connect to a Mosquitto broker, subscribe to the three Z2M topics, and
    /// spawn the rumqttc event loop poll task. The poll task must be spawned
    /// before any publish calls — without it, publishes hang silently.
    pub async fn connect(host: &str, port: u16, node_id: String) -> Result<Self, ZigbeeError> {
        let mut opts = MqttOptions::new(format!("ai-mesh-{node_id}"), host, port);
        opts.set_keep_alive(Duration::from_secs(30));

        let (mqtt_client, mut eventloop) = AsyncClient::new(opts, 64);
        let (tx, _) = broadcast::channel::<ZigbeeEvent>(256);
        let registry = Arc::new(DeviceRegistry::new());

        let tx_loop = tx.clone();
        let registry_loop = Arc::clone(&registry);
        let subscribe_client = mqtt_client.clone();

        tokio::spawn(async move {
            loop {
                match eventloop.poll().await {
                    Ok(Event::Incoming(Packet::ConnAck(_))) => {
                        info!("zigbee: MQTT connected to broker");
                        // Re-subscribe on every connect (not just the first) so that
                        // subscriptions are restored after a Mosquitto restart or network blip.
                        for topic in &[
                            "zigbee2mqtt/+/state",
                            "zigbee2mqtt/+/availability",
                            "zigbee2mqtt/bridge/devices",
                            "zigbee2mqtt/bridge/groups",
                        ] {
                            if let Err(e) =
                                subscribe_client.subscribe(*topic, QoS::AtMostOnce).await
                            {
                                warn!("zigbee: re-subscribe failed for {topic}: {e}");
                            }
                        }
                        let _ = tx_loop.send(ZigbeeEvent::ConnectionRestored);
                    }
                    Ok(Event::Incoming(Packet::Publish(p))) => {
                        let topic = p.topic.as_str();
                        if topic == "zigbee2mqtt/bridge/devices" {
                            let devices = registry_loop.update_from_payload(p.payload.as_ref());
                            let names: Vec<String> =
                                devices.iter().map(|d| d.friendly_name.clone()).collect();
                            for dev in &devices {
                                info!(
                                    friendly_name = %dev.friendly_name,
                                    ieee = %dev.ieee_address,
                                    "zigbee: device discovered"
                                );
                            }
                            let _ = tx_loop.send(ZigbeeEvent::DeviceListUpdated(names));
                        } else if topic == "zigbee2mqtt/bridge/groups" {
                            let groups = parse_group_names(p.payload.as_ref());
                            info!(count = groups.len(), "zigbee: groups updated");
                            let _ = tx_loop.send(ZigbeeEvent::GroupListUpdated(groups));
                        } else if topic.ends_with("/state") {
                            match parse_state_report(topic, p.payload.as_ref(), &node_id) {
                                Some(report) => {
                                    let _ = tx_loop.send(ZigbeeEvent::StateChanged(report));
                                }
                                None => warn!("zigbee: unparseable state on topic '{topic}'"),
                            }
                        }
                        // availability messages are received but not yet forwarded
                    }
                    Err(e) => {
                        warn!("zigbee: event loop error: {e}");
                        let _ = tx_loop.send(ZigbeeEvent::ConnectionLost);
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                    _ => {}
                }
            }
        });

        Ok(Self {
            mqtt: mqtt_client,
            registry,
            events: tx,
        })
    }

    pub async fn send_command(
        &self,
        target: &LightTarget,
        action: &LightAction,
    ) -> Result<(), ZigbeeError> {
        let topic = target_topic(target);
        let payload = action_payload(action).to_string();
        self.mqtt
            .publish(topic, QoS::AtMostOnce, false, payload)
            .await
            .map_err(|e| ZigbeeError::Client(e.to_string()))
    }

    pub fn device_registry(&self) -> Arc<DeviceRegistry> {
        Arc::clone(&self.registry)
    }

    /// Returns a new broadcast receiver. Each call to `subscribe()` gets its
    /// own independent receiver — safe to call on every coordinator reconnect.
    pub fn subscribe(&self) -> broadcast::Receiver<ZigbeeEvent> {
        self.events.subscribe()
    }
}

fn parse_group_names(payload: &[u8]) -> Vec<String> {
    let entries: Vec<serde_json::Value> = serde_json::from_slice(payload).unwrap_or_default();
    entries
        .iter()
        .filter_map(|e| e.get("friendly_name")?.as_str().map(String::from))
        .collect()
}

fn parse_state_report(topic: &str, payload: &[u8], node_id: &str) -> Option<LightStateReport> {
    // topic format: "zigbee2mqtt/<device_name>/state"
    let mut parts = topic.splitn(3, '/');
    let _ = parts.next(); // "zigbee2mqtt"
    let device_name = parts.next()?.to_owned();

    let json: serde_json::Value = serde_json::from_slice(payload)
        .map_err(|e| warn!("zigbee: JSON parse error for '{device_name}': {e}"))
        .ok()?;

    let on = json
        .get("state")
        .and_then(|v| v.as_str())
        .map(|s| s.eq_ignore_ascii_case("on"))
        .unwrap_or(false);

    let brightness = json
        .get("brightness")
        .and_then(|v| v.as_u64())
        .map(|v| v.min(255) as u8);

    let color_xy = json.get("color").and_then(|v| {
        let x = v.get("x")?.as_f64()? as f32;
        let y = v.get("y")?.as_f64()? as f32;
        Some((x, y))
    });

    let color_temp = json
        .get("color_temp")
        .and_then(|v| v.as_u64())
        .map(|v| v.min(u16::MAX as u64) as u16);

    Some(LightStateReport {
        node_id: node_id.to_owned(),
        device_id: device_name,
        on,
        brightness,
        color_xy,
        color_temp,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_on_with_brightness() {
        let r = parse_state_report(
            "zigbee2mqtt/kitchen_bulb/state",
            br#"{"state":"ON","brightness":200}"#,
            "pi1",
        )
        .unwrap();
        assert_eq!(r.node_id, "pi1");
        assert_eq!(r.device_id, "kitchen_bulb");
        assert!(r.on);
        assert_eq!(r.brightness, Some(200));
        assert!(r.color_xy.is_none());
        assert!(r.color_temp.is_none());
    }

    #[test]
    fn parse_off() {
        let r = parse_state_report(
            "zigbee2mqtt/living_room/state",
            br#"{"state":"OFF"}"#,
            "pi1",
        )
        .unwrap();
        assert!(!r.on);
        assert!(r.brightness.is_none());
    }

    #[test]
    fn parse_state_case_insensitive() {
        let r = parse_state_report("zigbee2mqtt/bulb/state", br#"{"state":"on"}"#, "pi1").unwrap();
        assert!(r.on);
    }

    #[test]
    fn parse_with_color_temp() {
        let r = parse_state_report(
            "zigbee2mqtt/desk_lamp/state",
            br#"{"state":"ON","color_temp":370}"#,
            "pi1",
        )
        .unwrap();
        assert_eq!(r.color_temp, Some(370));
    }

    #[test]
    fn parse_with_color_xy() {
        let r = parse_state_report(
            "zigbee2mqtt/bulb/state",
            br#"{"state":"ON","color":{"x":0.3127,"y":0.3290}}"#,
            "pi1",
        )
        .unwrap();
        let (x, y) = r.color_xy.unwrap();
        assert!((x - 0.3127_f32).abs() < 1e-4);
        assert!((y - 0.3290_f32).abs() < 1e-4);
    }

    #[test]
    fn parse_malformed_json_returns_none() {
        assert!(parse_state_report("zigbee2mqtt/bulb/state", b"not json", "pi1").is_none());
    }

    #[test]
    fn parse_group_names_extracts_friendly_names() {
        let payload = br#"[{"id":1,"friendly_name":"all","members":[]},{"id":2,"friendly_name":"living_room","members":[]}]"#;
        let names = parse_group_names(payload);
        assert_eq!(names, vec!["all", "living_room"]);
    }

    #[test]
    fn parse_group_names_empty_array() {
        assert!(parse_group_names(b"[]").is_empty());
    }

    #[test]
    fn parse_group_names_malformed_returns_empty() {
        assert!(parse_group_names(b"not json").is_empty());
    }

    #[test]
    fn parse_missing_state_field_defaults_to_off() {
        let r =
            parse_state_report("zigbee2mqtt/bulb/state", br#"{"brightness":100}"#, "pi1").unwrap();
        assert!(!r.on);
        assert_eq!(r.brightness, Some(100));
    }
}
