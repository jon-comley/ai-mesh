//! Breathing — single colour, brightness oscillating on a sine curve.
//!
//! The simplest possible effect — no spatial input, no palette interpolation,
//! no per-bulb state. All bulbs in the room breathe in phase. Useful as a
//! benchmark for the "minimal effect implementation" cost.
//!
//! Params:
//! - `period_secs` — full cycle length in seconds (1–60, default 8).
//! - `min_brightness` — bottom of the sine swing (1–254, default 30).
//! - `max_brightness` — top of the sine swing (1–254, default 200).
//! - `colour_xy` — optional `[x, y]` chromaticity. Defaults to 2000 K warm
//!   amber (looked up via the blend module's CT→xy table).

use std::f32::consts::PI;

use shared::messages::LightAction;

use super::blend::mireds_to_xy;
use super::{Effect, EffectCadence, EffectCategory, EffectCommand, EffectCtx};

pub struct BreathingEffect;

impl BreathingEffect {
    pub fn new() -> Self {
        Self
    }
}

impl Default for BreathingEffect {
    fn default() -> Self {
        Self::new()
    }
}

impl Effect for BreathingEffect {
    fn id(&self) -> &'static str {
        "breathing"
    }

    fn display_name(&self) -> &'static str {
        "Breathing"
    }

    fn description(&self) -> &'static str {
        "All bulbs gently breathe between min and max brightness on a sine curve."
    }

    fn category(&self) -> EffectCategory {
        EffectCategory::Ambient
    }

    fn cadence(&self) -> EffectCadence {
        EffectCadence::FivePerSecond
    }

    fn params_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "period_secs": {
                    "type": "integer",
                    "default": 8,
                    "minimum": 1,
                    "maximum": 60
                },
                "min_brightness": {
                    "type": "integer",
                    "default": 30,
                    "minimum": 1,
                    "maximum": 254
                },
                "max_brightness": {
                    "type": "integer",
                    "default": 200,
                    "minimum": 1,
                    "maximum": 254
                },
                "colour_xy": {
                    "type": "array",
                    "items": { "type": "number" },
                    "minItems": 2,
                    "maxItems": 2
                }
            }
        })
    }

    fn default_params(&self) -> serde_json::Value {
        serde_json::json!({
            "period_secs": 8,
            "min_brightness": 30,
            "max_brightness": 200
        })
    }

    fn tick(&mut self, ctx: &EffectCtx) -> Vec<EffectCommand> {
        let period_secs = ctx
            .params
            .get("period_secs")
            .and_then(|v| v.as_i64())
            .unwrap_or(8)
            .max(1) as f32;
        let min_bri = ctx
            .params
            .get("min_brightness")
            .and_then(|v| v.as_i64())
            .unwrap_or(30)
            .clamp(1, 254) as f32;
        let max_bri = ctx
            .params
            .get("max_brightness")
            .and_then(|v| v.as_i64())
            .unwrap_or(200)
            .clamp(1, 254) as f32;

        // If min > max (caller wrote them in the wrong order), swap rather
        // than emit something nonsensical.
        let (lo, hi) = if min_bri <= max_bri {
            (min_bri, max_bri)
        } else {
            (max_bri, min_bri)
        };

        let color = ctx
            .params
            .get("colour_xy")
            .and_then(|v| {
                let arr = v.as_array()?;
                if arr.len() != 2 {
                    return None;
                }
                Some(super::blend::ColorXy::new(
                    arr[0].as_f64()? as f32,
                    arr[1].as_f64()? as f32,
                ))
            })
            .unwrap_or_else(|| mireds_to_xy(500)); // ~2000 K warm amber

        let elapsed_ms = ctx.now_ms.saturating_sub(ctx.started_at_ms);
        let phase = (elapsed_ms as f32 / 1000.0) / period_secs * 2.0 * PI;
        // Map sin from [-1, 1] to [0, 1] then to [lo, hi].
        let sine_01 = (phase.sin() * 0.5) + 0.5;
        let brightness = (lo + (hi - lo) * sine_01).round().clamp(1.0, 254.0) as u8;

        let mut out = Vec::with_capacity(ctx.bulbs.len() * 2);
        for bulb in ctx.bulbs {
            out.push(EffectCommand {
                device_id: bulb.device_id.clone(),
                action: LightAction::Brightness(brightness),
            });
            out.push(EffectCommand {
                device_id: bulb.device_id.clone(),
                action: LightAction::ColorXY {
                    x: color.x,
                    y: color.y,
                },
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::{
        BulbCurrentState, BulbInRoom, EffectCtx, FixtureType, OpeningContext, RoomContext,
        SolarSample, SpatialHelpers,
    };

    fn bulb(id: &str) -> BulbInRoom {
        BulbInRoom {
            device_id: id.into(),
            x: 0.5,
            y: 0.5,
            z: 0.0,
            fixture_type: FixtureType::CeilingSpot,
            current: BulbCurrentState::default(),
        }
    }

    fn make_ctx<'a>(
        room: &'a RoomContext,
        bulbs: &'a [BulbInRoom],
        openings: &'a [OpeningContext],
        params: &'a serde_json::Value,
        now_ms: u64,
        started_at_ms: u64,
    ) -> EffectCtx<'a> {
        EffectCtx {
            room,
            bulbs,
            openings,
            solar: SolarSample {
                azimuth_degrees: 180.0,
                elevation_degrees: 0.0,
            },
            now_ms,
            started_at_ms,
            params,
            spatial: SpatialHelpers::new(room, openings),
        }
    }

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
        let e = BreathingEffect::new();
        assert_eq!(e.id(), "breathing");
        assert_eq!(e.category(), EffectCategory::Ambient);
        assert_eq!(e.cadence(), EffectCadence::FivePerSecond);
    }

    #[test]
    fn schema_validates_default_params() {
        let e = BreathingEffect::new();
        let schema = jsonschema::JSONSchema::compile(&e.params_schema()).unwrap();
        assert!(schema.is_valid(&e.default_params()));
    }

    #[test]
    fn brightness_swings_between_min_and_max() {
        let bulbs = vec![bulb("b")];
        let room = lounge();
        let openings = Vec::new();
        let params = serde_json::json!({
            "period_secs": 4,
            "min_brightness": 50,
            "max_brightness": 200
        });

        // Sample across one full period at 5 Hz and collect emitted brightnesses.
        let mut e = BreathingEffect::new();
        let mut samples: Vec<u8> = Vec::new();
        for tick in 0..21 {
            let now_ms = tick * 200; // 5 Hz cadence
            let ctx = make_ctx(&room, &bulbs, &openings, &params, now_ms, 0);
            let out = e.tick(&ctx);
            if let LightAction::Brightness(b) = out[0].action {
                samples.push(b);
            }
        }

        let min = *samples.iter().min().unwrap();
        let max = *samples.iter().max().unwrap();
        // Allow rounding slop at the very edges.
        assert!(
            min <= 52,
            "min sample should be near min_brightness=50, got {min}"
        );
        assert!(
            max >= 198,
            "max sample should be near max_brightness=200, got {max}"
        );
    }

    #[test]
    fn colour_xy_param_overrides_default_warm_amber() {
        let bulbs = vec![bulb("b")];
        let room = lounge();
        let openings = Vec::new();
        let params = serde_json::json!({
            "period_secs": 8,
            "min_brightness": 50,
            "max_brightness": 200,
            "colour_xy": [0.3, 0.4]
        });
        let ctx = make_ctx(&room, &bulbs, &openings, &params, 0, 0);
        let mut e = BreathingEffect::new();
        let out = e.tick(&ctx);
        match out[1].action {
            LightAction::ColorXY { x, y } => {
                assert!((x - 0.3).abs() < 1e-4);
                assert!((y - 0.4).abs() < 1e-4);
            }
            _ => panic!("expected ColorXY second"),
        }
    }

    #[test]
    fn default_colour_is_warm_amber() {
        let bulbs = vec![bulb("b")];
        let room = lounge();
        let openings = Vec::new();
        let params = serde_json::json!({});
        let ctx = make_ctx(&room, &bulbs, &openings, &params, 0, 0);
        let mut e = BreathingEffect::new();
        let out = e.tick(&ctx);
        match out[1].action {
            LightAction::ColorXY { x, y } => {
                // 2000 K → warm amber, x > 0.5, y > 0.4.
                assert!(x > 0.5, "expected warm amber x > 0.5, got {x}");
                assert!(y > 0.4, "expected warm amber y > 0.4, got {y}");
            }
            _ => panic!("expected ColorXY second"),
        }
    }

    #[test]
    fn swapped_min_max_handled_gracefully() {
        let bulbs = vec![bulb("b")];
        let room = lounge();
        let openings = Vec::new();
        // min > max — handler should swap rather than emit garbage.
        let params = serde_json::json!({
            "period_secs": 4,
            "min_brightness": 200,
            "max_brightness": 50
        });
        let mut e = BreathingEffect::new();
        let mut samples: Vec<u8> = Vec::new();
        for tick in 0..21 {
            let now_ms = tick * 200;
            let ctx = make_ctx(&room, &bulbs, &openings, &params, now_ms, 0);
            if let LightAction::Brightness(b) = e.tick(&ctx)[0].action {
                samples.push(b);
            }
        }
        assert!(*samples.iter().min().unwrap() >= 50);
        assert!(*samples.iter().max().unwrap() <= 200);
    }
}
