//! Candlelight — per-bulb stochastic flicker around ~2000 K warm amber.
//!
//! Each bulb runs its own pseudo-random walk that's fully derived from
//! `(master_seed, device_id, step)` via a small mixing hash — no per-bulb RNG
//! state is carried tick-to-tick, so resuming the effect after a coordinator
//! restart is free (the formula is the same; we just plug in a new step).
//!
//! The master seed is generated once on first tick (via `getrandom`) and
//! persisted via `PersistCadence::OnEnableOnly`. After that the runner reads
//! the row's `internal_state_json` on coordinator restart and feeds it back
//! into `deserialize_internal_state`, so the flicker pattern is preserved
//! across the gap.
//!
//! Params:
//! - `intensity` — 0.0–1.0 jitter amplitude (default 0.6). 0 = no flicker;
//!   1 = the bulb may swing ~50 brightness units around the base.
//! - `brightness` — base brightness (10–254, default 80).

use shared::messages::LightAction;

use super::blend::mireds_to_xy;
use super::{Effect, EffectCadence, EffectCategory, EffectCommand, EffectCtx, PersistCadence};

pub struct CandlelightEffect {
    master_seed: Option<u64>,
}

impl CandlelightEffect {
    pub fn new() -> Self {
        Self { master_seed: None }
    }
}

impl Default for CandlelightEffect {
    fn default() -> Self {
        Self::new()
    }
}

impl Effect for CandlelightEffect {
    fn id(&self) -> &'static str {
        "candlelight"
    }

    fn display_name(&self) -> &'static str {
        "Candlelight"
    }

    fn description(&self) -> &'static str {
        "Each bulb softly flickers like a candle around 2000 K warm amber."
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
                "intensity": {
                    "type": "number",
                    "default": 0.6,
                    "minimum": 0,
                    "maximum": 1
                },
                "brightness": {
                    "type": "integer",
                    "default": 80,
                    "minimum": 10,
                    "maximum": 254
                }
            }
        })
    }

    fn default_params(&self) -> serde_json::Value {
        serde_json::json!({
            "intensity": 0.6,
            "brightness": 80
        })
    }

    fn persist_cadence(&self) -> PersistCadence {
        PersistCadence::OnEnableOnly
    }

    fn serialize_internal_state(&self) -> Option<serde_json::Value> {
        self.master_seed
            .map(|s| serde_json::json!({ "master_seed": s }))
    }

    fn deserialize_internal_state(&mut self, state: serde_json::Value) -> anyhow::Result<()> {
        self.master_seed = state.get("master_seed").and_then(|v| v.as_u64());
        Ok(())
    }

    fn tick(&mut self, ctx: &EffectCtx) -> Vec<EffectCommand> {
        // Lazy-init the master seed on the first tick. The runner persists it
        // immediately after (PersistCadence::OnEnableOnly) so a restart picks
        // up the same flicker pattern.
        let master_seed = self.ensure_seed();

        let intensity = ctx
            .params
            .get("intensity")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.6)
            .clamp(0.0, 1.0);
        let base = ctx
            .params
            .get("brightness")
            .and_then(|v| v.as_i64())
            .unwrap_or(80)
            .clamp(10, 254) as i32;

        // One independent random draw per cadence period per bulb. We derive
        // the step width from `self.cadence()` rather than hard-coding 200 ms
        // so a future cadence change can't desync the per-bulb walk from the
        // runner's tick schedule.
        let elapsed_ms = ctx.now_ms.saturating_sub(ctx.started_at_ms);
        let period_ms = self.cadence().period_ms().max(1);
        let step = elapsed_ms / period_ms;

        // Warm amber ~2000 K; same colour for every bulb so the flicker reads
        // as candle-shaped rather than disco-shaped.
        let warm = mireds_to_xy(500);

        // Maximum jitter swing in brightness units when intensity = 1.0. ±25
        // gives a perceptible flicker without making the bulb feel broken.
        let max_swing: f32 = 25.0;

        let mut out = Vec::with_capacity(ctx.bulbs.len() * 2);
        for bulb in ctx.bulbs {
            let bulb_hash = fnv1a_64(&bulb.device_id);
            let raw = mix3(master_seed, bulb_hash, step);
            // Map raw u64 → signed jitter in [-max_swing, +max_swing] scaled
            // by intensity.
            let signed: f32 = ((raw % 10_001) as f32 / 5_000.0) - 1.0; // [-1, 1]
            let jitter = signed * max_swing * intensity as f32;
            let brightness = (base as f32 + jitter).round().clamp(10.0, 254.0) as u8;

            out.push(EffectCommand {
                device_id: bulb.device_id.clone(),
                action: LightAction::Brightness(brightness),
            });
            out.push(EffectCommand {
                device_id: bulb.device_id.clone(),
                action: LightAction::ColorXY {
                    x: warm.x,
                    y: warm.y,
                },
            });
        }
        out
    }
}

impl CandlelightEffect {
    /// Lazily generates and stores the master seed. Called from tick() on the
    /// first activation; deserialize_internal_state would have populated it on
    /// a restart instead.
    fn ensure_seed(&mut self) -> u64 {
        if let Some(s) = self.master_seed {
            return s;
        }
        let mut bytes = [0u8; 8];
        // OS RNG; falls back to a clock-derived seed if it's somehow not
        // available (shouldn't happen on any supported platform).
        if getrandom::getrandom(&mut bytes).is_err() {
            let ns = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
            bytes = ns.to_le_bytes();
        }
        let seed = u64::from_le_bytes(bytes);
        self.master_seed = Some(seed);
        seed
    }
}

