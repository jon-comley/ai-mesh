//! Sunrise — inverse-Sunset palette, east-biased.
//!
//! The same 7-keyframe machinery as Sunset, just with the brightness curve
//! reversed (dark → bright) and the chromaticity walking from indigo through
//! red-glow / amber / gold / warm-white back to cool morning white. Bias via
//! `directional_offset(_, East)` so east-wall bulbs (the side the sun rises
//! on) lead the curve.
//!
//! Unlike Sunset, lamps run the *full* palette — this is a wake-up effect, and
//! a bedside lamp ramping up with the curve is exactly what the user wants.
//!
//! Params (identical shape to Sunset):
//! - `duration_secs` — total length of the curve (60–7200, default 1800).
//! - `peak_warmth` — scales the brightness factor (0.0–1.0, default 0.7).
//! - `start_at` — `"now"` or `"real-sunrise"`. `real-sunrise` parks at t=0
//!   while the sun is still below the horizon and starts ticking once
//!   `elevation_degrees > 0`. Mirror of Sunset's `real-sunset` semantic.

use shared::messages::LightAction;

use super::blend::{ColorXy, oklab_lerp};
use super::{Direction, Effect, EffectCadence, EffectCategory, EffectCommand, EffectCtx};

pub struct SunriseEffect;

impl SunriseEffect {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SunriseEffect {
    fn default() -> Self {
        Self::new()
    }
}

impl Effect for SunriseEffect {
    fn id(&self) -> &'static str {
        "sunrise"
    }

