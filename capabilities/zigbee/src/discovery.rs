use std::collections::HashMap;
use std::sync::RwLock;
use tracing::warn;

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub ieee_address: String,
    pub friendly_name: String,
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

            let info = DeviceInfo {
                ieee_address: ieee,
                friendly_name: name.clone(),
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
