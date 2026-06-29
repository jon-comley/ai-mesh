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
    /// Fires when `zigbee2mqtt/<device>/availability` changes.
    DeviceAvailability {
        device_id: String,
        online: bool,
    },
    /// Fires when `zigbee2mqtt/bridge/state` changes — indicates whether the
    /// zigbee2mqtt process itself is up and connected to the dongle.
    BridgeState {
        online: bool,
    },
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
    pub async fn connect(
        host: &str,
        port: u16,
        node_id: String,
    ) -> Result<(Self, broadcast::Receiver<ZigbeeEvent>), ZigbeeError> {
        let mut opts = MqttOptions::new(format!("ai-mesh-{node_id}"), host, port);
        opts.set_keep_alive(Duration::from_secs(30));
        opts.set_max_packet_size(usize::MAX, 256 * 1024);

        let (mqtt_client, mut eventloop) = AsyncClient::new(opts, 64);
        let (tx, rx) = broadcast::channel::<ZigbeeEvent>(256);
        let registry = Arc::new(DeviceRegistry::new());

        let tx_loop = tx.clone();
        let registry_loop = Arc::clone(&registry);
        let subscribe_client = mqtt_client.clone();

        tokio::spawn(async move {
            let mut debounce: std::collections::HashMap<String, tokio::task::JoinHandle<()>> =
                std::collections::HashMap::new();
            let mut known_groups: std::collections::HashSet<String> =
                std::collections::HashSet::new();
            // Last-known availability per device, from `<device>/availability`.
            // Used to stamp the `online` flag on state reports: z2m answers a
            // `/get` poll with the device's *cached* state even when the device
            // is unreachable, so a naive state report would re-mark an offline
            // bulb online and clobber the availability=offline. Default true
            // (assume reachable until told otherwise).
            let mut availability: std::collections::HashMap<String, bool> =
                std::collections::HashMap::new();
            loop {
                match eventloop.poll().await {
                    Ok(Event::Incoming(Packet::ConnAck(_))) => {
                        info!("zigbee: MQTT connected to broker");
                        known_groups.clear();
                        // Re-subscribe on every connect (not just the first) so that
                        // subscriptions are restored after a Mosquitto restart or network blip.
                        for topic in &[
                            "zigbee2mqtt/+",
                            "zigbee2mqtt/+/state",
                            "zigbee2mqtt/+/availability",
                            "zigbee2mqtt/bridge/devices",
                            "zigbee2mqtt/bridge/groups",
                            "zigbee2mqtt/bridge/state",
                            "zigbee2mqtt/bridge/event",
                        ] {
                            if let Err(e) =
                                subscribe_client.subscribe(*topic, QoS::AtMostOnce).await
                            {
                                warn!("zigbee: re-subscribe failed for {topic}: {e}");
                            }
                        }
                        // Request the device list from Z2M. Z2M normally publishes
                        // bridge/devices with retain=true, but explicitly requesting it
                        // ensures we get a fresh copy even if the broker has no retained
                        // message (e.g. after a Mosquitto wipe or first boot).
                        if let Err(e) = subscribe_client
                            .publish(
                                "zigbee2mqtt/bridge/request/devices",
                                QoS::AtMostOnce,
                                false,
                                "",
                            )
                            .await
                        {
                            warn!("zigbee: failed to request device list: {e}");
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
                            // Poll current state for every device. Without this, the dashboard
                            // only shows state after the first device-triggered Z2M publish,
                            // which may never come if nothing changes.
                            let poll_client = subscribe_client.clone();
                            let poll_names = names.clone();
                            tokio::spawn(async move {
                                for name in &poll_names {
                                    let topic = format!("zigbee2mqtt/{name}/get");
                                    let payload = r#"{"state":"","brightness":"","color_temp":"","color":{"x":"","y":""}}"#;
                                    if let Err(e) = poll_client
                                        .publish(&topic, QoS::AtMostOnce, false, payload)
                                        .await
                                    {
                                        warn!("zigbee: state poll failed for {name}: {e}");
                                    }
                                }
                            });
                            let _ = tx_loop.send(ZigbeeEvent::DeviceListUpdated(names));
                        } else if topic == "zigbee2mqtt/bridge/groups" {
                            let groups = parse_group_names(p.payload.as_ref());
                            known_groups = groups.iter().cloned().collect();
                            info!(count = groups.len(), "zigbee: groups updated");
                            let _ = tx_loop.send(ZigbeeEvent::GroupListUpdated(groups));
                        } else if topic == "zigbee2mqtt/bridge/state" {
                            // z2m publishes {"state":"online"} on connect and its Last Will
                            // {"state":"offline"} (or plain "offline") when it disconnects.
                            let Some(online) = parse_bridge_online(p.payload.as_ref()) else {
                                continue;
                            };
                            info!(
                                "zigbee: bridge state -> {}",
                                if online { "online" } else { "offline" }
                            );
                            let _ = tx_loop.send(ZigbeeEvent::BridgeState { online });
                        } else if topic.ends_with("/state") || topic.matches('/').count() == 1 {
                            // Both the legacy "<device>/state" subtopic and the standard Z2M
                            // base topic "<device>" carry device state. Only warn on failures
                            // for the explicit /state form; base topics receive many non-state
                            // event types (actions, linkquality, etc.) so we skip silently.
                            match parse_state_report(topic, p.payload.as_ref(), &node_id) {
                                Some(mut report) if !known_groups.contains(&report.device_id) => {
                                    // Stamp the real availability: a `/get` poll reply for an
                                    // unreachable device carries stale cached state, and we must
                                    // not let it flip an offline bulb back to online.
                                    report.online = availability
                                        .get(&report.device_id)
                                        .copied()
                                        .unwrap_or(true);
                                    // Debounce: Z2M fires multiple partial updates per action.
                                    // Cancel any pending emit for this device and restart the timer.
                                    // Sweep completed handles (amortised on each state event).
                                    debounce.retain(|_, h| !h.is_finished());
                                    let device_id = report.device_id.clone();
                                    if let Some(h) = debounce.remove(&device_id) {
                                        h.abort();
                                    }
                                    let tx_d = tx_loop.clone();
                                    let handle = tokio::spawn(async move {
                                        tokio::time::sleep(Duration::from_millis(75)).await;
                                        let _ = tx_d.send(ZigbeeEvent::StateChanged(report));
                                    });
                                    debounce.insert(device_id, handle);
                                }
                                Some(_) => {} // group name — silently skip
                                None => {
                                    if topic.ends_with("/state") {
                                        warn!("zigbee: unparseable state on topic '{topic}'");
                                    }
                                }
                            }
                        } else if topic.ends_with("/availability") {
                            // Parse `zigbee2mqtt/<device>/availability`.
                            // Z2M publishes either `"online"`/`"offline"` (plain string) or
                            // `{"state":"online"}` / `{"state":"offline"}` (JSON object).
                            let device_id = topic
                                .strip_prefix("zigbee2mqtt/")
                                .and_then(|s| s.strip_suffix("/availability"))
                                .unwrap_or("")
                                .to_owned();
                            if !device_id.is_empty() && device_id != "bridge" {
                                let raw = std::str::from_utf8(p.payload.as_ref()).unwrap_or("");
                                let online = if raw.trim() == "online" {
                                    true
                                } else if raw.trim() == "offline" {
                                    false
                                } else if let Ok(v) = serde_json::from_str::<serde_json::Value>(raw)
                                {
                                    v.get("state").and_then(|s| s.as_str()) == Some("online")
                                } else {
                                    continue;
                                };
                                availability.insert(device_id.clone(), online);
                                let _ = tx_loop
                                    .send(ZigbeeEvent::DeviceAvailability { device_id, online });
                            }
                        }
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

        let client = Self {
            mqtt: mqtt_client,
            registry,
            events: tx,
        };
        Ok((client, rx))
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

    pub fn devices(&self) -> Vec<String> {
        self.registry
            .all()
            .into_iter()
            .map(|d| d.friendly_name)
            .collect()
    }
}

fn parse_group_names(payload: &[u8]) -> Vec<String> {
    let entries: Vec<serde_json::Value> = serde_json::from_slice(payload).unwrap_or_default();
    entries
        .iter()
        .filter_map(|e| e.get("friendly_name")?.as_str().map(String::from))
        .collect()
}

/// Parse a `zigbee2mqtt/bridge/state` payload into an online flag.
/// Accepts plain `online`/`offline` or `{"state":"online"|"offline"}`.
/// Returns `None` for unrecognised payloads (caller skips them).
fn parse_bridge_online(payload: &[u8]) -> Option<bool> {
    let raw = std::str::from_utf8(payload).unwrap_or("").trim();
    match raw {
        "online" => Some(true),
        "offline" => Some(false),
        _ => serde_json::from_str::<serde_json::Value>(raw)
            .ok()
            .and_then(|v| {
                v.get("state")
                    .and_then(|s| s.as_str())
                    .map(|s| s == "online")
            }),
    }
}

fn parse_state_report(topic: &str, payload: &[u8], node_id: &str) -> Option<LightStateReport> {
    // topic format: "zigbee2mqtt/<device_name>/state"
    let mut parts = topic.splitn(3, '/');
    let _ = parts.next(); // "zigbee2mqtt"
    let device_name = parts.next()?.to_owned();
    // "bridge" is the Z2M coordinator itself, not a light device.
    if device_name == "bridge" {
        return None;
    }

    let json: serde_json::Value = serde_json::from_slice(payload)
        .map_err(|e| warn!("zigbee: JSON parse error for '{device_name}': {e}"))
        .ok()?;

    // Require at least one light field — filters out button action events like {"action":"toggle"}.
    let has_light_fields = json.get("state").is_some()
        || json.get("brightness").is_some()
        || json.get("color_temp").is_some()
        || json.get("color").is_some();
    if !has_light_fields {
        return None;
    }

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
        online: true,
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
    fn parse_base_topic_on_with_brightness() {
        // Z2M standard format: zigbee2mqtt/<device> (no /state suffix)
        let r = parse_state_report(
            "zigbee2mqtt/kitchen_bulb",
            br#"{"state":"ON","brightness":200}"#,
            "pi1",
        )
        .unwrap();
        assert_eq!(r.device_id, "kitchen_bulb");
        assert!(r.on);
        assert_eq!(r.brightness, Some(200));
    }

    #[test]
    fn parse_base_topic_action_event_returns_none() {
        // Button/remote action events must not create false device entries.
        assert!(
            parse_state_report(
                "zigbee2mqtt/remote_switch",
                br#"{"action":"toggle","linkquality":95}"#,
                "pi1"
            )
            .is_none()
        );
    }

    #[test]
    fn parse_bridge_state_returns_none() {
        // zigbee2mqtt/bridge/state publishes {"state":"online"} — not a light device.
        assert!(
            parse_state_report("zigbee2mqtt/bridge/state", br#"{"state":"online"}"#, "pi1")
                .is_none()
        );
    }

    #[test]
    fn parse_bridge_online_plain_strings() {
        assert_eq!(parse_bridge_online(b"online"), Some(true));
        assert_eq!(parse_bridge_online(b"offline"), Some(false));
        assert_eq!(parse_bridge_online(b"  online  "), Some(true)); // trimmed
    }

    #[test]
    fn parse_bridge_online_json_forms() {
        assert_eq!(parse_bridge_online(br#"{"state":"online"}"#), Some(true));
        assert_eq!(parse_bridge_online(br#"{"state":"offline"}"#), Some(false));
    }

    #[test]
    fn parse_bridge_online_unrecognised_is_none() {
        assert_eq!(parse_bridge_online(b"garbage"), None);
        assert_eq!(parse_bridge_online(br#"{"foo":"bar"}"#), None);
        assert_eq!(parse_bridge_online(b""), None);
    }

    #[test]
    fn parse_missing_state_field_defaults_to_off() {
        let r =
            parse_state_report("zigbee2mqtt/bulb/state", br#"{"brightness":100}"#, "pi1").unwrap();
        assert!(!r.on);
        assert_eq!(r.brightness, Some(100));
    }

    #[test]
    fn parse_group_names_skips_entries_without_friendly_name() {
        let payload = br#"[{"id":1},{"id":2,"friendly_name":"all"}]"#;
        let names = parse_group_names(payload);
        assert_eq!(names, vec!["all"]);
    }

    #[test]
    fn group_name_state_would_be_parsed_but_filtered() {
        // Z2M publishes group state to zigbee2mqtt/<group_name> — same pattern as devices.
        // parse_state_report would accept it; the event loop filters it via known_groups.
        let r = parse_state_report(
            "zigbee2mqtt/all",
            br#"{"state":"ON","brightness":200}"#,
            "pi1",
        );
        assert!(
            r.is_some(),
            "parser accepts group topics — caller must filter"
        );
        assert_eq!(r.unwrap().device_id, "all");
    }
}
