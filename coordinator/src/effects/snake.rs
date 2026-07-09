//! Snake — a bright body glides through the room's bulbs in a zigzag path.
//!
//! Bulbs are ordered into a boustrophedon (row-by-row, alternating direction)
//! so the snake traverses them in a natural loop. The head is at full brightness;
//! the body fades linearly to off over `length` bulbs. All other bulbs are dark.
//!
//! Params:
//! - `speed_bps`      — head speed in bulbs per second (0.1–20, default 1.5).
//! - `length`         — body length in bulbs, fractional OK (0.5–8, default 2.0).
//! - `max_brightness` — head brightness (1–254, default 200).
//! - `colour` — preset colour: "red" (default), "green", "blue", "cyan",
//!   "purple", "amber", "white".

use std::cmp::Ordering;

use shared::messages::LightAction;

use super::blend::mireds_to_xy;
use super::{BulbInRoom, Effect, EffectCadence, EffectCategory, EffectCommand, EffectCtx};

fn colour_to_xy(name: &str) -> (f32, f32) {
    match name {
        "green" => (0.250, 0.650),
        "blue" => (0.152, 0.049),
        "cyan" => (0.200, 0.300),
        "purple" => (0.300, 0.100),
        "amber" => {
            let c = mireds_to_xy(500);
            (c.x, c.y)
        }
        "white" => (0.323, 0.329),
        _ => (0.700, 0.300), // "red" and anything unknown
    }
}

pub struct SnakeEffect {
    /// Path is cached and reused while `path_bulb_ids` still matches the
    /// bulb list so we pay the sort cost once per stable run, not every tick.
    path: Vec<usize>,
    /// Device IDs `path`'s indices refer to, in `ctx.bulbs` order — `ctx.bulbs`
    /// is already override-filtered upstream (`runner.rs`'s `active_bulbs`),
    /// so it can shrink/grow/reorder mid-run (a manual override toggle, a
    /// bulb added to the room, a dashboard reorder), not just once at
    /// startup. Caching the path forever regardless of this would leave
    /// `path`'s indices pointing at the wrong bulb, or out of bounds against
    /// a since-shrunk `ctx.bulbs` — the latter panics on the next `tick()`.
    path_bulb_ids: Vec<String>,
    last_color: Option<(f32, f32)>,
}

impl SnakeEffect {
    pub fn new() -> Self {
        Self {
            path: Vec::new(),
            path_bulb_ids: Vec::new(),
            last_color: None,
        }
    }

    /// Build or reuse a boustrophedon traversal order over `bulbs`. Reuses
    /// the cached path only when the bulb list (identity and order) is
    /// unchanged since it was built — see `path_bulb_ids`'s doc comment.
    /// Each row goes left-to-right, next row right-to-left, and so on.
    fn path_for(&mut self, bulbs: &[BulbInRoom]) {
        if !self.path.is_empty()
            && self.path_bulb_ids.len() == bulbs.len()
            && self
                .path_bulb_ids
                .iter()
                .zip(bulbs.iter())
                .all(|(cached, current)| cached == &current.device_id)
        {
            return; // bulb list unchanged since the path was built — reuse it
        }
        let n = bulbs.len();
        if n == 0 {
            self.path.clear();
            self.path_bulb_ids.clear();
            return;
        }

        // Sort indices by row (quantised y), then sort within each row.
        let row_of = |b: &BulbInRoom| (b.y * 5.0).floor() as i32;
        let mut indices: Vec<usize> = (0..n).collect();
        indices.sort_by(|&a, &b| {
            let ra = row_of(&bulbs[a]);
            let rb = row_of(&bulbs[b]);
            ra.cmp(&rb).then_with(|| {
                bulbs[a]
                    .x
                    .partial_cmp(&bulbs[b].x)
                    .unwrap_or(Ordering::Equal)
            })
        });

        let mut path = Vec::with_capacity(n);
        let mut i = 0;
        let mut left_to_right = true;
        while i < indices.len() {
            let row = row_of(&bulbs[indices[i]]);
            let row_end = (i..indices.len())
                .take_while(|&k| row_of(&bulbs[indices[k]]) == row)
                .count();
            let mut row_slice: Vec<usize> = indices[i..i + row_end].to_vec();
            // Already sorted by x ascending; just reverse on odd rows.
            if !left_to_right {
                row_slice.reverse();
            }
            path.extend(row_slice);
            i += row_end;
            left_to_right = !left_to_right;
        }
        self.path = path;
        self.path_bulb_ids = bulbs.iter().map(|b| b.device_id.clone()).collect();
    }
}

