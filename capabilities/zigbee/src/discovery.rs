use shared::DeviceType;
use std::collections::HashMap;
use std::sync::RwLock;
use tracing::warn;

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub ieee_address: String,
    pub friendly_name: String,
    pub device_type: DeviceType,
}

/// Classify a device from its z2m `definition.exposes` list.
///
/// z2m publishes, per device, the features it exposes (a `light` composite,
/// `cover` composite, `climate` composite, or bare sensor properties like
/// `temperature` / `humidity` / `occupancy` / `contact`). Order matters:
/// anything actuating (light/cover/climate) wins over sensor properties,
/// because actuators commonly also report sensor-ish fields. Switch/button
/// devices (wall switches, remotes, the Hue Tap Dial) are checked last,
/// after both actuator and sensor properties have had a chance to match —
/// they carry no state of their own, just an `action` event and often a
/// bare `switch` composite, so anything more specific should win first.
fn classify(exposes: Option<&serde_json::Value>) -> DeviceType {
    let Some(list) = exposes.and_then(|e| e.as_array()) else {
        return DeviceType::Unknown;
    };
    let types: Vec<&str> = list
        .iter()
        .filter_map(|e| e.get("type").and_then(|t| t.as_str()))
        .collect();
    if types.contains(&"light") {
        return DeviceType::Light;
    }
    if types.contains(&"cover") {
        return DeviceType::Cover;
    }
    if types.contains(&"climate") {
        return DeviceType::Climate;
    }
    // Sensors expose bare properties (no composite type): match on names.
    const SENSOR_PROPS: &[&str] = &[
        "temperature",
        "humidity",
        "occupancy",
        "contact",
        "illuminance",
        "pressure",
        "water_leak",
        "smoke",
        "vibration",
    ];
    let has_sensor_prop = list.iter().any(|e| {
        e.get("property")
            .or_else(|| e.get("name"))
            .and_then(|p| p.as_str())
            .is_some_and(|p| SENSOR_PROPS.contains(&p))
    });
    if has_sensor_prop {
        return DeviceType::Sensor;
    }
    // Button/dial/remote devices: a `switch` composite (wired wall switches,
    // z2m's generic "switch" exposes group), or a bare `action` property
    // (battery remotes and dials like the Hue Tap Dial, which enumerate
    // their button/rotation events under `action` with no composite type).
    let has_switch_prop = types.contains(&"switch")
        || list.iter().any(|e| {
            e.get("property")
                .or_else(|| e.get("name"))
                .and_then(|p| p.as_str())
                == Some("action")
        });
    if has_switch_prop {
        DeviceType::Switch
    } else {
        DeviceType::Unknown
    }
}

/// Live registry of Zigbee devices, keyed by friendly_name.
/// Updated from `zigbee2mqtt/bridge/devices` retained messages.
#[derive(Debug, Default)]
pub struct DeviceRegistry {
    by_name: RwLock<HashMap<String, DeviceInfo>>,
}