/// Deterministic FNV-1a 64-bit hash for device IDs. Stable across runs.
fn fnv1a_64(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Cheap stateless mix of three u64s — good enough for visible flicker but
/// not cryptographic. Same inputs always produce the same output.
fn mix3(a: u64, b: u64, c: u64) -> u64 {
    let mut x = a;
    x ^= b.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    x ^= c.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    x
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
        let e = CandlelightEffect::new();
        assert_eq!(e.id(), "candlelight");
        assert_eq!(e.category(), EffectCategory::Ambient);
        assert_eq!(e.cadence(), EffectCadence::FivePerSecond);
        assert!(matches!(e.persist_cadence(), PersistCadence::OnEnableOnly));
    }

    #[test]
    fn schema_validates_default_params() {
        let e = CandlelightEffect::new();
        let schema = jsonschema::JSONSchema::compile(&e.params_schema()).unwrap();
        assert!(schema.is_valid(&e.default_params()));
    }

    #[test]
    fn tick_lazily_generates_master_seed_and_serializes_it() {
        let mut e = CandlelightEffect::new();
        assert!(e.serialize_internal_state().is_none());

        let bulbs = vec![bulb("b")];
        let room = lounge();
        let openings = Vec::new();
        let params = serde_json::json!({});
        let ctx = make_ctx(&room, &bulbs, &openings, &params, 0, 0);
        let _ = e.tick(&ctx);

        let state = e.serialize_internal_state().expect("seed should be set");
        assert!(state.get("master_seed").is_some());
    }

    #[test]
    fn same_seed_produces_identical_output() {
        // Two fresh instances, same seed deserialized in → same emissions.
        let bulbs = vec![bulb("b1"), bulb("b2"), bulb("b3")];
        let room = lounge();
        let openings = Vec::new();
        let params = serde_json::json!({ "intensity": 0.6, "brightness": 80 });
        let ctx = make_ctx(&room, &bulbs, &openings, &params, 5_000, 0);

        let mut a = CandlelightEffect::new();
        a.deserialize_internal_state(serde_json::json!({ "master_seed": 42u64 }))
            .unwrap();
        let out_a = a.tick(&ctx);

        let mut b = CandlelightEffect::new();
        b.deserialize_internal_state(serde_json::json!({ "master_seed": 42u64 }))
            .unwrap();
        let out_b = b.tick(&ctx);

        assert_eq!(out_a, out_b);
    }

    #[test]
    fn different_bulbs_get_different_brightnesses() {
        // Per-bulb jitter should not collapse to one shared value.
        let bulbs = vec![bulb("alpha"), bulb("beta"), bulb("gamma"), bulb("delta")];
        let room = lounge();
        let openings = Vec::new();
        let params = serde_json::json!({ "intensity": 1.0, "brightness": 80 });
        let mut e = CandlelightEffect::new();
        e.deserialize_internal_state(serde_json::json!({ "master_seed": 12345u64 }))
            .unwrap();
        let ctx = make_ctx(&room, &bulbs, &openings, &params, 10_000, 0);
        let out = e.tick(&ctx);

        // Brightness command is every other emission.
        let brightnesses: Vec<u8> = out
            .iter()
            .step_by(2)
            .filter_map(|c| match c.action {
                LightAction::Brightness(b) => Some(b),
                _ => None,
            })
            .collect();
        let unique: std::collections::HashSet<u8> = brightnesses.iter().copied().collect();
        assert!(
            unique.len() >= 2,
            "expected per-bulb variation, all bulbs collapsed to: {brightnesses:?}",
        );
    }

    #[test]
    fn intensity_zero_means_no_jitter() {
        let bulbs = vec![bulb("b1"), bulb("b2"), bulb("b3")];
        let room = lounge();
        let openings = Vec::new();
        let params = serde_json::json!({ "intensity": 0.0, "brightness": 80 });
        let mut e = CandlelightEffect::new();
        e.deserialize_internal_state(serde_json::json!({ "master_seed": 7u64 }))
            .unwrap();
        let ctx = make_ctx(&room, &bulbs, &openings, &params, 10_000, 0);
        for cmd in e.tick(&ctx).iter().step_by(2) {
            assert!(matches!(cmd.action, LightAction::Brightness(80)));
        }
    }

    #[test]
    fn restart_via_persist_cycle_reproduces_brightness() {
        // Simulate the runner: tick once, persist, drop the effect, build a
        // fresh one, deserialize the persisted state, tick again at the
        // *same* simulated time — the output must match.
        let bulbs = vec![bulb("b1"), bulb("b2")];
        let room = lounge();
        let openings = Vec::new();
        let params = serde_json::json!({ "intensity": 0.7, "brightness": 100 });

        let mut a = CandlelightEffect::new();
        let ctx_a = make_ctx(&room, &bulbs, &openings, &params, 3_000, 0);
        let _ = a.tick(&ctx_a);
        let persisted = a.serialize_internal_state().expect("seed");

        let mut b = CandlelightEffect::new();
        b.deserialize_internal_state(persisted).unwrap();
        let ctx_b = make_ctx(&room, &bulbs, &openings, &params, 3_000, 0);
        assert_eq!(a.tick(&ctx_a), b.tick(&ctx_b));
    }
}
