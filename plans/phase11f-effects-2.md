# Phase 11F-Effects-2 — Effects Registry + Curated Library

> Solar is the first effect. F-Effects-2 turns "the first effect" into "any effect" — a registry the user can grow by writing one file per effect, plus an initial curated catalogue (Sunset, Sunrise, Candlelight, Aurora, Breathing, Telemetry) that exercises every spatial primitive the layout view exposes.

The current `rooms.solar_enabled bool` and the inline `if (effect === 'solar')` drop-handler are deliberate placeholders flagged in the F-Effects engineering notes. This phase replaces both with a registry pattern that scales to N effects without further plumbing changes, and ships the first batch of effects on top of it.

---

## Goals

1. **One file per effect.** Adding a new effect = drop a struct implementing `Effect` into `coordinator/src/effects/`, register it, ship. No schema changes, no API changes, no frontend changes.
2. **Per-room parameter customisation.** Each effect declares a JSON Schema for its params; the dashboard renders a generic editor; the coordinator persists per-room params.
3. **Spatial intelligence is reusable.** Helpers for "distance from west wall", "angle from window", "fixture archetype" live in one place and any effect can read them.
4. **Migration is silent.** Existing `rooms.solar_enabled = 1` rooms become `room_effects { effect_id: 'solar' }` rows on first start; nothing observable changes for the user.

## Non-goals

- The fast path (UDP/Entertainment) — still deferred to F10.
- Effect stacking / compositing — see Architecture Decision § 1.
- Scene library expansion via Intent-First — sketched at the end as F-Scenes-3.

---

## Architecture decisions (locked)

### 1. Single effect per room (MVP)

A room runs **zero or one** effect at a time. Applying a new effect disables the previous one. This matches scene semantics, matches user intuition ("the room is currently on Sunset"), and avoids the priority/compositor design work needed for stacking.

**When to revisit:** when Telemetry lands. Telemetry naturally wants to overlay (a brief pulse on top of whatever the room is doing). The clean answer at that point is to treat Telemetry as an *overlay* layer above the active effect rather than promoting effects to stacks. Overlay = absolute commands for short bursts that auto-revert; effect = ongoing baseline.

### 2. Effects are coordinator-side, tick-driven

Each effect implements `tick(&mut self, ctx: &EffectCtx) -> Vec<LightCommand>`. The `EffectRunner` task calls `tick` at the effect's preferred cadence (1 Hz for slow effects like Sunset, 5 Hz for breathing, 10 Hz for aurora drift). No effect runs faster than 10 Hz in MVP — the Z2M round-trip is the ceiling.

Why not async streams or a reactive model? Tick-driven is the simplest mental model, trivially testable (`tick(synthetic_ctx)` is a unit test), and matches how `SpatialEngine` already works. Reactive can come later if needed.

### 3. Params are JSON, schema is JSON Schema

Each effect returns a JSON Schema describing its tunable parameters. Frontend renders the editor automatically — no per-effect UI code. Examples:

- **Solar** — `{}` (no params)
- **Sunset** — `{ duration_secs: 1800, peak_warmth: 0.7, start_at?: "auto" | <ms epoch> }`
- **Breathing** — `{ period_secs: 8, min_brightness: 30, max_brightness: 200, colour_xy?: [x, y] }`

Params persist per-room. A room running Sunset at 30 minutes keeps that setting until edited.

### 4. Migration is one ALTER and one INSERT … SELECT

The existing `rooms.solar_enabled` column stays as a deprecated read-only mirror for one release (so a roll-back works) and is dropped in F-Effects-3.

---

## Data model

### New table: `room_effects`

```sql
CREATE TABLE IF NOT EXISTS room_effects (
    room_id              TEXT    NOT NULL,
    effect_id            TEXT    NOT NULL,                    -- "solar", "sunset", …
    enabled              INTEGER NOT NULL DEFAULT 1,          -- 0/1; off = paused, not removed
    params_json          TEXT    NOT NULL DEFAULT '{}',       -- serialized per-effect params
    snapshot_json        TEXT,                                -- nullable PreEffectSnapshot
    internal_state_json  TEXT,                                -- nullable per-effect runtime state
    started_at           INTEGER NOT NULL,                    -- ms epoch — for elapsed-time effects
    PRIMARY KEY (room_id, effect_id),
    FOREIGN KEY (room_id) REFERENCES rooms(id) ON DELETE CASCADE,
    CHECK (enabled IN (0, 1))
);

-- DB-level guarantee that at most one effect is enabled per room.
-- Application code still drives the transition; this is the integrity backstop
-- that fails loudly if a state bug ever tries to enable a second.
CREATE UNIQUE INDEX IF NOT EXISTS uid_enabled_room_effect
    ON room_effects (room_id)
    WHERE enabled = 1;
```

**Why composite PK + `enabled`?** Lets a room keep its Sunset params customised even when Solar is the currently-active effect. User flips back to Sunset and their saved params return.

**Why `snapshot_json` on the row?** The pre-effect light state (the "go back to this when I stop") needs to survive a coordinator restart so the user doesn't lose their baseline if the process crashes mid-effect. It also enables the **snapshot handoff** semantic described under § Effect lifecycle below — on Effect A → Effect B switch, A's snapshot transfers to B's row instead of being captured-then-restored-then-recaptured (which would briefly flicker the room through its baseline state between the two effects).

**Why `internal_state_json` on the row?** Some effects (Aurora, Candlelight, future games) have evolving per-bulb runtime state that isn't derivable from `(params, started_at, current_solar)` alone. The trait's `serialize_internal_state()` / `deserialize_internal_state()` pair (see § The trait) lets each effect declare what it needs to persist; the runner writes this column on a per-effect-chosen cadence (Never / OnEnableOnly / Periodic). Effects that don't need it leave the column null and pay zero cost.

**Constraint enforced in code _and_ DB:** the partial unique index above is the DB-level backstop. Application-level `set_active_effect(room_id, effect_id, params)` wraps the update in a transaction:

