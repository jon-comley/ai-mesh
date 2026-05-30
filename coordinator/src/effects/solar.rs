//! Solar — tracks the sun's elevation to drive brightness and colour temperature.
//!
//! Params
//! ------
//! - `min_brightness` (1–254, default 1)  — floor brightness at all times.
//!   Set to 80–120 for home use so evening/night stays at a useful level.
//!   Leave at 1 for office use where lights should follow the sun all the way down.
//! - `max_brightness` (1–254, default 254) — peak brightness cap at solar noon.
//!   Useful in bedrooms or any space where full power is too harsh.
//! - `ct_warmth` (0.0–1.0, default 1.0) — scales the warm-shift at low sun.
//!   1.0 = current behaviour (500 mireds / 2000 K candle-amber at night).
//!   0.4 = warm white (~3400 K) at night — comfortable for office evening use.
//!   0.0 = colour temperature never shifts (always at the daytime cool end).

use shared::messages::LightAction;

use super::{Effect, EffectCadence, EffectCategory, EffectCommand, EffectCtx, FixtureType};

pub struct SolarEffect;

impl SolarEffect {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SolarEffect {
    fn default() -> Self {
        Self::new()
    }
}

impl Effect for SolarEffect {
    fn id(&self) -> &'static str {
        "solar"
    }

    fn display_name(&self) -> &'static str {
        "Solar"
    }

    fn description(&self) -> &'static str {
        "Tracks the sun: brighter + cooler at noon, dimmer + warmer at sunset. Tune min/max brightness and warmth to suit home or office."
    }

    fn category(&self) -> EffectCategory {
        EffectCategory::TimeOfDay
    }

    fn cadence(&self) -> EffectCadence {
        EffectCadence::OnePerMinute
    }

    fn params_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "min_brightness": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 254,
                    "default": 1,
                    "description": "Floor brightness (1 = follow sun all the way down; 100 = stays usable at night)"
                },
                "max_brightness": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 254,
                    "default": 254,
                    "description": "Peak brightness cap at solar noon"
                },
                "ct_warmth": {
                    "type": "number",
                    "minimum": 0.0,
                    "maximum": 1.0,
                    "default": 1.0,
                    "description": "How warm the colour shifts at low sun (1.0 = amber candlelight, 0.4 = warm white, 0.0 = always cool)"
                }
            }
        })
    }

    fn default_params(&self) -> serde_json::Value {
        serde_json::json!({
            "min_brightness": 1,
            "max_brightness": 254,
            "ct_warmth": 1.0
        })
    }

    fn tick(&mut self, ctx: &EffectCtx) -> Vec<EffectCommand> {
        let min_bri = ctx
            .params
            .get("min_brightness")
            .and_then(|v| v.as_i64())
            .unwrap_or(1)
            .clamp(1, 254) as u8;
        let max_bri = ctx
            .params
            .get("max_brightness")
            .and_then(|v| v.as_i64())
            .unwrap_or(254)
            .clamp(1, 254) as u8;
        let ct_warmth = ctx
            .params
            .get("ct_warmth")
            .and_then(|v| v.as_f64())
            .unwrap_or(1.0)
            .clamp(0.0, 1.0) as f32;

        let mut out = Vec::with_capacity(ctx.bulbs.len() * 2);

        let azimuth = ctx.solar.azimuth_degrees;
        let elevation = ctx.solar.elevation_degrees;
        let room_orientation = ctx.room.orientation_degrees as f64;
        let effective_azimuth = (azimuth - room_orientation + 360.0) % 360.0;

        // Base brightness + CT from elevation alone.
        let (base_bri, base_ct) = calculate_solar_state(elevation);

        // Apply ct_warmth: interpolate CT between the cool end (153 mireds) and
        // the raw elevation-driven value. At 1.0 nothing changes; at 0.0 the
        // colour temperature never shifts from cool white.
        let effective_ct = (153.0 + ct_warmth * (base_ct as f32 - 153.0))
            .round()
            .clamp(153.0, 500.0) as u16;

        // Openings → intensity scale (room contribution from sun-facing windows).
        let intensity_scale =
            openings_intensity_scale(ctx.openings, effective_azimuth, elevation, room_orientation);

        // Ensure max_bri >= min_bri so the clamp is never inverted.
        let max_bri = max_bri.max(min_bri);

        for bulb in ctx.bulbs {
            let fixture_sensitivity = fixture_sensitivity(bulb.fixture_type);
            let exposure = bulb_exposure(bulb, effective_azimuth, elevation);

            let raw_bri = ((base_bri as f32 + (exposure * fixture_sensitivity * 20.0))
                * intensity_scale)
                .clamp(1.0, 254.0) as u8;
            let final_bri = raw_bri.clamp(min_bri, max_bri);

            out.push(EffectCommand {
                device_id: bulb.device_id.clone(),
                action: LightAction::BrightnessTransition {
                    value: final_bri,
                    transition_secs: 30.0,
                },
            });
            out.push(EffectCommand {
                device_id: bulb.device_id.clone(),
                action: LightAction::ColorTempTransition {
                    value: effective_ct,
                    transition_secs: 30.0,
                },
            });
        }
        out
    }
}

