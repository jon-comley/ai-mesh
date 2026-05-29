//! Sunset — choreographed warm → indigo journey, west-biased.
//!
//! A 7-keyframe colour palette interpolated perceptually in Oklab via
//! `effects::blend`. The global `t ∈ [0, 1]` advances by `elapsed_ms /
//! duration_secs`; each bulb's effective `t_bulb` is offset by its position
//! along the west→east axis, so bulbs near the west wall reach orange / red /
//! magenta earlier than bulbs in the east. Lamps (table / floor / pendant /
//! LED strip) run a truncated brightness curve so they don't try to do the
//! full ceiling-spot palette — they ramp up to amber, hold, then fade.
//!
//! Params:
//! - `duration_secs` — total length of the curve (60–7200, default 1800).
//! - `peak_warmth` — scales the brightness factor across the palette
//!   (0.0–1.0, default 0.7). Lower = dimmer overall.
//! - `start_at` — `"now"` (start immediately) or `"real-sunset"` (defer
//!   until the solar elevation crosses 0°).

use shared::messages::LightAction;

use super::blend::{ColorXy, oklab_lerp};
use super::{
    Direction, Effect, EffectCadence, EffectCategory, EffectCommand, EffectCtx, FixtureType,
};

pub struct SunsetEffect;

impl SunsetEffect {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SunsetEffect {
    fn default() -> Self {
        Self::new()
    }
}

impl Effect for SunsetEffect {
    fn id(&self) -> &'static str {
        "sunset"
    }