```rust
tx.execute("UPDATE room_effects SET enabled = 0 WHERE room_id = ?", [&room_id])?;
tx.execute(
    "INSERT OR REPLACE INTO room_effects (room_id, effect_id, enabled, params_json, snapshot_json, started_at)
     VALUES (?, ?, 1, ?, ?, ?)",
    params!(&room_id, &effect_id, &params_json, &snapshot_json, now_ms),
)?;
```

The `snapshot_json` value carried into the INSERT is either (a) the previous active effect's snapshot, transferred verbatim, or (b) a freshly captured snapshot of the room's current light state if no effect was previously active.

### Migration from `rooms.solar_enabled`

On coordinator start, after the existing `rooms` migrations:

```sql
INSERT OR IGNORE INTO room_effects (room_id, effect_id, enabled, params_json, started_at)
SELECT id, 'solar', 1, '{}', strftime('%s','now') * 1000
FROM rooms
WHERE solar_enabled = 1;
```

`solar_enabled` stays in the schema for one release as a read-only mirror updated by `set_active_effect` when `effect_id = 'solar'`. Removed in F-Effects-3.

---

## The trait

`coordinator/src/effects/mod.rs`:

```rust
pub trait Effect: Send + Sync {
    /// Stable kebab-case identifier persisted in `room_effects.effect_id`.
    /// Never change after release.
    fn id(&self) -> &'static str;

    /// User-facing name in the palette ("Sunset", "Aurora").
    fn display_name(&self) -> &'static str;

    /// One-liner shown in tooltip / inline help.
    fn description(&self) -> &'static str;

    fn category(&self) -> EffectCategory;

    /// JSON Schema describing tunable params. Empty object `{}` means no params.
    /// Used by the dashboard to render a param editor without per-effect frontend code.
    fn params_schema(&self) -> serde_json::Value;

    /// Default params instance (must validate against `params_schema`).
    fn default_params(&self) -> serde_json::Value;

    /// Cadence at which `tick()` should be called. Effects with cadence faster
    /// than the active LightCommand throttle still get called but their output
    /// is debounced upstream.
    fn cadence(&self) -> EffectCadence;

    /// Called at `cadence()` intervals. Returns the commands to send this tick.
    /// May return an empty Vec to skip a tick (e.g., Sunset before its start_at).
    fn tick(&mut self, ctx: &EffectCtx) -> Vec<LightCommand>;

    /// Called when the effect first becomes the active effect on a room with no
    /// previously-active effect. The default takes a snapshot of all room bulbs;
    /// most effects don't override this.
    fn on_enable(&mut self, ctx: &EffectCtx) -> PreEffectSnapshot {
        ctx.snapshot_room_state()
    }

    /// Called when the effect is being switched out for a different effect.
    /// The runner passes the existing snapshot through to the new effect — the
    /// outgoing effect does NOT restore state (the incoming one takes over the
    /// commands directly). Override only if the outgoing effect needs a last
    /// command (e.g., Aurora explicitly cancels its in-flight transitions).
    fn on_handoff(&mut self, _ctx: &EffectCtx) -> Vec<LightCommand> {
        vec![]
    }

    /// Called when the effect is disabled with no successor (going to idle).
    /// Default impl restores the snapshot with a 0.8s transition (matches
    /// scene-revert semantics).
    fn on_disable(&mut self, ctx: &EffectCtx, snap: &PreEffectSnapshot) -> Vec<LightCommand> {
        ctx.restore_with_transition(snap, 0.8)
    }

    /// If true, bulbs flagged "manually overridden" are skipped in `tick()`
    /// output so a user slider isn't fought by the effect. Default: true.
    /// Override to false only for effects that must own every bulb every tick
    /// (e.g., a strobe / safety-critical alert effect).
    fn respects_overrides(&self) -> bool { true }

    // ── Internal state persistence (opt-in) ─────────────────────────────────
    // Effects that need runtime state to survive a coordinator restart implement
    // these three. Default impls cover the most common case (no state at all).

    /// How often the runner should persist this effect's internal state.
    /// - `Never`         — effect is fully reconstructible from (params, started_at, snapshot, solar).
    ///                     Solar, Sunset, Sunrise, Breathing all return this.
    /// - `OnEnableOnly`  — effect needs a one-time seed but evolves deterministically from
    ///                     `(seed, elapsed)`. Aurora (temporal_phase seed), Candlelight
    ///                     (per-bulb RNG seed) return this.
    /// - `Periodic(d)`   — effect has genuinely evolving state that can't be reconstructed
    ///                     mathematically. Future game effects, Telemetry cooldowns.
    fn persist_cadence(&self) -> PersistCadence { PersistCadence::Never }

    /// Serialize whatever internal state this effect needs to resume after a restart.
    /// Called by the runner on the cadence above and on graceful coordinator shutdown.
    /// Default: no state.
    fn serialize_internal_state(&self) -> Option<serde_json::Value> { None }

    /// Restore internal state captured by a previous instance of this effect.
    /// Called once on coordinator startup before the first `tick()` for this room.
    /// Default: no-op. Effects that override `serialize_internal_state` must override
    /// this too. If deserialization fails, the runner logs at `warn!` and continues
    /// with a fresh instance — internal-state loss is annoying but not fatal.
    fn deserialize_internal_state(&mut self, _state: serde_json::Value) -> anyhow::Result<()> {
        Ok(())
    }
}

pub enum PersistCadence {
    Never,
    OnEnableOnly,
    Periodic(std::time::Duration),
}
```

### Persistence pattern: seed + elapsed > continuous writes

For `OnEnableOnly` effects, the recommended pattern is to persist a single seed at activation time and reconstruct evolved state purely from `(seed, now_ms - started_at_ms)`. Concrete example — Aurora:

```rust
pub struct AuroraEffect { seed: f32, params: AuroraParams }

impl Effect for AuroraEffect {
    fn persist_cadence(&self) -> PersistCadence { PersistCadence::OnEnableOnly }

    fn serialize_internal_state(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({ "seed": self.seed }))
    }

    fn deserialize_internal_state(&mut self, s: serde_json::Value) -> anyhow::Result<()> {
        self.seed = s["seed"].as_f64().ok_or(anyhow!("missing seed"))? as f32;
        Ok(())
    }

    fn tick(&mut self, ctx: &EffectCtx) -> Vec<LightCommand> {
        let phase = self.seed + (ctx.now_ms - ctx.started_at_ms) as f32 / 1000.0 * self.params.speed;
        // … sample palette at phase, emit commands
    }
}
```