fn fixture_sensitivity(f: FixtureType) -> f32 {
    match f {
        FixtureType::TableLamp => 1.0,
        FixtureType::FloorLamp => 0.8,
        FixtureType::LedStrip => 0.9,
        FixtureType::Pendant => 0.7,
        FixtureType::CeilingSpot | FixtureType::Unknown => 0.4,
    }
}

fn bulb_exposure(bulb: &super::BulbInRoom, effective_azimuth: f64, elevation: f64) -> f32 {
    if bulb.z > 0.0 {
        // Full 3D: sun direction vector × normalised bulb position vector.
        let az_rad = effective_azimuth.to_radians();
        let el_rad = elevation.to_radians();
        let sun = (
            (az_rad.sin() * el_rad.cos()) as f32,
            (az_rad.cos() * el_rad.cos()) as f32,
            el_rad.sin() as f32,
        );
        let bx = bulb.x - 0.5;
        let by = bulb.y - 0.5;
        let bz = bulb.z - 0.5;
        let len = (bx * bx + by * by + bz * bz).sqrt().max(1e-6);
        let dot = (bx / len) * sun.0 + (by / len) * sun.1 + (bz / len) * sun.2;
        dot.max(0.0)
    } else {
        // Legacy 2D path — matches spatial.rs behaviour for rooms without z.
        let sun_rad = effective_azimuth.to_radians();
        let raw = (bulb.x * sun_rad.sin() as f32 + bulb.y * sun_rad.cos() as f32) * 0.1;
        raw.clamp(-1.0, 1.0)
    }
}

fn openings_intensity_scale(
    openings: &[super::OpeningContext],
    effective_azimuth: f64,
    elevation: f64,
    room_orientation: f64,
) -> f32 {
    if openings.is_empty() {
        return 0.5;
    }
    let mut contribution = 0.0_f32;
    for o in openings {
        let facing = wall_edge_to_degrees(&o.wall_edge);
        let adj = (facing - room_orientation + 360.0) % 360.0;
        let diff = (effective_azimuth - adj).abs();
        let norm = if diff > 180.0 { 360.0 - diff } else { diff };
        if norm <= 90.0 {
            let elev_factor = (elevation as f32 / 45.0).clamp(0.0, 1.0);
            contribution +=
                (1.0 - norm as f32 / 90.0) * o.transmission * o.width_norm * elev_factor;
        }
    }
    let c = contribution.clamp(0.0, 1.0);
    if c > 0.0 { 1.0 + c * 0.5 } else { 0.2 }
}

fn wall_edge_to_degrees(edge: &str) -> f64 {
    match edge {
        "N" => 0.0,
        "E" => 90.0,
        "S" => 180.0,
        "W" => 270.0,
        _ => 0.0,
    }
}