    fn display_name(&self) -> &'static str {
        "Sunset"
    }

    fn description(&self) -> &'static str {
        "Choreographed warm → indigo journey, west-biased."
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
                    "enum": ["now", "real-sunset"]
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

        // `real-sunset`: the curve is parked at t=0 until the sun is at or below
        // the horizon. Once it crosses 0° we start counting from this tick.
        if start_at == "real-sunset" && ctx.solar.elevation_degrees > 0.0 {
            return Vec::new();
        }

        let elapsed_ms = ctx.now_ms.saturating_sub(ctx.started_at_ms);
        let t_global = (elapsed_ms as f32 / duration_ms as f32).clamp(0.0, 1.0);

        let mut out = Vec::with_capacity(ctx.bulbs.len() * 2);
        for bulb in ctx.bulbs {
            let offset = ctx.spatial.directional_offset(bulb, Direction::West);
            let t_bulb = (t_global + offset).clamp(0.0, 1.0);
            let (brightness_factor, color) = sample_palette(t_bulb, bulb.fixture_type, peak_warmth);

            // Brightness is clamped to 1 minimum so the bulb stays addressable
            // even at the indigo tail of the curve. A "fully off" effect would
            // confuse the dedup gate on the next tick.
            let brightness = ((brightness_factor * 255.0).round() as u8).max(1);

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

/// 7-keyframe Sunset palette. Each entry is `(t, brightness_factor, xy)`.
/// brightness_factor is multiplied by `peak_warmth` before final clamp.
const PALETTE: &[(f32, f32, ColorXy)] = &[
    (0.00, 1.00, ColorXy::new(0.313, 0.329)), // cool white (~D65)
    (0.20, 0.95, ColorXy::new(0.410, 0.390)), // warm white (~3500 K)
    (0.40, 0.85, ColorXy::new(0.510, 0.400)), // gold
    (0.60, 0.70, ColorXy::new(0.580, 0.360)), // orange
    (0.75, 0.50, ColorXy::new(0.670, 0.322)), // deep red
    (0.90, 0.30, ColorXy::new(0.420, 0.150)), // magenta
    (1.00, 0.05, ColorXy::new(0.167, 0.040)), // indigo (near off)
];

fn sample_palette(t: f32, fixture: FixtureType, peak_warmth: f32) -> (f32, ColorXy) {
    let t = t.clamp(0.0, 1.0);
    let is_lamp = matches!(
        fixture,
        FixtureType::TableLamp
            | FixtureType::FloorLamp
            | FixtureType::Pendant
            | FixtureType::LedStrip
    );

    // Lamps clamp their *colour* sample to the warm-amber zone of the curve so
    // they don't briefly turn magenta/indigo while fading out. The brightness
    // curve still runs the full t — only the chromaticity is clamped.
    let color_t = if is_lamp { t.min(0.60) } else { t };
    let (_, color) = lerp_palette_at(color_t);

    let brightness = if is_lamp {
        lamp_curve(t)
    } else {
        lerp_palette_at(t).0
    };

    (brightness * peak_warmth, color)
}

/// Find the bracketing PALETTE entries for `t` and interpolate. Returns
/// `(brightness_factor, colour)`.
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

fn lamp_curve(t: f32) -> f32 {
    if t < 0.10 {
        0.0
    } else if t < 0.30 {
        // Ramp 0 → 0.8 across the 0.10–0.30 window.
        ((t - 0.10) / 0.20) * 0.80
    } else if t < 0.70 {
        0.80
    } else if t < 0.95 {
        // Fade 0.8 → 0 across 0.70–0.95.
        (1.0 - (t - 0.70) / 0.25) * 0.80
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::{
        BulbCurrentState, BulbInRoom, EffectCtx, OpeningContext, RoomContext, SolarSample,
        SpatialHelpers,
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

    fn ctx_with(
        bulbs: &[BulbInRoom],
        now_ms: u64,
        started_at_ms: u64,
        params: serde_json::Value,
        elevation_degrees: f64,
    ) -> (RoomContext, Vec<OpeningContext>, SolarSample) {
        let room = RoomContext {
            id: "r1".into(),
            orientation_degrees: 0.0,
            width_m: 4.0,
            depth_m: 4.0,
            height_m: 2.4,
        };
        let solar = SolarSample {
            azimuth_degrees: 270.0,
            elevation_degrees,
        };
        let _ = (bulbs, now_ms, started_at_ms, params);
        (room, Vec::new(), solar)
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

    #[test]
    fn metadata_is_stable() {
        let e = SunsetEffect::new();
        assert_eq!(e.id(), "sunset");
        assert_eq!(e.display_name(), "Sunset");
        assert_eq!(e.category(), EffectCategory::TimeOfDay);
        assert_eq!(e.cadence(), EffectCadence::OnePerSecond);
        let defaults = e.default_params();
        assert_eq!(defaults["duration_secs"], 1800);
    }

    #[test]
    fn schema_validates_default_params() {
        let e = SunsetEffect::new();
        let schema = jsonschema::JSONSchema::compile(&e.params_schema()).unwrap();
        assert!(schema.is_valid(&e.default_params()));
    }

    #[test]
    fn schema_rejects_out_of_range_duration() {
        let e = SunsetEffect::new();
        let schema = jsonschema::JSONSchema::compile(&e.params_schema()).unwrap();
        let bad = serde_json::json!({ "duration_secs": 9_999_999 });
        assert!(!schema.is_valid(&bad));
    }

    #[test]
    fn tick_at_t0_returns_cool_white_brightness_high() {
        let bulbs = vec![bulb("b", 0.5)];
        let (room, openings, solar) = ctx_with(&bulbs, 0, 0, serde_json::json!({}), -10.0);
        let params =
            serde_json::json!({ "duration_secs": 1800, "peak_warmth": 1.0, "start_at": "now" });
        let ctx = make_ctx(&room, &bulbs, &openings, &params, 0, 0, solar);
        let mut e = SunsetEffect::new();
        let out = e.tick(&ctx);
        assert_eq!(out.len(), 2);
        match out[0].action {
            LightAction::Brightness(b) => {
                assert!(b > 200, "expected near-full brightness at t=0, got {b}");
            }
            _ => panic!("expected Brightness first"),
        }
    }

    #[test]
    fn tick_at_t1_returns_low_brightness() {
        // Duration 1000 ms, started 1000 ms ago → t_global = 1.0 for centre bulb,
        // and even with the west offset the value clamps.
        let bulbs = vec![bulb("b", 0.5)];
        let (room, openings, solar) = ctx_with(&bulbs, 1_000, 0, serde_json::json!({}), -10.0);
        let params =
            serde_json::json!({ "duration_secs": 1, "peak_warmth": 1.0, "start_at": "now" });
        let ctx = make_ctx(&room, &bulbs, &openings, &params, 1_000, 0, solar);
        let mut e = SunsetEffect::new();
        let out = e.tick(&ctx);
        match out[0].action {
            LightAction::Brightness(b) => {
                assert!(b < 30, "expected very low brightness at t=1, got {b}");
            }
            _ => panic!("expected Brightness first"),
        }
    }

    #[test]
    fn west_bulb_advances_faster_than_east_bulb() {
        // At global t=0.25 the west bulb (offset +0.5) is at t_bulb=0.75
        // (deep red zone, lower brightness), while the east bulb (offset -0.5)
        // is at t_bulb=0.0 (cool white, full brightness).
        let bulbs = vec![bulb("west", 0.0), bulb("east", 1.0)];
        let (room, openings, solar) = ctx_with(&bulbs, 250, 0, serde_json::json!({}), -10.0);
        let params =
            serde_json::json!({ "duration_secs": 1, "peak_warmth": 1.0, "start_at": "now" });
        // duration_secs = 1, elapsed = 250 ms → t_global = 0.25
        let ctx = make_ctx(&room, &bulbs, &openings, &params, 250, 0, solar);
        let mut e = SunsetEffect::new();
        let out = e.tick(&ctx);

        // First two commands are for the west bulb, next two for the east.
        let west_brightness = match out[0].action {
            LightAction::Brightness(b) => b,
            _ => panic!(),
        };
        let east_brightness = match out[2].action {
            LightAction::Brightness(b) => b,
            _ => panic!(),
        };
        assert!(
            west_brightness < east_brightness,
            "expected west to be dimmer (further along the curve), got west={west_brightness} east={east_brightness}",
        );
    }

    #[test]
    fn lamp_runs_truncated_curve() {
        // At t=0 the lamp curve is 0 (before its 10% ramp window), while a
        // ceiling spot starts at full brightness. So at t=0 lamp brightness
        // should be substantially lower than ceiling spot brightness.
        let mut lamp = bulb("lamp", 0.5);
        lamp.fixture_type = FixtureType::TableLamp;
        let ceiling = bulb("ceiling", 0.5);

        let bulbs = vec![lamp, ceiling];
        let (room, openings, solar) = ctx_with(&bulbs, 0, 0, serde_json::json!({}), -10.0);
        let params =
            serde_json::json!({ "duration_secs": 1800, "peak_warmth": 1.0, "start_at": "now" });
        let ctx = make_ctx(&room, &bulbs, &openings, &params, 0, 0, solar);
        let mut e = SunsetEffect::new();
        let out = e.tick(&ctx);
        let lamp_bri = match out[0].action {
            LightAction::Brightness(b) => b,
            _ => panic!(),
        };
        let ceiling_bri = match out[2].action {
            LightAction::Brightness(b) => b,
            _ => panic!(),
        };
        assert!(
            lamp_bri < ceiling_bri,
            "lamp should be dimmer (or off) than ceiling at t=0: lamp={lamp_bri} ceiling={ceiling_bri}",
        );
    }

    #[test]
    fn start_at_real_sunset_defers_until_elevation_crosses_zero() {
        let bulbs = vec![bulb("b", 0.5)];
        let (room, openings, _) = ctx_with(&bulbs, 0, 0, serde_json::json!({}), 30.0);
        let params = serde_json::json!({ "start_at": "real-sunset" });
        let solar_high = SolarSample {
            azimuth_degrees: 180.0,
            elevation_degrees: 30.0,
        };
        let ctx = make_ctx(&room, &bulbs, &openings, &params, 0, 0, solar_high);
        let mut e = SunsetEffect::new();
        assert!(
            e.tick(&ctx).is_empty(),
            "sunset should defer while sun is above horizon"
        );

        let solar_low = SolarSample {
            azimuth_degrees: 270.0,
            elevation_degrees: -1.0,
        };
        let ctx = make_ctx(&room, &bulbs, &openings, &params, 0, 0, solar_low);
        assert!(
            !e.tick(&ctx).is_empty(),
            "sunset should run once sun is below horizon"
        );
    }

    #[test]
    fn lamp_colour_stays_warm_during_fade() {
        // At t=0.85 the ceiling-spot colour is well into magenta (palette
        // keyframe at 0.90 is magenta). A lamp at the same t should have its
        // colour clamped to the t=0.60 zone (orange/gold), so the two bulbs
        // emit very different chromaticities.
        let mut lamp = bulb("lamp", 0.5);
        lamp.fixture_type = FixtureType::TableLamp;
        let ceiling = bulb("ceiling", 0.5);

        let bulbs = vec![lamp, ceiling];
        let (room, openings, solar) = ctx_with(&bulbs, 850, 0, serde_json::json!({}), -10.0);
        let params =
            serde_json::json!({ "duration_secs": 1, "peak_warmth": 1.0, "start_at": "now" });
        // duration=1s, elapsed=850ms → t=0.85. Centre bulb (offset=0) →
        // t_bulb=0.85 for both.
        let ctx = make_ctx(&room, &bulbs, &openings, &params, 850, 0, solar);
        let mut e = SunsetEffect::new();
        let out = e.tick(&ctx);

        let lamp_xy = match out[1].action {
            LightAction::ColorXY { x, y } => (x, y),
            _ => panic!("expected ColorXY second"),
        };
        let ceiling_xy = match out[3].action {
            LightAction::ColorXY { x, y } => (x, y),
            _ => panic!("expected ColorXY second"),
        };
        // Lamp xy should be in the warm-amber zone (x > 0.5 and y > 0.35);
        // ceiling xy at t=0.85 is between deep-red and magenta (y < 0.3).
        assert!(
            lamp_xy.0 > 0.5,
            "lamp x should stay warm, got {}",
            lamp_xy.0
        );
        assert!(
            lamp_xy.1 > 0.35,
            "lamp y should stay warm, got {}",
            lamp_xy.1
        );
        assert!(
            ceiling_xy.1 < lamp_xy.1,
            "ceiling at t=0.85 should be cooler/magenta-shifted relative to clamped lamp: ceiling={:?} lamp={:?}",
            ceiling_xy,
            lamp_xy,
        );
    }

    #[test]
    fn peak_warmth_scales_brightness() {
        let bulbs = vec![bulb("b", 0.5)];
        let (room, openings, solar) = ctx_with(&bulbs, 0, 0, serde_json::json!({}), -10.0);

        let params_full =
            serde_json::json!({ "duration_secs": 1800, "peak_warmth": 1.0, "start_at": "now" });
        let ctx_full = make_ctx(&room, &bulbs, &openings, &params_full, 0, 0, solar);
        let bri_full = match SunsetEffect::new().tick(&ctx_full)[0].action {
            LightAction::Brightness(b) => b,
            _ => panic!(),
        };

        let params_dim =
            serde_json::json!({ "duration_secs": 1800, "peak_warmth": 0.3, "start_at": "now" });
        let ctx_dim = make_ctx(&room, &bulbs, &openings, &params_dim, 0, 0, solar);
        let bri_dim = match SunsetEffect::new().tick(&ctx_dim)[0].action {
            LightAction::Brightness(b) => b,
            _ => panic!(),
        };

        assert!(
            bri_dim < bri_full,
            "peak_warmth=0.3 should yield lower brightness than 1.0 (got {bri_dim} vs {bri_full})",
        );
    }
}