After a restart 6 hours into Aurora, the engine reads the seed, computes `phase = seed + 21600.0 * speed`, and resumes at the exact perceptual point. No write happens between activation and shutdown for this effect.

Candlelight scales the same pattern to N independent stochastic streams from one persisted u64. Each bulb's flicker is its own random walk, but each walk is fully derived from `(master_seed, device_id, elapsed_ms)` — no per-bulb state is stored.

```rust
pub struct CandlelightEffect { master_seed: u64, params: CandlelightParams }

impl Effect for CandlelightEffect {
    fn persist_cadence(&self) -> PersistCadence { PersistCadence::OnEnableOnly }

    fn serialize_internal_state(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({ "master_seed": self.master_seed }))
    }

    fn deserialize_internal_state(&mut self, s: serde_json::Value) -> anyhow::Result<()> {
        self.master_seed = s["master_seed"].as_u64().ok_or(anyhow!("missing master_seed"))?;
        Ok(())
    }

    fn tick(&mut self, ctx: &EffectCtx) -> Vec<LightCommand> {
        let elapsed_ms = ctx.now_ms - ctx.started_at_ms;
        ctx.bulbs.iter().map(|bulb| {
            // Hash master_seed + device_id to derive this bulb's RNG stream.
            // Same (master, device_id) always produces the same Xoshiro state.
            let bulb_seed = mix(self.master_seed, hash_device_id(&bulb.device_id));
            let mut rng = Xoshiro256StarStar::seed_from_u64(bulb_seed);

            // Fast-forward to elapsed_ms / 200 (one walk step per 200 ms).
            // After restart this skips ahead to exactly where the walk would have been.
            let step = elapsed_ms / 200;
            rng.advance(step);  // O(log n) for Xoshiro — fast even at 6 hours

            let brightness = self.params.brightness_base + (rng.next_u32() % self.params.jitter) as u8;
            let mireds = 480 + (rng.next_u32() % 40) as u16;  // 2000 K ± a touch
            LightCommand::for_device(&bulb.device_id, brightness, ColorTemp(mireds))
        }).collect()
    }
}
```

Two properties worth calling out:
1. **No continuous writes.** The DB column `internal_state_json` is written exactly once at activation. The walk is replayed mathematically from `elapsed_ms` on every restart.
2. **`rng.advance(step)` is O(log n) for Xoshiro family RNGs.** Resuming Candlelight after a 12-hour restart costs the same amount of CPU as resuming after a 1-second restart — the runner doesn't pay a "fast-forward tax" proportional to elapsed time. LCG also supports cheap jump-ahead; the `rand_xoshiro` crate has it built in.

This is the pattern any future stochastic effect should follow. Periodic disk writes (`PersistCadence::Periodic(d)`) exist as an escape hatch for effects whose state genuinely cannot be reconstructed from `(seed, elapsed)` — none of the MVP catalogue need it.

pub enum EffectCategory {
    Ambient,     // Breathing, Candlelight
    TimeOfDay,   // Solar, Sunset, Sunrise
    Reactive,    // Telemetry (when F8 #1 lands)
    Game,        // Simon, Aurora (treated as a game in MVP)
}

pub enum EffectCadence {
    OnePerMinute,  // Solar
    OnePerSecond,  // Sunset, Sunrise
    FivePerSecond, // Breathing, Candlelight
    TenPerSecond,  // Aurora (cap)
}
```

### `EffectCtx`

Everything an effect needs to do its job, passed by reference into `tick`. No `&mut self` on registry data — effects own only their internal state.

```rust
pub struct EffectCtx<'a> {
    pub room: &'a RoomRecord,
    pub bulbs: &'a [BulbInRoom],         // device_id + position + fixture_type + current state
    pub openings: &'a [OpeningRecord],
    pub solar: SolarSample,              // current azimuth + elevation
    pub now_ms: u64,
    pub started_at_ms: u64,              // when this effect became active for this room
    pub params: serde_json::Value,
    pub spatial: SpatialHelpers<'a>,     // see below
}

pub struct BulbInRoom {
    pub device_id: String,
    pub x: f32, pub y: f32, pub z: f32,  // normalised 0–1
    pub fixture_type: FixtureType,
    pub current: LightState,             // last known on/brightness/CT/xy
}
```

### `SpatialHelpers` — the reuse layer

The whole point of "spatial effects" is that several effects want the same geometric questions answered. Centralise them:

```rust
pub struct SpatialHelpers<'a> { /* references room + openings */ }

impl<'a> SpatialHelpers<'a> {
    /// 0 = on the west wall (rotated by room orientation), 1 = on the east wall.
    pub fn west_to_east(&self, bulb: &BulbInRoom) -> f32;

    /// Angular separation between the bulb's "view of the window" and the sun.
    /// 0° = sun directly through the window onto the bulb.
    pub fn angle_to_sun(&self, bulb: &BulbInRoom) -> f32;

    /// 0 = no window within line-of-sight, 1 = bulb directly under the largest window.
    pub fn window_proximity(&self, bulb: &BulbInRoom) -> f32;

    /// "ceiling" | "mid" | "floor" — derived from fixture_type and z.
    pub fn altitude_band(&self, bulb: &BulbInRoom) -> AltitudeBand;

    /// Normalised 0–1 distance from a bulb to a specific wall (rotated by room
    /// orientation). Used by wall-wash, corridor-chase, and any effect with a
    /// "starting wall" concept.
    pub fn distance_to_wall(&self, bulb: &BulbInRoom, wall: Wall) -> f32;

