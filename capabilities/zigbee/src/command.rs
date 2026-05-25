use shared::{LightAction, LightTarget};

pub fn target_topic(target: &LightTarget) -> String {
    match target {
        LightTarget::Group(name) => format!("zigbee2mqtt/{name}/set"),
        LightTarget::Device(name) => format!("zigbee2mqtt/{name}/set"),
    }
}

pub fn action_payload(action: &LightAction) -> serde_json::Value {
    match action {
        LightAction::On => serde_json::json!({"state": "ON"}),
        LightAction::Off => serde_json::json!({"state": "OFF"}),
        LightAction::Toggle => serde_json::json!({"state": "TOGGLE"}),
        LightAction::Brightness(b) => serde_json::json!({"brightness": b}),
        LightAction::BrightnessTransition {
            value,
            transition_secs,
        } => serde_json::json!({"brightness": value, "transition": transition_secs}),
        LightAction::ColorTemp(mireds) => serde_json::json!({"color_temp": mireds}),
        LightAction::ColorXY { x, y } => serde_json::json!({"color": {"x": x, "y": y}}),
        LightAction::SolarMode(_) => serde_json::json!({}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_topic() {
        assert_eq!(
            target_topic(&LightTarget::Group("all".into())),
            "zigbee2mqtt/all/set"
        );
        assert_eq!(
            target_topic(&LightTarget::Group("living_room".into())),
            "zigbee2mqtt/living_room/set"
        );
    }

    #[test]
    fn device_topic() {
        assert_eq!(
            target_topic(&LightTarget::Device("kitchen_light".into())),
            "zigbee2mqtt/kitchen_light/set"
        );
    }

    #[test]
    fn on_off_toggle_payloads() {
        assert_eq!(
            action_payload(&LightAction::On),
            serde_json::json!({"state": "ON"})
        );
        assert_eq!(
            action_payload(&LightAction::Off),
            serde_json::json!({"state": "OFF"})
        );
        assert_eq!(
            action_payload(&LightAction::Toggle),
            serde_json::json!({"state": "TOGGLE"})
        );
    }

    #[test]
    fn brightness_payload() {
        assert_eq!(
            action_payload(&LightAction::Brightness(128)),
            serde_json::json!({"brightness": 128})
        );
        assert_eq!(
            action_payload(&LightAction::Brightness(0)),
            serde_json::json!({"brightness": 0})
        );
        assert_eq!(
            action_payload(&LightAction::Brightness(255)),
            serde_json::json!({"brightness": 255})
        );
    }

    #[test]
    fn color_temp_payload() {
        assert_eq!(
            action_payload(&LightAction::ColorTemp(370)),
            serde_json::json!({"color_temp": 370})
        );
    }

    #[test]
    fn color_xy_payload() {
        let v = action_payload(&LightAction::ColorXY {
            x: 0.3127,
            y: 0.3290,
        });
        let color = v["color"].as_object().unwrap();
        assert!((color["x"].as_f64().unwrap() - 0.3127_f64).abs() < 1e-4);
        assert!((color["y"].as_f64().unwrap() - 0.3290_f64).abs() < 1e-4);
    }
}