fn calculate_solar_state(elevation: f64) -> (u8, u16) {
    if elevation <= 0.0 {
        let t = ((elevation + 18.0) / 18.0).clamp(0.0, 1.0);
        let bri = (1.0 + t * 29.0).round() as u8;
        return (bri, 500);
    }
    let t = (elevation / 90.0).clamp(0.0, 1.0);
    let bri = (30.0 + t * 225.0).round() as u8;
    let ct = (454.0 - t * 301.0).round() as u16;
    (bri, ct)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::{
        BulbCurrentState, BulbInRoom, EffectCtx, OpeningContext, RoomContext, SolarSample,
        SpatialHelpers,
    };

    fn ctx_with(
        bulbs: Vec<BulbInRoom>,
        openings: Vec<OpeningContext>,
        elevation: f64,
        azimuth: f64,
    ) -> (
        RoomContext,
        Vec<BulbInRoom>,
        Vec<OpeningContext>,
        SolarSample,
    ) {
        let room = RoomContext {
            id: "r1".into(),
            orientation_degrees: 0.0,
            width_m: 4.0,
            depth_m: 4.0,
            height_m: 2.4,
        };
        let solar = SolarSample {
            azimuth_degrees: azimuth,
            elevation_degrees: elevation,
        };
        (room, bulbs, openings, solar)
    }

    fn bulb(id: &str, x: f32, y: f32, z: f32, fixture: FixtureType) -> BulbInRoom {
        BulbInRoom {
            device_id: id.into(),
            x,
            y,
            z,
            fixture_type: fixture,
            current: BulbCurrentState::default(),
        }
    }

    #[test]
    fn metadata_is_stable() {
        let s = SolarEffect::new();
        assert_eq!(s.id(), "solar");
        assert_eq!(s.display_name(), "Solar");
        assert_eq!(s.category(), EffectCategory::TimeOfDay);
        assert_eq!(s.cadence(), EffectCadence::OnePerMinute);
    }

    #[test]
    fn calculate_solar_state_matches_legacy() {
        // These four values were locked by the previous spatial.rs tests.
        assert_eq!(calculate_solar_state(90.0), (255, 153));
        assert_eq!(calculate_solar_state(0.0), (30, 500));
        let (bri, ct) = calculate_solar_state(45.0);
        assert!(bri > 100 && bri < 200);
        assert!(ct > 153 && ct < 454);
        assert_eq!(calculate_solar_state(-18.0), (1, 500));
    }

    #[test]
    fn wall_edge_to_degrees_known_values() {
        assert!((wall_edge_to_degrees("N") - 0.0).abs() < 1e-6);
        assert!((wall_edge_to_degrees("E") - 90.0).abs() < 1e-6);
        assert!((wall_edge_to_degrees("S") - 180.0).abs() < 1e-6);
        assert!((wall_edge_to_degrees("W") - 270.0).abs() < 1e-6);
        assert!((wall_edge_to_degrees("?") - 0.0).abs() < 1e-6);
    }

    #[test]
    fn openings_intensity_empty_returns_half() {
        let scale = openings_intensity_scale(&[], 180.0, 30.0, 0.0);
        assert!((scale - 0.5).abs() < 1e-6);
    }

    #[test]
    fn openings_intensity_south_window_sun_south_amplifies() {
        let openings = vec![OpeningContext {
            wall_edge: "S".into(),
            width_norm: 0.5,
            transmission: 1.0,
        }];
        // Sun at 180° hitting the south wall at 45° elevation.
        let scale = openings_intensity_scale(&openings, 180.0, 45.0, 0.0);
        // Contribution = (1 - 0/90) * 1.0 * 0.5 * (45/45) = 0.5 → scale = 1.0 + 0.5*0.5 = 1.25
        assert!((scale - 1.25).abs() < 1e-3);
    }

    #[test]
    fn openings_intensity_north_window_sun_south_dampens() {
        let openings = vec![OpeningContext {
            wall_edge: "N".into(),
            width_norm: 0.5,
            transmission: 1.0,
        }];
        // Sun at 180° is on the opposite side of the north wall — no light through.
        let scale = openings_intensity_scale(&openings, 180.0, 45.0, 0.0);
        assert!((scale - 0.2).abs() < 1e-3);
    }

    #[test]
    fn tick_emits_brightness_and_color_temp_per_bulb() {
        let (room, bulbs, openings, solar) = ctx_with(
            vec![bulb("b1", 0.5, 0.5, 0.0, FixtureType::CeilingSpot)],
            vec![],
            45.0,
            90.0,
        );
        let params = serde_json::json!({});
        let ctx = EffectCtx {
            room: &room,
            bulbs: &bulbs,
            openings: &openings,
            solar,
            now_ms: 0,
            started_at_ms: 0,
            params: &params,
            spatial: SpatialHelpers::new(&room, &openings),
        };
        let mut effect = SolarEffect::new();
        let out = effect.tick(&ctx);
        assert_eq!(out.len(), 2);
        assert!(matches!(
            out[0].action,
            LightAction::BrightnessTransition { .. }
        ));
        assert!(matches!(
            out[1].action,
            LightAction::ColorTempTransition { .. }
        ));
        assert_eq!(out[0].device_id, "b1");
    }

    #[test]
    fn schema_validates_default_params() {
        let e = SolarEffect::new();
        let schema = jsonschema::JSONSchema::compile(&e.params_schema()).unwrap();
        assert!(schema.is_valid(&e.default_params()));
    }

    #[test]
    fn min_brightness_lifts_nighttime_floor() {
        let (room, bulbs, openings, solar) = ctx_with(
            vec![bulb("b1", 0.5, 0.5, 0.0, FixtureType::CeilingSpot)],
            vec![],
            -10.0, // well below horizon — raw brightness would be ~12
            180.0,
        );
        let params = serde_json::json!({ "min_brightness": 100 });
        let ctx = EffectCtx {
            room: &room,
            bulbs: &bulbs,
            openings: &openings,
            solar,
            now_ms: 0,
            started_at_ms: 0,
            params: &params,
            spatial: SpatialHelpers::new(&room, &openings),
        };
        let mut effect = SolarEffect::new();
        let out = effect.tick(&ctx);
        if let LightAction::BrightnessTransition { value: b, .. } = out[0].action {
            assert!(
                b >= 100,
                "expected min_brightness=100 to floor at 100, got {b}"
            );
        }
    }

    #[test]
    fn max_brightness_caps_noon() {
        let (room, bulbs, openings, solar) = ctx_with(
            vec![bulb("b1", 0.5, 0.5, 0.0, FixtureType::CeilingSpot)],
            vec![],
            89.0, // near zenith — raw brightness ~254
            180.0,
        );
        let params = serde_json::json!({ "max_brightness": 150 });
        let ctx = EffectCtx {
            room: &room,
            bulbs: &bulbs,
            openings: &openings,
            solar,
            now_ms: 0,
            started_at_ms: 0,
            params: &params,
            spatial: SpatialHelpers::new(&room, &openings),
        };
        let mut effect = SolarEffect::new();
        let out = effect.tick(&ctx);
        if let LightAction::BrightnessTransition { value: b, .. } = out[0].action {
            assert!(
                b <= 150,
                "expected max_brightness=150 to cap at 150, got {b}"
            );
        }
    }

    #[test]
    fn ct_warmth_zero_keeps_cool_white() {
        let (room, bulbs, openings, solar) = ctx_with(
            vec![bulb("b1", 0.5, 0.5, 0.0, FixtureType::CeilingSpot)],
            vec![],
            -5.0, // below horizon — raw CT would be 500 (very warm)
            180.0,
        );
        let params = serde_json::json!({ "ct_warmth": 0.0 });
        let ctx = EffectCtx {
            room: &room,
            bulbs: &bulbs,
            openings: &openings,
            solar,
            now_ms: 0,
            started_at_ms: 0,
            params: &params,
            spatial: SpatialHelpers::new(&room, &openings),
        };
        let mut effect = SolarEffect::new();
        let out = effect.tick(&ctx);
        if let LightAction::ColorTempTransition { value: ct, .. } = out[1].action {
            assert_eq!(
                ct, 153,
                "ct_warmth=0.0 should keep CT at 153 mireds (cool white)"
            );
        }
    }

    #[test]
    fn ct_warmth_partial_interpolates() {
        let (room, bulbs, openings, solar) = ctx_with(
            vec![bulb("b1", 0.5, 0.5, 0.0, FixtureType::CeilingSpot)],
            vec![],
            0.0, // at horizon — raw CT = 500
            180.0,
        );
        let params = serde_json::json!({ "ct_warmth": 0.5 });
        let ctx = EffectCtx {
            room: &room,
            bulbs: &bulbs,
            openings: &openings,
            solar,
            now_ms: 0,
            started_at_ms: 0,
            params: &params,
            spatial: SpatialHelpers::new(&room, &openings),
        };
        let mut effect = SolarEffect::new();
        let out = effect.tick(&ctx);
        // expected: 153 + 0.5*(500-153) = 153 + 173.5 = 326 (rounded)
        if let LightAction::ColorTempTransition { value: ct, .. } = out[1].action {
            assert!(
                (ct as i32 - 327).abs() <= 1,
                "expected ~327 mireds at ct_warmth=0.5, got {ct}"
            );
        }
    }

    #[test]
    fn tick_brightness_stays_within_bounds() {
        let (room, bulbs, openings, solar) = ctx_with(
            vec![
                bulb("b1", 0.0, 0.0, 0.0, FixtureType::CeilingSpot),
                bulb("b2", 1.0, 1.0, 1.0, FixtureType::TableLamp),
            ],
            vec![],
            89.0,
            270.0,
        );
        let params = serde_json::json!({});
        let ctx = EffectCtx {
            room: &room,
            bulbs: &bulbs,
            openings: &openings,
            solar,
            now_ms: 0,
            started_at_ms: 0,
            params: &params,
            spatial: SpatialHelpers::new(&room, &openings),
        };
        let mut effect = SolarEffect::new();
        for cmd in effect.tick(&ctx) {
            if let LightAction::BrightnessTransition { value: b, .. } = cmd.action {
                assert!(b >= 1, "brightness {b} below clamp");
            }
        }
    }
}