    /// Computes a per-bulb time offset into a normalised [0, 1] effect curve based
    /// on a direction vector. Used by Sunset (west bias) and Sunrise (east bias).
    /// Returns offset in [-0.5, +0.5] so `t_bulb = clamp(t_global + offset, 0, 1)`.
    pub fn directional_offset(&self, bulb: &BulbInRoom, dir: Direction) -> f32;
}
```

`SpatialHelpers` is the single piece of code that knows about `orientation_degrees` + `openings` + `fixture_type` semantics. Effects ask questions; helpers answer.

### `LightCommand` already exists

`tick()` returns `Vec<LightCommand>` — the same struct the dispatch path already takes. No new wire format.

---

## Initial effect catalogue

The first six. Each one specifically exercises a different `SpatialHelpers` method so the helper API is shaken out before the registry is closed.

### 1. Solar (port)

Existing `SpatialEngine` becomes an `Effect`. Cadence: `OnePerMinute`. Params: `{}`. Exercises `angle_to_sun`, `window_proximity`. No behaviour change for end users.

### 2. Sunset

Sketched in the prior chat. Choreographed `duration_secs` curve: white → gold → orange → red → magenta → indigo → off. Per-bulb offset from `directional_offset(_, West)` + altitude_band. Ceiling spots run the full palette; table/floor lamps run a truncated curve (start off, ramp to warm amber, hold, fade). Cadence: `OnePerSecond`. Params:

```json
{
  "duration_secs": { "type": "integer", "default": 1800, "min": 60, "max": 7200 },
  "peak_warmth":   { "type": "number",  "default": 0.7,  "min": 0,  "max": 1   },
  "start_at":      { "type": "string",  "default": "now", "enum": ["now", "real-sunset"] }
}
```

`start_at: "real-sunset"` defers the curve until the real solar elevation crosses 0°.

### 3. Sunrise

Inverse palette of Sunset, east-biased via `directional_offset(_, East)`. Designed as a bedside wake-up: starts dark → deep red glow at lowest brightness → warm amber → cool morning white. Cadence: `OnePerSecond`. Same param shape; `start_at` accepts `"real-sunrise"`.

### 4. Candlelight

Per-bulb low-frequency flicker with subtle hue jitter around 2000 K. Each bulb runs its own pseudo-random walk seeded by `device_id` so neighbouring bulbs don't flicker in sync. Cadence: `FivePerSecond`. Params:

```json
{
  "intensity":  { "type": "number", "default": 0.6, "min": 0, "max": 1 },
  "brightness": { "type": "integer", "default": 80, "min": 10, "max": 200 }
}
```

Exercises: per-bulb seeded RNG without spatial input — proves effects can ignore the spatial helpers and still fit the framework.

### 5. Aurora

Slow drifting green/purple/cyan waves across the bulb grid. Treats the room as a 2D field; computes `t_field = (x * cos(θ) + y * sin(θ)) * spatial_freq + temporal_phase`; samples an OKLCH palette at `t_field`. Cadence: `TenPerSecond` (this is the only effect that needs the full cap). Params:

```json
{
  "speed":       { "type": "number", "default": 0.3, "min": 0.05, "max": 2.0 },
  "spatial_freq":{ "type": "number", "default": 1.5, "min": 0.5,  "max": 5.0 }
}
```

Exercises: 10 Hz tick rate + per-tick palette sampling. Will surface MQTT round-trip pain — informs the F10 fast-path decision.

### 6. Breathing

Single colour (default warm amber), brightness oscillates between `min_brightness` and `max_brightness` on a sine curve. All bulbs in phase. Cadence: `FivePerSecond`. Params shown earlier. Simplest possible effect — proves the registry doesn't require spatial input or palette interpolation. Good benchmark for "minimal effect implementation".

### Telemetry (F8 #1 — sketched, not built here)

Subscribes to inference start/end and node health events; emits short bursts on bulbs nearest the affected node's "anchor point" in the layout. Lands when Telemetry is prioritised.

---

## Engine

`coordinator/src/effects/runner.rs`:

```rust
pub struct EffectRunner {
    registry: Arc<EffectRegistry>,
    state: Arc<Mutex<HashMap<RoomId, ActiveEffectInstance>>>,
    registry_db: Arc<Registry>,
    state_chan: Arc<DashboardState>,
}

struct ActiveEffectInstance {
    effect: Box<dyn Effect>,
    params: serde_json::Value,
    pre_snapshot: PreEffectSnapshot,
    started_at_ms: u64,
    next_tick_at_ms: u64,
}
```

Single `tokio::task` loop:

```rust
loop {
    let next = self.earliest_due_ms();
    tokio::select! {
        _ = sleep_until(next) => self.run_due_ticks().await,
        _ = self.notify.notified() => {
            // Set-active-effect changed the room map; recompute due times.
        }
    }
}
```

`run_due_ticks` walks rooms with `next_tick_at_ms <= now`, calls `effect.tick(&ctx)`, dispatches the commands via existing `send_to_node`. Updates `next_tick_at_ms += cadence_to_ms()`. Failures (panic in tick) are caught and the effect is auto-disabled with an error logged.

The `notify` `tokio::sync::Notify` lets `set_active_effect` wake the runner immediately so changes are visible within a tick (~100 ms perceived) instead of waiting up to `cadence`.

### Effect → effect lifecycle

`set_active_effect` from inside the runner is **not** a "stop A, start B" sequence — it's a single transition. The state machine:

1. Read the outgoing effect's `snapshot_json` from the row.
2. Call `outgoing.on_handoff(&ctx)` — usually a no-op, but Aurora cancels in-flight transitions here.
3. Disable outgoing row.
4. Insert incoming row with `snapshot_json` *copied verbatim* from the outgoing row. `on_enable` is **not** called when there's a previously-active effect — the snapshot is already correct.
5. Notify the loop to re-tick immediately.

The room never sees a "reverted to baseline then re-styled" flicker. If the user instead *disables* without a successor (`DELETE /api/rooms/{id}/effect`), the runner calls `outgoing.on_disable(&ctx, &snap)` and the 0.8s restore transition runs normally.

### Last Emitted State cache (LES)

The runner maintains a per-room `HashMap<DeviceId, LastEmittedState>` recording what was actually dispatched to each bulb on the most recent tick. This is one structure that earns its keep three times over: it powers dedup, it's the anchor for effect→effect interpolation, and it tells the per-device override system what "ignored" looks like.

```rust
struct LastEmittedState {
    on: bool,
    brightness: u8,
    color: ColorState,
    ts_ms: u64,           // when this was sent
}

