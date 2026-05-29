//! Aurora — slow drifting green / cyan / purple waves across the bulb grid.
//!
//! Treats the room as a 2D field. For each bulb at room-local `(x, y)` we
//! compute a scalar along the wave direction:
//!
//! ```text
//! t = (x*cos(θ) + y*sin(θ)) * spatial_freq + seed + elapsed_s * speed
//! ```
//!
//! and sample a 4-keyframe Aurora palette at `t mod 1.0`. Bulbs further along
//! the wave direction (rotated by θ) lead the colour cycle; bulbs perpendicular
//! to it move in lockstep. The wave direction is hard-coded at 45° for MVP —
//! once a real "direction of drift" UX exists it becomes a param.
//!
//! Cadence: `TenPerSecond` — Aurora is the only effect that uses the full cap.
//! In practice the EffectRunner's dedup gate will drop most sub-threshold
//! commands so the actual MQTT load is ≪ 10 Hz per bulb.
//!
//! Params:
//! - `speed` — temporal_phase advance per second (0.05–2.0, default 0.3).
//! - `spatial_freq` — wave count across the room (0.5–5.0, default 1.5).

use shared::messages::LightAction;

use super::blend::{ColorXy, oklab_lerp};
use super::{Effect, EffectCadence, EffectCategory, EffectCommand, EffectCtx, PersistCadence};

/// Fixed wave direction across the room. East-by-northeast — a diagonal
/// across most landscape-oriented rooms looks naturally aurora-shaped.
const WAVE_ANGLE_DEGREES: f32 = 45.0;

pub struct AuroraEffect {
    /// Random initial phase, generated lazily on first tick and persisted via
    /// `serialize_internal_state`. After a coordinator restart we read this
    /// back, add `elapsed_since_started_at * speed`, and continue at the
    /// exact same visual point in the cycle.
    seed: Option<f32>,
}

impl AuroraEffect {
    pub fn new() -> Self {
        Self { seed: None }
    }

    fn ensure_seed(&mut self) -> f32 {
        if let Some(s) = self.seed {
            return s;
        }
        // Sample 4 bytes of OS randomness → u32 → f32 in [0.0, 1.0).
        let mut bytes = [0u8; 4];
        if getrandom::getrandom(&mut bytes).is_err() {
            // Fallback: clock-derived seed. Shouldn't happen on any platform.
            let ns = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or(0);
            bytes = ns.to_le_bytes();
        }
        let raw = u32::from_le_bytes(bytes);
        let s = (raw as f32) / (u32::MAX as f32);
        self.seed = Some(s);
        s
    }
}

impl Default for AuroraEffect {
    fn default() -> Self {
        Self::new()
    }
}

