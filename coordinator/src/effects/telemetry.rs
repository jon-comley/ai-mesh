//! Telemetry — F8 #1 placeholder.
//!
//! The plan ("Telemetry Lighting effect") wants this to subscribe to inference
//! start/end and node-health events and emit short bursts on the bulbs nearest
//! the affected node's anchor point in the layout. None of those wires exist
//! yet — there's no node-anchor data in `EffectCtx`, no event subscription
//! mechanism plumbed through the runner, and no "short overlay burst"
//! lifecycle in the trait. F-Effects-2 lists it as sketched, not built.
//!
//! This stub exists so the catalog (`GET /api/effects`) can show Telemetry
//! under the Reactive category right away — the dashboard chip is visible
//! immediately, and the activation pipeline gets exercised. The `tick()` body
//! is empty, so registering it on a room is a no-op until the real
//! implementation lands.

use super::{Effect, EffectCadence, EffectCategory, EffectCommand, EffectCtx};

pub struct TelemetryEffect;

impl TelemetryEffect {
    pub fn new() -> Self {
        Self
    }
}

impl Default for TelemetryEffect {
    fn default() -> Self {
        Self::new()
    }
}

impl Effect for TelemetryEffect {
    fn id(&self) -> &'static str {
        "telemetry"
    }

    fn display_name(&self) -> &'static str {
        "Telemetry"
    }

    fn description(&self) -> &'static str {
        "Mesh health / inference activity ambient overlay (coming with F8 #1)."
    }

    fn category(&self) -> EffectCategory {
        EffectCategory::Reactive
    }

    fn cadence(&self) -> EffectCadence {
        EffectCadence::OnePerSecond
    }

    fn params_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {}
        })
    }

    fn default_params(&self) -> serde_json::Value {
        serde_json::json!({})
    }

    fn tick(&mut self, _ctx: &EffectCtx) -> Vec<EffectCommand> {
        // No-op until F8 #1 wires inference/heartbeat events into the
        // runner. Returning an empty Vec makes activation observable in the
        // dashboard (the badge appears) without changing any bulb state.
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::{
        BulbCurrentState, BulbInRoom, EffectCtx, FixtureType, OpeningContext, RoomContext,
        SolarSample, SpatialHelpers,
    };

    fn lounge() -> RoomContext {
        RoomContext {
            id: "r1".into(),
            orientation_degrees: 0.0,
            width_m: 4.0,
            depth_m: 4.0,
            height_m: 2.4,
        }
    }

    #[test]
    fn metadata_is_stable() {
        let e = TelemetryEffect::new();
        assert_eq!(e.id(), "telemetry");
        assert_eq!(e.category(), EffectCategory::Reactive);
    }

    #[test]
    fn tick_emits_nothing() {
        let room = lounge();
        let bulbs = vec![BulbInRoom {
            device_id: "b".into(),
            x: 0.5,
            y: 0.5,
            z: 0.0,
            fixture_type: FixtureType::CeilingSpot,
            current: BulbCurrentState::default(),
        }];
        let openings: Vec<OpeningContext> = Vec::new();
        let params = serde_json::json!({});
        let ctx = EffectCtx {
            room: &room,
            bulbs: &bulbs,
            openings: &openings,
            solar: SolarSample {
                azimuth_degrees: 0.0,
                elevation_degrees: 0.0,
            },
            now_ms: 0,
            started_at_ms: 0,
            params: &params,
            spatial: SpatialHelpers::new(&room, &openings),
        };
        assert!(TelemetryEffect::new().tick(&ctx).is_empty());
    }
}