    fn display_name(&self) -> &'static str {
        "Sunrise"
    }

    fn description(&self) -> &'static str {
        "Wake-up glow: indigo → red → amber → cool morning white, east-biased."
    }

    fn category(&self) -> EffectCategory {
        EffectCategory::TimeOfDay
    }

    fn cadence(&self) -> EffectCadence {
        EffectCadence::OnePerSecond
    }

    fn params_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "duration_secs": {
                    "type": "integer",
                    "default": 1800,
                    "minimum": 60,
                    "maximum": 7200
                },
                "peak_warmth": {
                    "type": "number",
                    "default": 0.7,
                    "minimum": 0,
                    "maximum": 1
                },
                "start_at": {
                    "type": "string",
                    "default": "now",
                    "enum": ["now", "real-sunrise"]
                }
            }
        })
    }

    fn default_params(&self) -> serde_json::Value {
        serde_json::json!({
            "duration_secs": 1800,
            "peak_warmth": 0.7,
            "start_at": "now"
        })
    }

    fn tick(&mut self, ctx: &EffectCtx) -> Vec<EffectCommand> {
        let duration_secs = ctx
            .params
            .get("duration_secs")
            .and_then(|v| v.as_i64())
            .unwrap_or(1800)
            .max(1) as u64;
        let duration_ms = duration_secs * 1000;
        let peak_warmth = ctx
            .params
            .get("peak_warmth")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.7)
            .clamp(0.0, 1.0) as f32;
        let start_at = ctx
            .params
            .get("start_at")
            .and_then(|v| v.as_str())
            .unwrap_or("now");

        // `real-sunrise` parks the curve until the sun crosses up through
        // the horizon. Inverse of Sunset's deferral.
        if start_at == "real-sunrise" && ctx.solar.elevation_degrees < 0.0 {
            return Vec::new();
        }

        let elapsed_ms = ctx.now_ms.saturating_sub(ctx.started_at_ms);
        let t_global = (elapsed_ms as f32 / duration_ms as f32).clamp(0.0, 1.0);

        let mut out = Vec::with_capacity(ctx.bulbs.len() * 2);
        for bulb in ctx.bulbs {
            let offset = ctx.spatial.directional_offset(bulb, Direction::East);
            let t_bulb = (t_global + offset).clamp(0.0, 1.0);
            let (brightness_factor, color) = lerp_palette_at(t_bulb);

            // Brightness ≥1 so bulbs stay addressable through the dark
            // opening of the curve.
            let brightness = ((brightness_factor * peak_warmth * 255.0).round() as u8).max(1);

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

/// 7-keyframe Sunrise palette. Brightness climbs from ~5% to 100%; colour
/// walks indigo → red glow → deep red → orange → gold → warm white → cool
/// morning white. Essentially Sunset's keyframes rolled in reverse.
const PALETTE: &[(f32, f32, ColorXy)] = &[
    (0.00, 0.05, ColorXy::new(0.167, 0.040)), // indigo / near off
    (0.10, 0.20, ColorXy::new(0.420, 0.150)), // first red glow
    (0.25, 0.40, ColorXy::new(0.670, 0.322)), // deep red
    (0.40, 0.60, ColorXy::new(0.580, 0.360)), // orange
    (0.60, 0.80, ColorXy::new(0.510, 0.400)), // gold
    (0.80, 0.95, ColorXy::new(0.410, 0.390)), // warm white
    (1.00, 1.00, ColorXy::new(0.313, 0.329)), // cool morning white
];

fn lerp_palette_at(t: f32) -> (f32, ColorXy) {
    let t = t.clamp(0.0, 1.0);
    let mut lo = 0;
    for (i, kf) in PALETTE.iter().enumerate() {
        if kf.0 >= t {
            lo = i.saturating_sub(1);
            break;
        }
        lo = i;
    }
    let hi = (lo + 1).min(PALETTE.len() - 1);
    let (t0, b0, c0) = PALETTE[lo];
    let (t1, b1, c1) = PALETTE[hi];
    let local_t = if (t1 - t0).abs() > f32::EPSILON {
        (t - t0) / (t1 - t0)
    } else {
        0.0
    };
    let brightness = b0 + (b1 - b0) * local_t;
    let color = oklab_lerp(c0, c1, local_t);
    (brightness, color)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::{
        BulbCurrentState, BulbInRoom, EffectCtx, FixtureType, OpeningContext, RoomContext,
        SolarSample, SpatialHelpers,
    };

    fn bulb(id: &str, x: f32) -> BulbInRoom {
        BulbInRoom {
            device_id: id.into(),
            x,
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
        solar: SolarSample,
    ) -> EffectCtx<'a> {
        EffectCtx {
            room,
            bulbs,
            openings,
            solar,
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
        let e = SunriseEffect::new();
        assert_eq!(e.id(), "sunrise");
        assert_eq!(e.display_name(), "Sunrise");
        assert_eq!(e.category(), EffectCategory::TimeOfDay);
        assert_eq!(e.cadence(), EffectCadence::OnePerSecond);
        let defaults = e.default_params();
        assert_eq!(defaults["start_at"], "now");
    }

    #[test]
    fn schema_validates_default_params() {
        let e = SunriseEffect::new();
        let schema = jsonschema::JSONSchema::compile(&e.params_schema()).unwrap();
        assert!(schema.is_valid(&e.default_params()));
    }

    #[test]
    fn tick_at_t0_is_dim() {
        let bulbs = vec![bulb("b", 0.5)];
        let room = lounge();
        let openings = Vec::new();
        let params =
            serde_json::json!({ "duration_secs": 1800, "peak_warmth": 1.0, "start_at": "now" });
        let solar = SolarSample {
            azimuth_degrees: 90.0,
            elevation_degrees: 5.0,
        };
        let ctx = make_ctx(&room, &bulbs, &openings, &params, 0, 0, solar);
        let mut e = SunriseEffect::new();
        let out = e.tick(&ctx);
        let bri = match out[0].action {
            LightAction::Brightness(b) => b,
            _ => panic!(),
        };
        assert!(
            bri < 50,
            "expected very low brightness at sunrise t=0, got {bri}"
        );
    }

    #[test]
    fn tick_at_t1_is_bright() {
        let bulbs = vec![bulb("b", 0.5)];
        let room = lounge();
        let openings = Vec::new();
        let params =
            serde_json::json!({ "duration_secs": 1, "peak_warmth": 1.0, "start_at": "now" });
        let solar = SolarSample {
            azimuth_degrees: 90.0,
            elevation_degrees: 30.0,
        };
        let ctx = make_ctx(&room, &bulbs, &openings, &params, 1_000, 0, solar);
        let mut e = SunriseEffect::new();
        let out = e.tick(&ctx);
        let bri = match out[0].action {
            LightAction::Brightness(b) => b,
            _ => panic!(),
        };
        assert!(
            bri > 200,
            "expected high brightness at sunrise t=1, got {bri}"
        );
    }

    #[test]
    fn east_bulb_advances_faster_than_west_bulb() {
        // At global t=0.25 the east bulb (offset +0.5) is at t_bulb=0.75
        // (well into orange/gold, high brightness); the west bulb (offset
        // -0.5) clamps at 0 (still dark).
        let bulbs = vec![bulb("east", 1.0), bulb("west", 0.0)];
        let room = lounge();
        let openings = Vec::new();
        let params =
            serde_json::json!({ "duration_secs": 1, "peak_warmth": 1.0, "start_at": "now" });
        let solar = SolarSample {
            azimuth_degrees: 90.0,
            elevation_degrees: 10.0,
        };
        let ctx = make_ctx(&room, &bulbs, &openings, &params, 250, 0, solar);
        let mut e = SunriseEffect::new();
        let out = e.tick(&ctx);

        let east_bri = match out[0].action {
            LightAction::Brightness(b) => b,
            _ => panic!(),
        };
        let west_bri = match out[2].action {
            LightAction::Brightness(b) => b,
            _ => panic!(),
        };
        assert!(
            east_bri > west_bri,
            "expected east to be brighter (further along the climb): east={east_bri} west={west_bri}",
        );
    }

    #[test]
    fn start_at_real_sunrise_defers_until_elevation_crosses_zero() {
        let bulbs = vec![bulb("b", 0.5)];
        let room = lounge();
        let openings = Vec::new();
        let params = serde_json::json!({ "start_at": "real-sunrise" });

        // Sun below horizon — defer.
        let solar_low = SolarSample {
            azimuth_degrees: 90.0,
            elevation_degrees: -5.0,
        };
        let ctx = make_ctx(&room, &bulbs, &openings, &params, 0, 0, solar_low);
        let mut e = SunriseEffect::new();
        assert!(
            e.tick(&ctx).is_empty(),
            "sunrise should defer while sun is below horizon"
        );

        // Sun crossing up — run.
        let solar_high = SolarSample {
            azimuth_degrees: 90.0,
            elevation_degrees: 1.0,
        };
        let ctx = make_ctx(&room, &bulbs, &openings, &params, 0, 0, solar_high);
        assert!(
            !e.tick(&ctx).is_empty(),
            "sunrise should run once sun is above horizon"
        );
    }
}