impl Effect for AuroraEffect {
    fn id(&self) -> &'static str {
        "aurora"
    }

    fn display_name(&self) -> &'static str {
        "Aurora"
    }

    fn description(&self) -> &'static str {
        "Slow drifting green / cyan / purple waves across the room."
    }

    fn category(&self) -> EffectCategory {
        EffectCategory::Game
    }

    fn cadence(&self) -> EffectCadence {
        EffectCadence::TenPerSecond
    }

    fn params_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "speed": {
                    "type": "number",
                    "default": 0.3,
                    "minimum": 0.05,
                    "maximum": 2.0
                },
                "spatial_freq": {
                    "type": "number",
                    "default": 1.5,
                    "minimum": 0.5,
                    "maximum": 5.0
                }
            }
        })
    }

    fn default_params(&self) -> serde_json::Value {
        serde_json::json!({
            "speed": 0.3,
            "spatial_freq": 1.5
        })
    }

    fn persist_cadence(&self) -> PersistCadence {
        PersistCadence::OnEnableOnly
    }

    fn serialize_internal_state(&self) -> Option<serde_json::Value> {
        self.seed.map(|s| serde_json::json!({ "seed": s }))
    }

    fn deserialize_internal_state(&mut self, state: serde_json::Value) -> anyhow::Result<()> {
        self.seed = state.get("seed").and_then(|v| v.as_f64()).map(|v| v as f32);
        Ok(())
    }

    fn tick(&mut self, ctx: &EffectCtx) -> Vec<EffectCommand> {
        let seed = self.ensure_seed();

        let speed = ctx
            .params
            .get("speed")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.3)
            .clamp(0.05, 2.0) as f32;
        let spatial_freq = ctx
            .params
            .get("spatial_freq")
            .and_then(|v| v.as_f64())
            .unwrap_or(1.5)
            .clamp(0.5, 5.0) as f32;

        let elapsed_s = ctx.now_ms.saturating_sub(ctx.started_at_ms) as f32 / 1000.0;
        let temporal_phase = seed + elapsed_s * speed;

        let theta = WAVE_ANGLE_DEGREES.to_radians();
        let (cos_t, sin_t) = (theta.cos(), theta.sin());

        let mut out = Vec::with_capacity(ctx.bulbs.len() * 2);
        for bulb in ctx.bulbs {
            let projection = bulb.x * cos_t + bulb.y * sin_t;
            let t = (projection * spatial_freq + temporal_phase).rem_euclid(1.0);
            let color = sample_palette(t);

            // Aurora always emits a fairly bright wash so the colour reads —
            // brightness is fixed so the user perceives the colour cycle, not
            // a brightness flicker.
            out.push(EffectCommand {
                device_id: bulb.device_id.clone(),
                action: LightAction::Brightness(200),
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

/// 4-keyframe Aurora palette that wraps: green → cyan → purple → green.
/// `t` is taken mod 1.0; the final keyframe at t=1.0 closes the loop.
const PALETTE: &[(f32, ColorXy)] = &[
    (0.00, ColorXy::new(0.250, 0.650)), // saturated green
    (0.33, ColorXy::new(0.200, 0.300)), // cyan
    (0.66, ColorXy::new(0.300, 0.100)), // purple
    (1.00, ColorXy::new(0.250, 0.650)), // green again (loop closure)
];

fn sample_palette(t: f32) -> ColorXy {
    let t = t.rem_euclid(1.0);
    let mut lo = 0;
    for (i, kf) in PALETTE.iter().enumerate() {
        if kf.0 >= t {
            lo = i.saturating_sub(1);
            break;
        }
        lo = i;
    }
    let hi = (lo + 1).min(PALETTE.len() - 1);
    let (t0, c0) = PALETTE[lo];
    let (t1, c1) = PALETTE[hi];
    let local_t = if (t1 - t0).abs() > f32::EPSILON {
        (t - t0) / (t1 - t0)
    } else {
        0.0
    };
    oklab_lerp(c0, c1, local_t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::{
        BulbCurrentState, BulbInRoom, EffectCtx, FixtureType, OpeningContext, RoomContext,
        SolarSample, SpatialHelpers,
    };

    fn bulb(id: &str, x: f32, y: f32) -> BulbInRoom {
        BulbInRoom {
            device_id: id.into(),
            x,
            y,
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
                azimuth_degrees: 0.0,
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
        let e = AuroraEffect::new();
        assert_eq!(e.id(), "aurora");
        assert_eq!(e.category(), EffectCategory::Game);
        assert_eq!(e.cadence(), EffectCadence::TenPerSecond);
        assert!(matches!(e.persist_cadence(), PersistCadence::OnEnableOnly));
    }

    #[test]
    fn schema_validates_default_params() {
        let e = AuroraEffect::new();
        let schema = jsonschema::JSONSchema::compile(&e.params_schema()).unwrap();
        assert!(schema.is_valid(&e.default_params()));
    }

    #[test]
    fn same_seed_produces_identical_output() {
        let bulbs = vec![bulb("b1", 0.2, 0.3), bulb("b2", 0.8, 0.7)];
        let room = lounge();
        let openings = Vec::new();
        let params = serde_json::json!({});
        let ctx = make_ctx(&room, &bulbs, &openings, &params, 5_000, 0);

        let mut a = AuroraEffect::new();
        a.deserialize_internal_state(serde_json::json!({ "seed": 0.42_f64 }))
            .unwrap();
        let out_a = a.tick(&ctx);

        let mut b = AuroraEffect::new();
        b.deserialize_internal_state(serde_json::json!({ "seed": 0.42_f64 }))
            .unwrap();
        let out_b = b.tick(&ctx);

        assert_eq!(out_a, out_b);
    }

    #[test]
    fn persist_round_trip_resumes_at_correct_phase() {
        // Tick at elapsed=5s, persist seed, drop, fresh instance gets seed
        // back, tick again at elapsed=5s — identical output.
        let bulbs = vec![bulb("b", 0.5, 0.5)];
        let room = lounge();
        let openings = Vec::new();
        let params = serde_json::json!({ "speed": 0.5, "spatial_freq": 2.0 });

        let mut a = AuroraEffect::new();
        let ctx_a = make_ctx(&room, &bulbs, &openings, &params, 5_000, 0);
        let _ = a.tick(&ctx_a);
        let persisted = a.serialize_internal_state().expect("seed should be set");

        let mut b = AuroraEffect::new();
        b.deserialize_internal_state(persisted).unwrap();
        let ctx_b = make_ctx(&room, &bulbs, &openings, &params, 5_000, 0);
        assert_eq!(a.tick(&ctx_a), b.tick(&ctx_b));
    }

    #[test]
    fn temporal_phase_wraps_via_rem_euclid() {
        // A bulb at room centre after a very long elapsed time still produces
        // a valid colour — proves the rem_euclid clamp keeps the palette lookup
        // sane.
        let bulbs = vec![bulb("b", 0.5, 0.5)];
        let room = lounge();
        let openings = Vec::new();
        let params = serde_json::json!({ "speed": 2.0, "spatial_freq": 5.0 });
        let mut e = AuroraEffect::new();
        e.deserialize_internal_state(serde_json::json!({ "seed": 0.0 }))
            .unwrap();
        let ctx = make_ctx(&room, &bulbs, &openings, &params, 3_600_000, 0);
        let out = e.tick(&ctx);
        // Look at the ColorXY emission (every other command).
        if let LightAction::ColorXY { x, y } = out[1].action {
            assert!((0.0..=1.0).contains(&x), "x out of range: {x}");
            assert!((0.0..=1.0).contains(&y), "y out of range: {y}");
        } else {
            panic!("expected ColorXY second emission");
        }
    }

    #[test]
    fn different_bulb_positions_get_different_colours_along_wave() {
        // Two bulbs along the wave direction (45°) → different t projections →
        // different palette samples → different ColorXY emissions.
        let bulbs = vec![bulb("near", 0.0, 0.0), bulb("far", 1.0, 1.0)];
        let room = lounge();
        let openings = Vec::new();
        let params = serde_json::json!({ "speed": 0.0, "spatial_freq": 1.5 });
        let mut e = AuroraEffect::new();
        e.deserialize_internal_state(serde_json::json!({ "seed": 0.0 }))
            .unwrap();
        let ctx = make_ctx(&room, &bulbs, &openings, &params, 0, 0);
        let out = e.tick(&ctx);

        let xy_near = match out[1].action {
            LightAction::ColorXY { x, y } => (x, y),
            _ => panic!(),
        };
        let xy_far = match out[3].action {
            LightAction::ColorXY { x, y } => (x, y),
            _ => panic!(),
        };
        // They should not collapse to the same point.
        let dist = ((xy_near.0 - xy_far.0).powi(2) + (xy_near.1 - xy_far.1).powi(2)).sqrt();
        assert!(
            dist > 0.05,
            "bulbs along wave should differ in colour: near={xy_near:?} far={xy_far:?}",
        );
    }

    #[test]
    fn palette_samples_stay_in_cyan_green_purple_band() {
        // Random spot checks across t — the OKLCH path should never wander
        // into orange/red.
        for raw in 0..10 {
            let t = raw as f32 / 10.0;
            let c = sample_palette(t);
            assert!((0.0..=1.0).contains(&c.x));
            assert!((0.0..=1.0).contains(&c.y));
            // Aurora keyframes all have x < 0.35 — no warm tints.
            assert!(
                c.x < 0.40,
                "palette sample at t={t} drifted warm: x={}",
                c.x
            );
        }
    }
}