impl Default for SnakeEffect {
    fn default() -> Self {
        Self::new()
    }
}

impl Effect for SnakeEffect {
    fn id(&self) -> &'static str {
        "snake"
    }

    fn display_name(&self) -> &'static str {
        "Snake"
    }

    fn description(&self) -> &'static str {
        "A glowing snake slithers through the room's bulbs. The head is bright; the body fades to dark behind it."
    }

    fn category(&self) -> EffectCategory {
        EffectCategory::Game
    }

    fn cadence(&self) -> EffectCadence {
        EffectCadence::FivePerSecond
    }

    fn params_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "speed_bps": {
                    "type": "number",
                    "default": 1.5,
                    "minimum": 0.1,
                    "maximum": 20.0
                },
                "length": {
                    "type": "number",
                    "default": 2.0,
                    "minimum": 0.5,
                    "maximum": 8.0
                },
                "max_brightness": {
                    "type": "integer",
                    "default": 200,
                    "minimum": 1,
                    "maximum": 254
                },
                "colour": {
                    "type": "string",
                    "default": "red",
                    "enum": ["red", "green", "blue", "cyan", "purple", "amber", "white"]
                }
            }
        })
    }

    fn default_params(&self) -> serde_json::Value {
        serde_json::json!({
            "speed_bps": 1.5,
            "length": 2.0,
            "max_brightness": 200,
            "colour": "red"
        })
    }

    fn tick(&mut self, ctx: &EffectCtx) -> Vec<EffectCommand> {
        let n = ctx.bulbs.len();
        if n == 0 {
            return vec![];
        }

        self.path_for(ctx.bulbs);
        let n_f = n as f32;

        let speed = ctx
            .params
            .get("speed_bps")
            .and_then(|v| v.as_f64())
            .unwrap_or(1.5)
            .clamp(0.1, 20.0) as f32;

        let length = ctx
            .params
            .get("length")
            .and_then(|v| v.as_f64())
            .unwrap_or(2.0)
            .clamp(0.5, 8.0) as f32;

        let max_bri = ctx
            .params
            .get("max_brightness")
            .and_then(|v| v.as_i64())
            .unwrap_or(200)
            .clamp(1, 254) as f32;

        let colour_name = ctx
            .params
            .get("colour")
            .and_then(|v| v.as_str())
            .unwrap_or("red");
        let color = colour_to_xy(colour_name);

        let color_changed = self.last_color != Some(color);
        if color_changed {
            self.last_color = Some(color);
        }

        // Head position bounces back and forth along the path at speed_bps.
        // The effective cycle is 2*n (forward n, backward n).
        let elapsed_secs = ctx.now_ms.saturating_sub(ctx.started_at_ms) as f32 / 1000.0;
        let distance = (elapsed_secs * speed).rem_euclid(2.0 * n_f);
        let head_pos = if distance <= n_f {
            distance
        } else {
            2.0 * n_f - distance
        };

        // Transition slightly longer than the 200 ms tick so motion is continuous.
        const TRANSITION: f32 = 0.25;

        let mut out = Vec::with_capacity(n * if color_changed { 2 } else { 1 });

        for (slot, &bulb_idx) in self.path.iter().enumerate() {
            let bulb = &ctx.bulbs[bulb_idx];

            // dist = how far behind the head this slot sits (wrapping).
            // dist=0 → at head, dist=length → tail, dist>length → off.
            let dist = (head_pos - slot as f32).rem_euclid(n_f);

            let brightness = if dist < length {
                // Linear fade: head bright, tail dark.
                let t = dist / length;
                (max_bri * (1.0 - t)).round().clamp(1.0, 254.0) as u8
            } else {
                1 // essentially off
            };

            out.push(EffectCommand {
                device_id: bulb.device_id.clone(),
                action: LightAction::BrightnessTransition {
                    value: brightness,
                    transition_secs: TRANSITION,
                },
            });

            if color_changed {
                out.push(EffectCommand {
                    device_id: bulb.device_id.clone(),
                    action: LightAction::ColorXYTransition {
                        x: color.0,
                        y: color.1,
                        transition_secs: 0.5,
                    },
                });
            }
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

    fn lounge() -> RoomContext {
        RoomContext {
            id: "r1".into(),
            orientation_degrees: 0.0,
            width_m: 4.0,
            depth_m: 4.0,
            height_m: 2.4,
        }
    }

    fn make_ctx<'a>(
        room: &'a RoomContext,
        bulbs: &'a [BulbInRoom],
        params: &'a serde_json::Value,
        now_ms: u64,
        started_at_ms: u64,
    ) -> EffectCtx<'a> {
        let openings: &'a [OpeningContext] = &[];
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

    #[test]
    fn metadata_is_stable() {
        let e = SnakeEffect::new();
        assert_eq!(e.id(), "snake");
        assert_eq!(e.category(), EffectCategory::Game);
        assert_eq!(e.cadence(), EffectCadence::FivePerSecond);
    }

    #[test]
    fn schema_validates_default_params() {
        let e = SnakeEffect::new();
        let schema = jsonschema::JSONSchema::compile(&e.params_schema()).unwrap();
        assert!(schema.is_valid(&e.default_params()));
    }

    #[test]
    fn empty_bulbs_returns_no_commands() {
        let room = lounge();
        let params = serde_json::json!({});
        let ctx = make_ctx(&room, &[], &params, 1000, 0);
        let mut e = SnakeEffect::new();
        assert!(e.tick(&ctx).is_empty());
    }

    #[test]
    fn head_is_brightest_tail_is_dim() {
        // 4 bulbs in a line along x.
        let bulbs = vec![
            bulb("b0", 0.0, 0.5),
            bulb("b1", 0.33, 0.5),
            bulb("b2", 0.66, 0.5),
            bulb("b3", 1.0, 0.5),
        ];
        let room = lounge();
        let params = serde_json::json!({
            "speed_bps": 1.0,
            "length": 2.0,
            "max_brightness": 200
        });
        // t=0: head at position 0.0 → slot 0 (b0) is head.
        let ctx = make_ctx(&room, &bulbs, &params, 0, 0);
        let mut e = SnakeEffect::new();
        let out = e.tick(&ctx);

        // Collect brightness per device.
        let bri = |id: &str| -> u8 {
            out.iter()
                .find(|c| c.device_id == id)
                .map(|c| match c.action {
                    LightAction::BrightnessTransition { value, .. } => value,
                    _ => 0,
                })
                .unwrap_or(0)
        };

        // Path order: b0(slot0) → b1(slot1) → b2(slot2) → b3(slot3).
        // At t=0, head_pos=0 so slot 0 (b0) is the head.
        // The body wraps backward: slot 3 (dist=1) is the body, slot 2 (dist=2) is off.
        assert!(bri("b0") > 150, "head should be bright, got {}", bri("b0"));
        assert!(
            bri("b3") > 50,
            "body (slot behind head) should be medium, got {}",
            bri("b3")
        );
        assert_eq!(bri("b2"), 1, "b2 should be off");
        assert_eq!(bri("b1"), 1, "b1 should be off");
    }

    #[test]
    fn snake_advances_over_time() {
        // 4 bulbs in a line. At t=1s with speed=1 bps the head has moved ~1 bulb.
        let bulbs = vec![
            bulb("b0", 0.0, 0.5),
            bulb("b1", 0.33, 0.5),
            bulb("b2", 0.66, 0.5),
            bulb("b3", 1.0, 0.5),
        ];
        let room = lounge();
        let params = serde_json::json!({
            "speed_bps": 1.0,
            "length": 1.5,
            "max_brightness": 200
        });

        let ctx0 = make_ctx(&room, &bulbs, &params, 0, 0);
        let ctx1 = make_ctx(&room, &bulbs, &params, 1000, 0);

        let mut e = SnakeEffect::new();
        let out0 = e.tick(&ctx0);
        let out1 = e.tick(&ctx1);

        let bri_at = |out: &[EffectCommand], id: &str| -> u8 {
            out.iter()
                .find(|c| c.device_id == id)
                .map(|c| match c.action {
                    LightAction::BrightnessTransition { value, .. } => value,
                    _ => 0,
                })
                .unwrap_or(0)
        };

        // At t=0, b0 is brightest. At t=1s, b1 should be brightest.
        assert!(bri_at(&out0, "b0") > bri_at(&out0, "b1"));
        assert!(bri_at(&out1, "b1") > bri_at(&out1, "b0"));
    }

    #[test]
    fn colour_enum_green_is_applied() {
        let bulbs = vec![bulb("b0", 0.5, 0.5)];
        let room = lounge();
        let params = serde_json::json!({ "colour": "green" });
        let ctx = make_ctx(&room, &bulbs, &params, 0, 0);
        let mut e = SnakeEffect::new();
        let out = e.tick(&ctx);
        let color_cmd = out
            .iter()
            .find(|c| matches!(c.action, LightAction::ColorXYTransition { .. }))
            .expect("colour command on first tick");
        match color_cmd.action {
            LightAction::ColorXYTransition { x, y, .. } => {
                // green xy ≈ (0.250, 0.650)
                assert!((x - 0.250).abs() < 0.01, "x={x}");
                assert!((y - 0.650).abs() < 0.01, "y={y}");
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn boustrophedon_path_alternates_row_direction() {
        // 4 bulbs in 2 rows.
        // Row 0 (y=0.1): b_nw(x=0.2), b_ne(x=0.8)
        // Row 1 (y=0.7): b_sw(x=0.2), b_se(x=0.8)
        // Expected path: b_nw → b_ne → b_se → b_sw  (snake zigzag)
        let bulbs = vec![
            bulb("b_nw", 0.2, 0.1),
            bulb("b_ne", 0.8, 0.1),
            bulb("b_se", 0.8, 0.7),
            bulb("b_sw", 0.2, 0.7),
        ];
        let room = lounge();
        let params = serde_json::json!({});
        let ctx = make_ctx(&room, &bulbs, &params, 0, 0);
        let mut e = SnakeEffect::new();
        e.tick(&ctx); // populate path cache

        // Slot 0 = b_nw, slot 1 = b_ne, slot 2 = b_se, slot 3 = b_sw.
        let path_ids: Vec<&str> = e
            .path
            .iter()
            .map(|&i| bulbs[i].device_id.as_str())
            .collect();
        assert_eq!(path_ids, vec!["b_nw", "b_ne", "b_se", "b_sw"]);
    }

    #[test]
    fn path_recomputes_when_a_bulb_is_excluded_mid_run() {
        // ctx.bulbs is already override-filtered upstream (runner.rs) — an
        // override toggle mid-run shrinks it on the very next tick. A path
        // cached against the old (longer) bulb list must not be reused
        // as-is: its indices would point at the wrong bulb, or go out of
        // bounds against the new shorter slice.
        let all_four = vec![
            bulb("b_nw", 0.2, 0.1),
            bulb("b_ne", 0.8, 0.1),
            bulb("b_se", 0.8, 0.7),
            bulb("b_sw", 0.2, 0.7),
        ];
        let room = lounge();
        let params = serde_json::json!({});
        let mut e = SnakeEffect::new();

        let ctx0 = make_ctx(&room, &all_four, &params, 0, 0);
        e.tick(&ctx0); // builds and caches the 4-bulb path

        // b_ne gets manually overridden out of the effect — the next tick
        // only sees the remaining 3.
        let three = vec![
            all_four[0].clone(),
            all_four[2].clone(),
            all_four[3].clone(),
        ];
        let ctx1 = make_ctx(&room, &three, &params, 100, 0);
        let out = e.tick(&ctx1); // must not panic on a stale, longer cached path

        let commanded_ids: std::collections::HashSet<&str> =
            out.iter().map(|c| c.device_id.as_str()).collect();
        assert!(
            !commanded_ids.contains("b_ne"),
            "the excluded bulb must not receive commands: {commanded_ids:?}"
        );
        assert_eq!(
            e.path.len(),
            3,
            "cached path must be rebuilt to match the shrunk bulb list"
        );
    }

    #[test]
    fn path_reuses_cache_when_bulb_list_is_unchanged() {
        let bulbs = vec![
            bulb("b_nw", 0.2, 0.1),
            bulb("b_ne", 0.8, 0.1),
            bulb("b_se", 0.8, 0.7),
            bulb("b_sw", 0.2, 0.7),
        ];
        let room = lounge();
        let params = serde_json::json!({});
        let mut e = SnakeEffect::new();

        let ctx0 = make_ctx(&room, &bulbs, &params, 0, 0);
        e.tick(&ctx0);
        let path_after_first_tick = e.path.clone();

        let ctx1 = make_ctx(&room, &bulbs, &params, 100, 0);
        e.tick(&ctx1);
        assert_eq!(
            e.path, path_after_first_tick,
            "an unchanged bulb list must reuse the cached path, not rebuild it"
        );
    }
}