impl DeviceRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Parse a `zigbee2mqtt/bridge/devices` payload and replace the registry contents.
    /// Returns the list of discovered devices.
    /// Entries without `ieee_address` (the Z2M coordinator itself) are silently skipped.
    pub fn update_from_payload(&self, payload: &[u8]) -> Vec<DeviceInfo> {
        let entries: Vec<serde_json::Value> = match serde_json::from_slice(payload) {
            Ok(v) => v,
            Err(e) => {
                warn!("zigbee: failed to parse bridge/devices: {e}");
                return vec![];
            }
        };

        let mut new_map = HashMap::new();
        let mut discovered = vec![];

        for entry in entries {
            // Skip the Z2M coordinator itself — identified by type=="Coordinator" or absent ieee_address.
            if entry.get("type").and_then(|v| v.as_str()) == Some("Coordinator") {
                continue;
            }
            let ieee = match entry.get("ieee_address").and_then(|v| v.as_str()) {
                Some(s) => s.to_owned(),
                None => continue,
            };
            let name = entry
                .get("friendly_name")
                .and_then(|v| v.as_str())
                .unwrap_or(&ieee)
                .to_owned();
            let device_type = classify(entry.pointer("/definition/exposes"));

            let info = DeviceInfo {
                ieee_address: ieee,
                friendly_name: name.clone(),
                device_type,
            };
            discovered.push(info.clone());
            new_map.insert(name, info);
        }

        *self.by_name.write().unwrap() = new_map;
        discovered
    }

    pub fn get_by_name(&self, name: &str) -> Option<DeviceInfo> {
        self.by_name.read().unwrap().get(name).cloned()
    }

    pub fn all(&self) -> Vec<DeviceInfo> {
        self.by_name.read().unwrap().values().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_devices_and_skips_coordinator() {
        let payload = r#"[
            {"ieee_address": "0xabc123", "friendly_name": "kitchen_bulb"},
            {"friendly_name": "Coordinator", "type": "Coordinator"}
        ]"#;
        let reg = DeviceRegistry::new();
        let found = reg.update_from_payload(payload.as_bytes());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].ieee_address, "0xabc123");
        assert_eq!(found[0].friendly_name, "kitchen_bulb");
    }

    #[test]
    fn skips_coordinator_with_ieee_address() {
        // Newer Z2M versions include ieee_address on the coordinator entry.
        let payload = r#"[
            {"ieee_address": "0x0000000000000000", "friendly_name": "Coordinator", "type": "Coordinator"},
            {"ieee_address": "0xabc123", "friendly_name": "test_bulb", "type": "EndDevice"}
        ]"#;
        let reg = DeviceRegistry::new();
        let found = reg.update_from_payload(payload.as_bytes());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].friendly_name, "test_bulb");
    }

    #[test]
    fn classifies_light_from_exposes() {
        let payload = r#"[{
            "ieee_address": "0x1", "friendly_name": "bulb",
            "definition": {"exposes": [
                {"type": "light", "features": []},
                {"property": "linkquality", "type": "numeric"}
            ]}
        }]"#;
        let reg = DeviceRegistry::new();
        let found = reg.update_from_payload(payload.as_bytes());
        assert_eq!(found[0].device_type, DeviceType::Light);
    }

    #[test]
    fn classifies_sensor_from_bare_properties() {
        let payload = r#"[{
            "ieee_address": "0x2", "friendly_name": "temp_sensor",
            "definition": {"exposes": [
                {"property": "temperature", "type": "numeric"},
                {"property": "humidity", "type": "numeric"},
                {"property": "battery", "type": "numeric"}
            ]}
        }]"#;
        let reg = DeviceRegistry::new();
        let found = reg.update_from_payload(payload.as_bytes());
        assert_eq!(found[0].device_type, DeviceType::Sensor);
    }

    #[test]
    fn classifies_occupancy_sensor() {
        let payload = r#"[{
            "ieee_address": "0x3", "friendly_name": "hall_motion",
            "definition": {"exposes": [
                {"property": "occupancy", "type": "binary"},
                {"property": "battery", "type": "numeric"}
            ]}
        }]"#;
        let reg = DeviceRegistry::new();
        let found = reg.update_from_payload(payload.as_bytes());
        assert_eq!(found[0].device_type, DeviceType::Sensor);
    }

    #[test]
    fn classifies_cover_and_climate() {
        let payload = r#"[
            {"ieee_address": "0x4", "friendly_name": "blind",
             "definition": {"exposes": [{"type": "cover", "features": []}]}},
            {"ieee_address": "0x5", "friendly_name": "trv",
             "definition": {"exposes": [{"type": "climate", "features": []}]}}
        ]"#;
        let reg = DeviceRegistry::new();
        let found = reg.update_from_payload(payload.as_bytes());
        assert_eq!(found[0].device_type, DeviceType::Cover);
        assert_eq!(found[1].device_type, DeviceType::Climate);
    }

    #[test]
    fn actuator_wins_over_sensor_properties() {
        // Lights often also expose sensor-ish numerics; the composite wins.
        let payload = r#"[{
            "ieee_address": "0x6", "friendly_name": "fancy_bulb",
            "definition": {"exposes": [
                {"property": "temperature", "type": "numeric"},
                {"type": "light", "features": []}
            ]}
        }]"#;
        let reg = DeviceRegistry::new();
        let found = reg.update_from_payload(payload.as_bytes());
        assert_eq!(found[0].device_type, DeviceType::Light);
    }

    #[test]
    fn missing_exposes_is_unknown() {
        let payload = r#"[{"ieee_address": "0x7", "friendly_name": "mystery"}]"#;
        let reg = DeviceRegistry::new();
        let found = reg.update_from_payload(payload.as_bytes());
        assert_eq!(found[0].device_type, DeviceType::Unknown);
    }

    #[test]
    fn classifies_action_only_remote_as_switch() {
        // Hue Tap Dial shape: rotation/button events under a bare `action`
        // enum, plus battery — no light/cover/climate/sensor properties.
        let payload = r#"[{
            "ieee_address": "0x8", "friendly_name": "tap_dial",
            "definition": {"exposes": [
                {"property": "action", "type": "enum", "values": ["press_1", "rotate_left"]},
                {"property": "battery", "type": "numeric"}
            ]}
        }]"#;
        let reg = DeviceRegistry::new();
        let found = reg.update_from_payload(payload.as_bytes());
        assert_eq!(found[0].device_type, DeviceType::Switch);
    }

    #[test]
    fn classifies_switch_composite_as_switch() {
        // Wired wall switches expose a `switch` composite type.
        let payload = r#"[{
            "ieee_address": "0x9", "friendly_name": "hallway_switch",
            "definition": {"exposes": [{"type": "switch", "features": []}]}
        }]"#;
        let reg = DeviceRegistry::new();
        let found = reg.update_from_payload(payload.as_bytes());
        assert_eq!(found[0].device_type, DeviceType::Switch);
    }

    #[test]
    fn sensor_properties_win_over_switch_action() {
        // A combo device (e.g. a scene remote that also reports battery-only
        // "sensor-ish" data) should still classify by its real sensor prop
        // when one is present, not fall through to Switch.
        let payload = r#"[{
            "ieee_address": "0xa", "friendly_name": "combo_device",
            "definition": {"exposes": [
                {"property": "action", "type": "enum", "values": ["single"]},
                {"property": "occupancy", "type": "binary"}
            ]}
        }]"#;
        let reg = DeviceRegistry::new();
        let found = reg.update_from_payload(payload.as_bytes());
        assert_eq!(found[0].device_type, DeviceType::Sensor);
    }

    #[test]
    fn get_by_name_returns_device() {
        let reg = DeviceRegistry::new();
        reg.update_from_payload(
            r#"[{"ieee_address": "0xabc", "friendly_name": "living_room"}]"#.as_bytes(),
        );
        let dev = reg.get_by_name("living_room").unwrap();
        assert_eq!(dev.ieee_address, "0xabc");
    }

    #[test]
    fn get_by_name_returns_none_for_unknown() {
        assert!(DeviceRegistry::new().get_by_name("nonexistent").is_none());
    }

    #[test]
    fn malformed_payload_returns_empty_and_does_not_panic() {
        let reg = DeviceRegistry::new();
        assert!(reg.update_from_payload(b"not json").is_empty());
    }

    #[test]
    fn ieee_used_as_friendly_name_fallback() {
        let reg = DeviceRegistry::new();
        let found = reg.update_from_payload(r#"[{"ieee_address": "0x001"}]"#.as_bytes());
        assert_eq!(found[0].friendly_name, "0x001");
    }

    #[test]
    fn update_replaces_previous_entries() {
        let reg = DeviceRegistry::new();
        reg.update_from_payload(
            r#"[{"ieee_address": "0x1", "friendly_name": "bulb_a"}]"#.as_bytes(),
        );
        reg.update_from_payload(
            r#"[{"ieee_address": "0x2", "friendly_name": "bulb_b"}]"#.as_bytes(),
        );
        assert!(reg.get_by_name("bulb_a").is_none());
        assert!(reg.get_by_name("bulb_b").is_some());
    }

    #[test]
    fn all_returns_all_devices() {
        let reg = DeviceRegistry::new();
        reg.update_from_payload(
            r#"[
                {"ieee_address": "0x1", "friendly_name": "a"},
                {"ieee_address": "0x2", "friendly_name": "b"}
            ]"#
            .as_bytes(),
        );
        assert_eq!(reg.all().len(), 2);
    }
}