enum ColorState {
    None,                  // bulb is CT-only or off
    Ct(u16),               // mireds
    Xy { x: f32, y: f32 }, // CIE xy
}
```

Updated *after* successful dispatch (a command rejected by the dedup gate or skipped due to a `respects_overrides` override does **not** update LES). LES survives within the runner's lifetime; rebuilt from `LightState` snapshots on coordinator restart (each bulb's last known state from the SQLite `light_states` table seeds the LES).

### Output dedup (Zigbee congestion guard)

Tick output passes through a per-room dedup gate before dispatch. For each command in `effect.tick(...)`, compare the resolved target value against the bulb's LES entry:

```rust
let should_send = match (les.get(&cmd.device_id), &cmd.action) {
    (None, _) => true,
    (Some(les), LightAction::Brightness(b)) =>
        (les.brightness as i16 - *b as i16).abs() >= 2,
    (Some(les), LightAction::ColorXY { x, y }) => match les.color {
        ColorState::Xy { x: lx, y: ly } => (lx - x).hypot(ly - y) >= 0.005,
        _ => true,  // mode-switch always sends
    },
    (Some(les), LightAction::ColorTemp(ct)) => match les.color {
        ColorState::Ct(lct) => (lct as i16 - *ct as i16).abs() >= 4,
        _ => true,
    },
    _ => true,  // on/off, scenes, group commands always pass
};
```

Thresholds: brightness Δ < 2 of 255 is invisible; xy Δ < 0.005 is below the JND for almost all viewers; CT Δ < 4 mireds (~50 K at the warm end) is imperceptible. Aurora at 10 Hz will typically emit 1–2 commands per bulb per second after dedup instead of 10 — well within Z2M's comfort zone.

### Effect → effect interpolation

The 1-second blend at the boundary between two effects works in two stages: *property-level normalisation* (because effects are not required to emit the same properties on the same bulb on the same tick), then *perceptual blending*.

**Stage 1 — normalisation.** For each device the new effect emits a command for, the runner constructs a `BlendPoint`:

```rust
struct BlendPoint {
    brightness: u8,
    color: ColorXy,    // normalised to xy; CT converted via lookup
    on: bool,
}
```

- LES → BlendPoint A. If LES color is `Ct(mireds)`, convert to xy via the standard Kelvin → CIE xy formula (cached lookup table, 8 entries from 2000 K to 6500 K, linear interp between). If LES color is `None`, blend point uses the room's pre-effect snapshot for that bulb. If the device has no LES entry at all (effect didn't touch it before), no blend — the new command goes through as-is.
- New command → BlendPoint B, same normalisation.

**Stage 2 — blend.** Over `t ∈ [0, 1]` across the first second of the new effect (timer in the runner, ticking at the effect's cadence):

```rust
fn blend(a: BlendPoint, b: BlendPoint, t: f32) -> BlendPoint {
    BlendPoint {
        brightness: lerp_u8(a.brightness, b.brightness, t),
        color: oklch_lerp(a.color, b.color, t),  // perceptual, via `palette` crate
        on: if t < 0.5 { a.on } else { b.on },   // step at midpoint, avoids 50% flicker
    }
}
```

OKLCH interpolation matches what commercial lighting engines (Hue, LIFX, Nanoleaf) do internally for any cross-fade — red→blue passes through clean purple, not muddy grey. The output BlendPoint is then converted back into whichever `LightAction` shape the new effect declared (xy or CT) for dispatch.

**Missing-bulb case.** If the new effect doesn't emit a command for a bulb the LES knows about, that bulb is left alone — no command is sent for it during the transition. LES carries it forward; the dedup gate skips it because nothing changed.

**Mismatched-mode case.** If LES has `Ct(370)` and the new effect emits `ColorXY { 0.4, 0.5 }`, both are normalised to xy and blended in OKLCH — the bulb crosses from white-light gamut into colour gamut smoothly. Output command shape matches the new effect (xy in this case), so subsequent ticks continue in the new colour space without further coordination.

### The interpolation layer pays off twice

Cross-fade between effects is the immediate consumer of the BlendPoint + OKLCH layer, but the same machinery is reusable for any case where the runner needs to smoothly transition a room between two known light states:

- **Time scrubbing.** Dragging the layout-view scrubber from "now" to "4 pm" computes the predicted solar state at 4 pm and blends from the current LES into it — no jarring snap.
- **Effect parameter preview sliders.** Pulling Aurora's `speed` slider in the param editor can preview the new tempo by blending into the next tick's output rather than dropping the visible state and restarting the loop.
- **"Jump to midpoint" controls.** A 30-minute sunset has a `Jump to t=0.5` button — runner blends from current LES into the predicted t=0.5 state.
- **Scene recall over a configurable duration.** Scenes today use a 0.8 s hardware transition. With the BlendPoint layer, the runner can drive the transition itself with arbitrary easing curves and the bulb hardware sees only steady-state commands.

These are all out of scope for F-Effects-2 — but knowing the layer is reusable shapes the API: keep `blend(A, B, t)` and the BlendPoint type public to the rest of the coordinator crate, not buried as `pub(super)` inside the effects module.

### Cadence drift measurement

Each tick records `elapsed_since_last_tick`. The runner maintains an EWMA of actual cadence per room. If actual cadence drifts more than 20% from the effect's declared cadence for 30 consecutive seconds, the runner:

1. Logs at `warn!` with effect_id, room_id, declared/actual cadence, suspected cause (Z2M backpressure if dispatch is the bottleneck; runner overload if `next_tick_at_ms` is consistently behind).
2. Surfaces the actual cadence on the `DashboardEvent::EffectUpdate` payload so the badge tooltip can show "Aurora · 4.2 Hz (target 10 Hz)" — empirical input for the F10 fast-path decision without per-effect instrumentation.

### Internal-state persistence schedule

The runner also drives the `serialize_internal_state` / `deserialize_internal_state` lifecycle:

- **Activation** — after `on_enable`, if `persist_cadence != Never`, call `serialize_internal_state()` and write `internal_state_json` once.
- **Tick** — if `persist_cadence == Periodic(d)` and `now - last_persist_ms >= d`, persist after the tick.
- **Shutdown** — on graceful coordinator shutdown, persist all active effects once regardless of cadence (best-effort; bounded by a 500 ms total budget so a sluggish disk doesn't delay shutdown).
- **Coordinator start** — for every row in `room_effects WHERE enabled = 1`, construct the effect instance, call `deserialize_internal_state(internal_state_json)` if non-null; if it errors, drop the state and continue.

### Cadence drift measurement

Each tick records `elapsed_since_last_tick`. The runner maintains an EWMA of actual cadence per room. If actual cadence drifts more than 20% from the effect's declared cadence for 30 consecutive seconds, the runner:

1. Logs at `warn!` with effect_id, room_id, declared/actual cadence, suspected cause (Z2M backpressure if dispatch is the bottleneck; runner overload if `next_tick_at_ms` is consistently behind).
2. Surfaces the actual cadence on the `DashboardEvent::EffectUpdate` payload so the badge tooltip can show "Aurora · 4.2 Hz (target 10 Hz)" — empirical input for the F10 fast-path decision without per-effect instrumentation.

### Where does `SpatialEngine` go?

Becomes the `SolarEffect`'s implementation. The standalone `SpatialEngine` task is deleted in F-Effects-2 §1; its 60-second tick becomes the `Solar` effect's `OnePerMinute` cadence within `EffectRunner`. The `solar_sweep_notify` mechanism moves to the runner-wide `notify` channel.

---

## API

### Discovery

`GET /api/effects` — list of all registered effects with metadata.

```json
[
  {
    "id": "sunset",
    "display_name": "Sunset",
    "description": "Choreographed warm→indigo journey, west-biased.",
    "category": "TimeOfDay",
    "default_params": { "duration_secs": 1800, "peak_warmth": 0.7, "start_at": "now" },
    "params_schema": { /* JSON Schema */ }
  },
  …
]
```

Cacheable. Called once on dashboard load.

### Activation

`POST /api/rooms/{id}/effect` — sets the active effect.

```json
{ "effect_id": "sunset", "params": { "duration_secs": 1200 } }
```

Response: `204 No Content` on success. Validates `effect_id` is registered and `params` conforms to `params_schema`. Atomic — old effect's `on_disable` runs before new effect's `on_enable`.

`DELETE /api/rooms/{id}/effect` — disables the active effect (calls `on_disable`, returns bulbs to pre-effect snapshot). Response: `204`.

### Live updates

`DashboardEvent::EffectUpdate { room_id, effect_id: Option<String>, params: serde_json::Value }` broadcast on every change. Frontend updates the effect badge + active-chip state.

### Migration of existing endpoints

`POST /api/rooms/{id}/solar` and `POST /api/lights/{device}/restore-solar` keep working — they're rewritten internally to delegate to `set_active_effect("solar", {})` and the per-device override path respectively. Deprecated in F-Effects-3; removed in F-Effects-4.

---

## Frontend

### Effect palette

Currently a hand-rolled DOM chip strip. Becomes data-driven: render one chip per entry from `GET /api/effects`. Chip text = `display_name`, tooltip = `description`. Chip icon from a static map keyed by `effect_id` (sunset = orange gradient, solar = sun glyph, candlelight = flame, aurora = wave).

**Scaling beyond ~8 effects:** once the palette exceeds ~8 chips the strip becomes visually noisy. The `EffectCategory` enum already partitions effects into Ambient / TimeOfDay / Reactive / Game — when the catalogue crosses that threshold, group chips under category tabs (or a collapsed accordion on mobile). No backend changes needed; the metadata is already on `GET /api/effects`. Tracked here so the F8 catalogue expansion doesn't have to re-discover the design.

### Param editor (generic)

A single component `<EffectParamsEditor>` that reads `params_schema` and emits an updated params object. Supports the JSON Schema subset we use:

- `type: "integer" | "number"` with `min`/`max`/`default` → range slider with `min`/`max`/`step` set on the `<input type=range>` so the browser refuses out-of-range values before they hit JS
- `type: "string"` with `enum` → segmented button group (one button per enum value — no free input possible)
- `type: "boolean"` → switch
- (No nested objects or arrays in MVP.)

Constraints come from the schema. The editor never sends a value outside the declared range — the server's `jsonschema` validation is the backstop, not the first line of defence. A 400 from the server on a param change is a frontend bug, not a user error.

Opens when the user clicks the room's active effect badge. Saves on change with debounce; same code path as scenes.

### Effect badge

Replaces the current `☀ Solar` badge with a generic `<icon> <name>` badge for whatever effect is active. Click → param editor. Long-press (or right-click) → disable. Always the same UX regardless of effect.

### Drag-handler map

The roadmap engineering note (`if (effect === 'solar')`) becomes:

```js
async function activateEffect(roomId, effectId, params = null) {
  const body = params != null ? { effect_id: effectId, params } : { effect_id: effectId };
  await fetch(`/api/rooms/${roomId}/effect?token=…`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  });
}
```

Single function regardless of effect.

---

## Sub-phases

### F-Effects-2.1 — Registry + runtime plumbing

- [ ] `coordinator/src/effects/mod.rs`: `Effect` trait (incl. `respects_overrides`, `persist_cadence`, `serialize_internal_state`, `deserialize_internal_state`, `on_handoff`), `EffectCategory`, `EffectCadence`, `PersistCadence`, `EffectCtx`, `BulbInRoom`, `PreEffectSnapshot`.
- [ ] `coordinator/src/effects/registry.rs`: `EffectRegistry` (HashMap-backed, registered at coordinator startup), `register(Box<dyn Effect>)`, `get(id)`, `list_metadata()`.
- [ ] `coordinator/src/effects/les.rs`: `LastEmittedState`, `ColorState`, per-room LES map, seed-from-light-states helper.
- [ ] `coordinator/src/effects/blend.rs`: `BlendPoint`, CT↔xy normalisation table, OKLCH blend (via `palette` crate), `lerp_u8`.
- [ ] `room_effects` table (incl. `internal_state_json`) + partial unique index + migration from `rooms.solar_enabled`.
- [ ] `Registry::set_active_effect(room_id, effect_id, params)` + `get_active_effect(room_id)` with handoff semantics (snapshot transfer, no flicker).
- [ ] Port `SpatialEngine` to `SolarEffect`; delete the standalone task.
- [ ] `EffectRunner` task in `coordinator.rs`: tick scheduler, LES update, dedup gate, 1 s transition timer, internal-state persistence schedule, cadence drift EWMA.
- [ ] Unit tests:
  - [ ] Registry registration / list / get-by-id.
  - [ ] `set_active_effect` idempotence + handoff (snapshot transferred verbatim).
  - [ ] Partial unique index actually rejects a second `enabled = 1` row.
  - [ ] Migration from `solar_enabled = 1` produces the right row count and `effect_id = 'solar'`.
  - [ ] Dedup gate drops sub-threshold commands and passes super-threshold ones.
  - [ ] CT→xy normalisation matches reference values to 3 decimal places at 2700 K, 4000 K, 6500 K.
  - [ ] `blend(a, b, 0) == a`, `blend(a, b, 1) == b`, monotone in `t` per component.
  - [ ] `persist_cadence == Never` causes zero writes to `internal_state_json` across 100 ticks.
  - [ ] `persist_cadence == OnEnableOnly` writes exactly once at activation.

### F-Effects-2.2 — Discovery + activation API

- [ ] `GET /api/effects` returning metadata.
- [ ] `POST /api/rooms/{id}/effect` + `DELETE /api/rooms/{id}/effect`.
- [ ] `DashboardEvent::EffectUpdate` broadcast.
- [ ] `params` validation against `params_schema` (use `jsonschema` crate).
- [ ] Tests: bad effect_id → 400, bad params → 400, valid → 204 + WS event.

### F-Effects-2.3 — Frontend: generic palette + badge + param editor

- [ ] Replace inline palette HTML with data-driven render from `GET /api/effects`.
- [ ] `<EffectParamsEditor>` component (JSON-Schema → form).
- [ ] Generic effect badge replacing `☀ Solar`; click → params editor.
- [ ] Wire `EffectUpdate` WS event.
- [ ] Solar continues to work exactly as before — visible regression check.

### F-Effects-2.4 — Sunset (first new effect)

- [ ] Implement `SunsetEffect`.
- [ ] `SpatialHelpers::directional_offset` + `west_to_east`.
- [ ] OKLCH palette interpolation via `palette` crate.
- [ ] Per-fixture-type personality (ceiling vs. lamp curves).
- [ ] Manual test with the time scrubber: scrub through a 30-min sunset in 30s.

### F-Effects-2.5 — Sunrise + Breathing + Candlelight

- [ ] `SunriseEffect` (palette inverse + east bias).
- [ ] `BreathingEffect` (cadence + sine math; no spatial input).
- [ ] `CandlelightEffect` (per-bulb seeded RNG).
- [ ] First chance to refactor `SpatialHelpers` based on real friction.

### F-Effects-2.6 — Aurora + Telemetry stub

- [ ] `AuroraEffect` at 10 Hz — pressure-tests the runner cadence.
- [ ] If MQTT round-trip can't sustain 10 Hz, log a measured "actual cadence" per room and surface it in the badge tooltip. This is the empirical input for the F10 fast-path decision.
- [ ] Telemetry effect skeleton (subscribes to inference events but emits no commands) — placeholder for F8 #1.

### F-Effects-2.7 — Cleanup

- [ ] Drop `rooms.solar_enabled` column (gated behind a sentinel migration so old DBs still work).
- [ ] Remove `POST /api/rooms/{id}/solar` and `POST /api/lights/{device}/restore-solar` (replaced by generic effect endpoints).
- [ ] Roadmap entry; doc the trait shape so users adding effects in their own forks have a reference.

---

## Scene library expansion (F-Scenes-3, sketched, separate phase)

Scenes are static and already work. The interesting upgrade is **starter scenes** the user gets for free:

- **Activity:** Reading, Cooking, Work, Movie, Dinner, Cleaning
- **Mood:** Cozy, Energize, Romance, Focus, Unwind
- **Time of day:** Morning, Afternoon, Evening, Night

Two implementation paths:

1. **Hardcoded presets.** Reliable, instant, boring. One JSON file per scene with device-state recipes; coordinator clones a recipe into the user's scene list on first room create.
2. **Intent-First scenes (F8 #3).** Each starter is a *prompt*, not a recipe. "Cozy" gets resolved by the LLM at recall time using room layout + time + current weather. Magical when it works, costs ~300 ms latency, occasionally produces an off result.

**My recommendation for F-Scenes-3:** ship both — hardcoded presets as the immediate "starter pack" you see in a brand new room, *and* an Intent-First "🪄 Resolve" button on each preset that says "I want a fresh interpretation right now". Hardcoded gives the guarantee; Intent-First gives the surprise. Out of scope for F-Effects-2 — flagged here so the registry doesn't paint itself into a corner that prevents it.

---

## Test plan

Tests live in `coordinator/src/effects/tests/` and run via `cargo test -p coordinator effects::`. Each is a focused unit test using a `FakeDispatcher` (records commands instead of sending), a synthetic `EffectCtx`, and the real `EffectRunner` / `Registry` types — no MQTT, no Z2M, no real bulbs.

### Resume + persistence

- **`resume_sunset_after_restart`** — Start Sunset with `duration_secs = 1800`. Tick for 6 minutes of simulated time. Snapshot the runner state, drop it, instantiate a fresh runner from the DB. Assert: next tick output corresponds to `t = 0.24` ± 0.005 (24% through the curve).
- **`resume_aurora_seed_preserved`** — Start Aurora. Capture the first tick's emitted xy values per bulb. Drop the runner, reinstantiate. Tick once. Assert: emitted xy values match the pre-restart values for `t = elapsed`, not for `t = 0`.
- **`persist_cadence_never_zero_writes`** — Run Sunset for 1000 simulated ticks. Assert: `internal_state_json` column for that row is `NULL` throughout.
- **`persist_cadence_on_enable_one_write`** — Activate Aurora. Tick 100×. Assert: exactly one write to `internal_state_json` (immediately after activation).
- **`deserialize_error_is_recoverable`** — Manually corrupt `internal_state_json` to invalid JSON. Restart. Assert: warn logged, effect runs with fresh state, no panic.

### Lifecycle + handoff

- **`migration_solar_enabled_to_row`** — Seed DB with `rooms.solar_enabled = 1` for three rooms. Run migration. Assert: three rows in `room_effects` with `effect_id = 'solar'`, `enabled = 1`, snapshot = current bulb state.
- **`handoff_no_flicker`** — Activate Sunset on a room, tick once, capture LES per bulb. Activate Aurora directly (without disabling Sunset first). Assert: no `LightCommand` is ever sent that matches the pre-Sunset baseline values exactly; specifically, every bulb's first Aurora command is the blend midpoint at `t ≈ 0`.
- **`disable_to_idle_restores_snapshot`** — Activate Sunset, tick, then `DELETE /api/rooms/{id}/effect`. Assert: a final `LightCommand` sequence is dispatched matching the snapshot with a `transition_secs = 0.8` field set.
- **`partial_unique_index_rejects_second_enabled`** — Insert two `room_effects` rows for the same `room_id` both with `enabled = 1`. Assert: second INSERT returns `SqliteError::UniqueViolation`.

### Dedup gate

- **`dedup_drops_brightness_delta_1`** — LES has `brightness = 80`. Effect emits `Brightness(81)`. Assert: command not dispatched.
- **`dedup_passes_brightness_delta_2`** — LES has `brightness = 80`. Effect emits `Brightness(82)`. Assert: command dispatched, LES updated to 82.
- **`dedup_xy_threshold`** — LES `xy = (0.4, 0.5)`. Effect emits `(0.4015, 0.5015)` (dist 0.0021). Assert: dropped. Effect emits `(0.405, 0.504)` (dist 0.0064). Assert: passed.
- **`dedup_mode_switch_always_sends`** — LES is `Ct(370)`. Effect emits `ColorXY { 0.4, 0.5 }`. Assert: command dispatched regardless of any colour-space delta (we have no meaningful Δ to compute across modes).

### Interpolation

- **`blend_endpoints_exact`** — `blend(A, B, 0.0).brightness == A.brightness`; `blend(A, B, 1.0).brightness == B.brightness`.
- **`blend_monotone_brightness`** — For `t1 < t2`, `|blend(A, B, t1).brightness − A.brightness| < |blend(A, B, t2).brightness − A.brightness|`. (Same for OKLCH lightness.)
- **`blend_red_to_blue_passes_through_purple`** — Endpoints in xy: red (0.675, 0.322), blue (0.167, 0.040). Assert: at `t = 0.5`, blended xy is closer in OKLCH to magenta (≈0.32, 0.13) than to grey (≈0.31, 0.32). This is the headline win of OKLCH over naive xy lerp.
- **`blend_ct_to_xy_normalised`** — A = `Ct(370)`, B = `Xy(0.167, 0.040)`. Assert: blend converts A to xy via the lookup table; no NaN; output is a valid xy point throughout `t ∈ [0, 1]`.
- **`missing_bulb_no_command`** — Effect tick emits commands for bulbs 1 and 2; bulb 3 has an LES entry. Assert: bulb 3 receives no command during transition (LES carries it forward).

### Override respect

- **`respects_overrides_true_skips_overridden_bulbs`** — Mark bulb 2 as overridden. Effect with `respects_overrides() == true` ticks. Assert: bulbs 1 and 3 receive commands; bulb 2 does not; LES for bulb 2 unchanged.
- **`respects_overrides_false_forces_all`** — Mark bulb 2 as overridden. Effect with `respects_overrides() == false` ticks. Assert: all three bulbs receive commands; override flag is *not* cleared (effect is intruding, not taking ownership).

### Cadence drift

- **`drift_warning_fires_after_30s_at_20_pct`** — `FakeDispatcher` artificially delays dispatch by 250 ms (so a 100 ms cadence effect runs at 250 ms actual). Tick for 35 simulated seconds. Assert: exactly one `warn!` log emitted at ≈30 s; EffectUpdate WS event payload includes the measured cadence.

### Param validation

- **`activate_with_invalid_params_returns_400`** — POST to `/api/rooms/{id}/effect` with `effect_id = 'sunset', params = { duration_secs: 9999999 }` (above max). Assert: 400 response, `room_effects` row unchanged.

---

## Open questions

None outstanding for F-Effects-2.1 scope. The two raised in earlier drafts (resume-after-restart, effect→effect transition) are now both resolved in-spec — the first via the `serialize/deserialize` trait pair + `PersistCadence` enum, the second via the LES + per-property OKLCH blending layer.

---

## Why this is the right shape

- **Adding effect #11 costs a single file.** No SQL changes, no API changes, no frontend changes. That's the entire point.
- **Spatial intelligence is one helper, not seven.** `SpatialHelpers` becomes the place where new geometric questions land (e.g., "distance from kitchen island"), and every effect benefits.
- **The user experience scales linearly.** One chip per effect, one generic param editor, one badge UX. Adding effects doesn't add cognitive load until the palette gets long enough to need categorisation tabs — which the `EffectCategory` enum already accommodates.
- **Migration is a single SQL insert.** Existing rooms keep working. Nothing observable changes for any user during F-Effects-2.1.
- **The hard problems are isolated.** Cadence-vs-MQTT tension is in `EffectRunner`, not in every effect. Palette interpolation is in `SpatialHelpers`, not in every effect. Param validation is in the activation endpoint, not in every effect. Each effect gets to do exactly one job: derive commands from spatial context and elapsed time.
