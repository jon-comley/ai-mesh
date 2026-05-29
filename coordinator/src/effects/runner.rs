//! EffectRunner — schedules `Effect::tick()` for every active room.
//!
//! This is the single tokio task that owns the live effect state machine:
//!
//! - Rehydrates active effects from `room_effects` on coordinator start.
//! - Computes the current solar position once per tick and pushes it to the
//!   dashboard (replaces the old `SpatialEngine` task).
//! - For each room with an enabled effect, calls `effect.tick(&ctx)` at its
//!   declared cadence, dedups the output against the per-room LES cache, and
//!   dispatches the remaining commands via `DashboardState::send_to_node`.
//! - On activation (`activate`) and disable (`disable`) the runner is notified
//!   via `tokio::sync::Notify` so user input is reflected within one tick.
//!
//! Out of scope for this slice (tracked as TODOs):
//! - 1 s OKLCH blend on effect→effect handoff. The blend layer exists in
//!   `effects::blend`; wiring it through here lands when the second effect
//!   does (Sunset, F-Effects-2.4).
//! - Cadence drift EWMA + warning. Observability-only — Solar at 1/min won't
//!   surface drift; relevant when Aurora at 10 Hz lands (F-Effects-2.6).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use chrono::Utc;
use shared::messages::{LightAction, LightCommandRequest, LightTarget, MeshMessage};
use tokio::sync::Notify;
use tokio::time::sleep_until;
use tracing::{debug, info, warn};

use crate::http::state::DashboardState;
use crate::registry::Registry;

use super::blend::{ColorXy, mireds_to_xy};
use super::les::{ColorState, LastEmittedState, RoomLes};
use super::registry::EffectRegistry;
use super::{
    BulbCurrentState, BulbInRoom, Effect, EffectCtx, FixtureType, OpeningContext, PersistCadence,
    PreEffectSnapshot, RoomContext, SolarSample, SpatialHelpers,
};

/// Dedup thresholds — values below these are imperceptible to a human eye
/// looking at a bulb and cost a Zigbee round trip we don't need.
const BRIGHTNESS_DELTA: u8 = 2;
const CT_DELTA_MIREDS: u16 = 4;
const XY_DELTA: f32 = 0.005;

// Cadence drift detection — see `update_drift_ewma`.
const DRIFT_EWMA_ALPHA: f64 = 0.2;
const DRIFT_THRESHOLD_FRAC: f64 = 0.20;
const DRIFT_WARN_HOLD_MS: u64 = 30_000;
const DRIFT_WARN_THROTTLE_MS: u64 = 60_000;

/// One live effect on one room.
struct ActiveEffectInstance {
    room_id: String,
    effect: Box<dyn Effect>,
    params: serde_json::Value,
    started_at_ms: u64,
    next_tick_at_ms: u64,
    persist_cadence: PersistCadence,
    last_persist_ms: Option<u64>,
    /// Time of the most-recent successful tick — used to compute the
    /// inter-tick interval that feeds the EWMA.
    last_tick_at_ms: Option<u64>,
    /// Exponential moving average of the observed interval, in ms. Compared
    /// against `effect.cadence().period_ms()` to detect sustained drift.
    ewma_interval_ms: Option<f64>,
    /// When the EWMA first crossed the drift threshold. Cleared whenever the
    /// interval recovers; warning fires once 30 s of sustained drift accrues.
    drifted_since_ms: Option<u64>,
    /// Last time we warned about drift on this instance. Used to throttle the
    /// warn log so a stuck Zigbee bus doesn't spam the journal.
    last_warned_ms: Option<u64>,
}

pub struct EffectRunner {
    registry: Arc<Mutex<Registry>>,
    dashboard: Arc<DashboardState>,
    effects: Arc<EffectRegistry>,
    latitude: f64,
    longitude: f64,
    state: Mutex<RunnerState>,
    notify: Arc<Notify>,
}

#[derive(Default)]
struct RunnerState {
    instances: HashMap<String, ActiveEffectInstance>,
    les: RoomLes,
}

impl EffectRunner {
    pub fn new(
        registry: Arc<Mutex<Registry>>,
        dashboard: Arc<DashboardState>,
        effects: Arc<EffectRegistry>,
    ) -> Arc<Self> {
        let latitude = std::env::var("MESH_LATITUDE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(51.5074); // London default — matches SpatialEngine
        let longitude = std::env::var("MESH_LONGITUDE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(-0.1278);

        // Reuse the solar_sweep_notify Notify on DashboardState so existing API
        // handlers that call `state.solar_sweep_notify.notify_one()` keep
        // waking the runner without any wiring change.
        let notify = dashboard.solar_sweep_notify.clone();

        Arc::new(Self {
            registry,
            dashboard,
            effects,
            latitude,
            longitude,
            state: Mutex::new(RunnerState::default()),
            notify,
        })
    }

    /// Entry point — runs forever. Spawn via `tokio::spawn(runner.run())`.
    pub async fn run(self: Arc<Self>) {
        info!(
            latitude = self.latitude,
            longitude = self.longitude,
            "EffectRunner started"
        );

        // Initial rehydration of any effect rows from the DB.
        self.rehydrate_from_db();

        loop {
            // Compute solar once per loop iteration and push to dashboard. This
            // replaces SpatialEngine's loop and keeps `DashboardEvent::SolarUpdate`
            // firing regardless of whether any room is running an effect.
            let solar = self.compute_solar();
            self.dashboard
                .push_solar_update(solar.azimuth_degrees, solar.elevation_degrees);

            // Tick every effect whose next_tick_at is due.
            let now_ms = now_ms();
            self.tick_due(now_ms, &solar);

            // Wait until the earliest next tick, or until notified that the
            // active-effect map changed.
            let next_due_ms = self.earliest_next_tick_ms().unwrap_or(now_ms + 1_000);
            let wait_for = next_due_ms.saturating_sub(now_ms).max(50);
            let deadline = tokio::time::Instant::now() + Duration::from_millis(wait_for);
            tokio::select! {
                _ = sleep_until(deadline) => {}
                _ = self.notify.notified() => {
                    // Could be activate/disable from the HTTP layer — rehydrate
                    // and tick everything that's now due immediately.
                    self.rehydrate_from_db();
                }
            }
        }
    }

    /// Build (or rebuild) `ActiveEffectInstance` entries from the DB. Idempotent:
    /// effects that are still active keep their in-memory state; rows that have
    /// been disabled are dropped; new rows are constructed via the registry.
    fn rehydrate_from_db(&self) {
        let live_rows = {
            let reg = self.registry.lock().unwrap();
            reg.list_active_effects()
        };
        let live_keys: std::collections::HashSet<String> =
            live_rows.iter().map(|r| r.room_id.clone()).collect();

        let mut state = self.state.lock().unwrap();

        // Drop instances whose room no longer has an active effect.
        state
            .instances
            .retain(|room_id, _| live_keys.contains(room_id));
        for room_id in state
            .instances
            .keys()
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .filter(|k| !live_keys.contains(k))
        {
            state.les.clear_room(&room_id);
        }

        // Add any rows that don't yet have an in-memory instance, or that
        // changed effect_id (effect swap).
        for row in live_rows {
            let needs_construct = match state.instances.get(&row.room_id) {
                Some(inst) => inst.effect.id() != row.effect_id,
                None => true,
            };
            if !needs_construct {
                continue;
            }
            let Some(mut effect) = self.effects.instantiate(&row.effect_id) else {
                warn!(effect_id = %row.effect_id, room_id = %row.room_id, "unknown effect_id in room_effects — skipping");
                continue;
            };
            if let Some(state_json) = &row.internal_state_json
                && let Ok(v) = serde_json::from_str::<serde_json::Value>(state_json)
                && let Err(e) = effect.deserialize_internal_state(v)
            {
                warn!(error = %e, effect_id = %row.effect_id, "deserialize_internal_state failed — continuing with fresh state");
            }
            let params: serde_json::Value =
                serde_json::from_str(&row.params_json).unwrap_or(serde_json::json!({}));
            let persist_cadence = effect.persist_cadence();
            let started_at_ms = row.started_at_ms.max(0) as u64;
            let instance = ActiveEffectInstance {
                room_id: row.room_id.clone(),
                effect,
                params,
                started_at_ms,
                next_tick_at_ms: now_ms(), // tick immediately
                persist_cadence,
                last_persist_ms: row.internal_state_json.as_ref().map(|_| started_at_ms),
                last_tick_at_ms: None,
                ewma_interval_ms: None,
                drifted_since_ms: None,
                last_warned_ms: None,
            };
            state.instances.insert(row.room_id.clone(), instance);
        }
    }

    fn earliest_next_tick_ms(&self) -> Option<u64> {
        let state = self.state.lock().unwrap();
        state.instances.values().map(|i| i.next_tick_at_ms).min()
    }

    fn tick_due(self: &Arc<Self>, now_ms: u64, solar: &SolarSample) {
        // Snapshot the rooms that are due. We process one room at a time and
        // release the lock between rooms so HTTP handlers can interleave.
        let due_room_ids: Vec<String> = {
            let state = self.state.lock().unwrap();
            state
                .instances
                .values()
                .filter(|i| i.next_tick_at_ms <= now_ms)
                .map(|i| i.room_id.clone())
                .collect()
        };

        for room_id in due_room_ids {
            if let Err(e) = self.tick_one(&room_id, now_ms, solar) {
                warn!(room_id = %room_id, error = %e, "effect tick failed");
            }
        }
    }

    fn tick_one(
        self: &Arc<Self>,
        room_id: &str,
        now_ms: u64,
        solar: &SolarSample,
    ) -> Result<(), String> {
        // Snapshot what we need for ctx construction.
        let (room_ctx, openings, bulbs) = self
            .build_ctx_inputs(room_id)
            .ok_or_else(|| "room/devices not available".to_string())?;

        // Build the effect call frame (params + started_at + spatial). We do
        // the actual tick + dispatch outside the runner-state lock so the
        // effect can panic-safely.
        let (commands, internal_state, persist_cadence, effect_id_owned) = {
            let mut state = self.state.lock().unwrap();
            let Some(inst) = state.instances.get_mut(room_id) else {
                return Err("instance gone".into());
            };
            let ctx = EffectCtx {
                room: &room_ctx,
                bulbs: &bulbs,
                openings: &openings,
                solar: *solar,
                now_ms,
                started_at_ms: inst.started_at_ms,
                params: &inst.params,
                spatial: SpatialHelpers::new(&room_ctx, &openings),
            };
            let commands = inst.effect.tick(&ctx);
            let internal_state = match inst.persist_cadence {
                PersistCadence::OnEnableOnly if inst.last_persist_ms.is_none() => {
                    inst.effect.serialize_internal_state()
                }
                PersistCadence::Periodic(d) => {
                    let since = inst
                        .last_persist_ms
                        .map(|t| now_ms.saturating_sub(t))
                        .unwrap_or(u64::MAX);
                    if since >= d.as_millis() as u64 {
                        inst.effect.serialize_internal_state()
                    } else {
                        None
                    }
                }
                _ => None,
            };
            let persist_cadence = inst.persist_cadence;
            let effect_id_owned = inst.effect.id().to_string();
            let period = inst.effect.cadence().period_ms();
            inst.next_tick_at_ms = now_ms + period;
            if internal_state.is_some() {
                inst.last_persist_ms = Some(now_ms);
            }

            // Cadence drift EWMA — warn if observed inter-tick interval
            // sustains >20% drift from the effect's declared cadence for 30 s.
            if let Some(report) = update_drift_state(DriftInputs {
                now_ms,
                declared_ms: period,
                last_tick_at_ms: inst.last_tick_at_ms,
                ewma_interval_ms: &mut inst.ewma_interval_ms,
                drifted_since_ms: &mut inst.drifted_since_ms,
                last_warned_ms: &mut inst.last_warned_ms,
            }) {
                warn!(
                    room_id = %inst.room_id,
                    effect_id = %effect_id_owned,
                    declared_ms = report.declared_ms,
                    observed_ewma_ms = report.observed_ms,
                    drift_frac = report.drift_frac,
                    "effect cadence drift sustained for 30 s — Zigbee backpressure or runner overload",
                );
            }
            inst.last_tick_at_ms = Some(now_ms);

            (commands, internal_state, persist_cadence, effect_id_owned)
        };

        // Persist internal state outside the runner lock.
        if let Some(value) = internal_state {
            let json = value.to_string();
            let mut reg = self.registry.lock().unwrap();
            reg.update_effect_internal_state(room_id, &effect_id_owned, Some(&json));
        }
        let _ = persist_cadence; // future: drive PersistCadence::Periodic schedule

        // Dispatch commands with dedup.
        self.dispatch(room_id, &effect_id_owned, &bulbs, commands);

        Ok(())
    }

    /// Pull everything the EffectCtx needs from the registry + dashboard.
    /// Returns (room context, openings, bulbs) where `bulbs` is every bulb
    /// assigned to the room — effects operate on the full set and decide for
    /// themselves whether to emit a command for each.
    fn build_ctx_inputs(
        &self,
        room_id: &str,
    ) -> Option<(RoomContext, Vec<OpeningContext>, Vec<BulbInRoom>)> {
        let (room, room_openings, positions, room_device_ids) = {
            let reg = self.registry.lock().unwrap();
            let rooms = reg.list_rooms();
            let room = rooms.iter().find(|r| r.id == room_id).cloned()?;
            let openings = reg
                .get_all_openings_by_room()
                .get(room_id)
                .cloned()
                .unwrap_or_default();
            let positions = reg.get_all_light_positions();
            let device_ids = room.device_ids.clone();
            (room, openings, positions, device_ids)
        };

        let room_ctx = RoomContext {
            id: room.id.clone(),
            orientation_degrees: room.orientation_degrees,
            width_m: room.width_m,
            depth_m: room.depth_m,
            height_m: room.height_m,
        };
        let opening_ctxs: Vec<OpeningContext> = room_openings
            .iter()
            .map(|o| OpeningContext {
                wall_edge: o.wall_edge.clone(),
                width_norm: o.width_norm,
                transmission: o.transmission,
            })
            .collect();

        let light_snapshot = self.dashboard.get_light_snapshot();
        let mut bulbs: Vec<BulbInRoom> = Vec::new();
        for device_id in &room_device_ids {
            let pos = positions.get(device_id);
            let (x, y, z, fixture_str) = pos
                .map(|p| (p.x, p.y, p.z, p.fixture_type.clone()))
                .unwrap_or((0.0, 0.0, 0.0, None));
            let report = light_snapshot.iter().find(|r| &r.device_id == device_id);
            let current = report
                .map(|r| BulbCurrentState {
                    on: r.on,
                    brightness: r.brightness,
                    color_xy: r.color_xy,
                    color_temp: r.color_temp,
                })
                .unwrap_or_default();
            bulbs.push(BulbInRoom {
                device_id: device_id.clone(),
                x,
                y,
                z,
                fixture_type: FixtureType::parse(fixture_str.as_deref()),
                current,
            });
        }
        Some((room_ctx, opening_ctxs, bulbs))
    }

    fn dispatch(
        self: &Arc<Self>,
        room_id: &str,
        effect_id: &str,
        bulbs: &[BulbInRoom],
        commands: Vec<super::EffectCommand>,
    ) {
        let mut state = self.state.lock().unwrap();
        for cmd in commands {
            let les_entry = state.les.get(room_id, &cmd.device_id).copied();
            if !should_dispatch(&les_entry, &cmd.action) {
                debug!(
                    device = %cmd.device_id,
                    effect = effect_id,
                    "dedup gate dropped sub-threshold command"
                );
                continue;
            }
            let Some(node_id) = self.dashboard.get_node_for_device(&cmd.device_id) else {
                continue;
            };
            let request_id = format!("{effect_id}-{}", Utc::now().timestamp_millis());
            let sent = self.dashboard.send_to_node(
                &node_id,
                MeshMessage::LightCommand(LightCommandRequest {
                    request_id,
                    target: LightTarget::Device(cmd.device_id.clone()),
                    command: cmd.action.clone(),
                }),
            );
            if !sent {
                warn!(device = %cmd.device_id, "send_to_node failed — channel full or absent");
                continue;
            }

            // Update LES with the resolved values. We only know the on/brightness/color
            // from the action itself + the existing LES context.
            let baseline = les_entry.unwrap_or(LastEmittedState {
                on: bulbs
                    .iter()
                    .find(|b| b.device_id == cmd.device_id)
                    .map(|b| b.current.on)
                    .unwrap_or(true),
                brightness: bulbs
                    .iter()
                    .find(|b| b.device_id == cmd.device_id)
                    .and_then(|b| b.current.brightness)
                    .unwrap_or(0),
                color: ColorState::None,
                ts_ms: 0,
            });
            let next = apply_action_to_les(baseline, &cmd.action);
            state.les.record(room_id, &cmd.device_id, next);
        }
    }

    fn compute_solar(&self) -> SolarSample {
        let now = Utc::now();
        match spa::calc_solar_position(now, self.latitude, self.longitude) {
            Ok(p) => SolarSample {
                azimuth_degrees: p.azimuth,
                elevation_degrees: 90.0 - p.zenith_angle,
            },
            Err(e) => {
                warn!(error = ?e, "solar position calculation failed; reporting darkness");
                SolarSample {
                    azimuth_degrees: 0.0,
                    elevation_degrees: -90.0,
                }
            }
        }
    }
}

// ── Dedup gate ──────────────────────────────────────────────────────────────

// ── Cadence drift ────────────────────────────────────────────────────────────

/// Inputs to `update_drift_state`. The mutable fields belong to the
/// `ActiveEffectInstance` and are written through this struct.
struct DriftInputs<'a> {
    now_ms: u64,
    declared_ms: u64,
    last_tick_at_ms: Option<u64>,
    ewma_interval_ms: &'a mut Option<f64>,
    drifted_since_ms: &'a mut Option<u64>,
    last_warned_ms: &'a mut Option<u64>,
}

/// Returned when a drift warning should fire. Caller logs it; this function
/// keeps no I/O of its own so it can be unit-tested.
struct DriftReport {
    declared_ms: u64,
    observed_ms: f64,
    drift_frac: f64,
}

/// Update the EWMA of observed inter-tick intervals and, if drift has been
/// sustained for `DRIFT_WARN_HOLD_MS` past `DRIFT_THRESHOLD_FRAC`, return a
/// report the caller should warn about. Returns `None` while drift is absent
/// or hasn't held long enough, or while we're inside the warn-throttle
/// window.
fn update_drift_state(io: DriftInputs<'_>) -> Option<DriftReport> {
    let DriftInputs {
        now_ms,
        declared_ms,
        last_tick_at_ms,
        ewma_interval_ms,
        drifted_since_ms,
        last_warned_ms,
    } = io;

    let last = last_tick_at_ms?;
    let observed = now_ms.saturating_sub(last) as f64;
    let smoothed = match *ewma_interval_ms {
        Some(prev) => DRIFT_EWMA_ALPHA * observed + (1.0 - DRIFT_EWMA_ALPHA) * prev,
        None => observed,
    };
    *ewma_interval_ms = Some(smoothed);

    let declared = declared_ms as f64;
    let drift_frac = (smoothed - declared).abs() / declared.max(1.0);
    if drift_frac <= DRIFT_THRESHOLD_FRAC {
        *drifted_since_ms = None;
        return None;
    }

    let since = *drifted_since_ms.get_or_insert(now_ms);
    let held_for = now_ms.saturating_sub(since);
    let throttled = last_warned_ms
        .map(|t| now_ms.saturating_sub(t) < DRIFT_WARN_THROTTLE_MS)
        .unwrap_or(false);
    if held_for >= DRIFT_WARN_HOLD_MS && !throttled {
        *last_warned_ms = Some(now_ms);
        Some(DriftReport {
            declared_ms,
            observed_ms: smoothed,
            drift_frac,
        })
    } else {
        None
    }
}

fn should_dispatch(les: &Option<LastEmittedState>, action: &LightAction) -> bool {
    let Some(les) = les else {
        return true;
    };
    match action {
        LightAction::Brightness(b) | LightAction::BrightnessTransition { value: b, .. } => {
            (les.brightness as i16 - *b as i16).unsigned_abs() >= BRIGHTNESS_DELTA as u16
        }
        LightAction::ColorTemp(ct) | LightAction::ColorTempTransition { value: ct, .. } => {
            match les.color {
                ColorState::Ct(prev) => {
                    (prev as i16 - *ct as i16).unsigned_abs() >= CT_DELTA_MIREDS
                }
                _ => true,
            }
        }
        LightAction::ColorXY { x, y } | LightAction::ColorXYTransition { x, y, .. } => {
            match les.color {
                ColorState::Xy { x: px, y: py } => ((px - *x).hypot(py - *y)) >= XY_DELTA,
                _ => true,
            }
        }
        // On/Off/Toggle — always pass.
        _ => true,
    }
}

fn apply_action_to_les(prev: LastEmittedState, action: &LightAction) -> LastEmittedState {
    let mut next = prev;
    next.ts_ms = now_ms();
    match action {
        LightAction::On => next.on = true,
        LightAction::Off => next.on = false,
        LightAction::Toggle => next.on = !next.on,
        LightAction::Brightness(b) | LightAction::BrightnessTransition { value: b, .. } => {
            next.brightness = *b;
            next.on = *b > 0;
        }
        LightAction::ColorTemp(ct) | LightAction::ColorTempTransition { value: ct, .. } => {
            next.color = ColorState::Ct(*ct);
        }
        LightAction::ColorXY { x, y } | LightAction::ColorXYTransition { x, y, .. } => {
            next.color = ColorState::Xy { x: *x, y: *y };
        }
    }
    next
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ── Snapshot helpers ─────────────────────────────────────────────────────────

/// Capture the room's current light state as the pre-effect baseline. Used by
/// `EffectRunner::activate()` (added in the next slice when API endpoints
/// drive activation) — kept here so the dispatch path can share it.
#[allow(dead_code)]
pub fn snapshot_room_state(
    dashboard: &DashboardState,
    room_device_ids: &[String],
) -> PreEffectSnapshot {
    let mut bulbs = HashMap::new();
    for report in dashboard.get_light_snapshot() {
        if !room_device_ids.iter().any(|d| d == &report.device_id) {
            continue;
        }
        bulbs.insert(
            report.device_id.clone(),
            super::BulbBaselineState {
                on: report.on,
                brightness: report.brightness,
                color_xy: report.color_xy,
                color_temp: report.color_temp,
            },
        );
    }
    PreEffectSnapshot { bulbs }
}

// Silence unused-import warning until the next slice consumes these.
const _: fn(ColorXy) -> ColorXy = |c| {
    let _ = mireds_to_xy(370);
    c
};

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(brightness: u8, color: ColorState) -> LastEmittedState {
        LastEmittedState {
            on: true,
            brightness,
            color,
            ts_ms: 0,
        }
    }

    #[test]
    fn dedup_drops_brightness_delta_1() {
        let les = Some(entry(80, ColorState::None));
        assert!(!should_dispatch(&les, &LightAction::Brightness(81)));
        assert!(!should_dispatch(&les, &LightAction::Brightness(79)));
    }

    #[test]
    fn dedup_passes_brightness_delta_2() {
        let les = Some(entry(80, ColorState::None));
        assert!(should_dispatch(&les, &LightAction::Brightness(82)));
        assert!(should_dispatch(&les, &LightAction::Brightness(78)));
    }

    #[test]
    fn dedup_first_command_always_passes() {
        // No LES entry → always dispatch.
        assert!(should_dispatch(&None, &LightAction::Brightness(50)));
        assert!(should_dispatch(&None, &LightAction::ColorTemp(370)));
    }

    #[test]
    fn dedup_color_temp_threshold() {
        let les = Some(entry(80, ColorState::Ct(370)));
        assert!(!should_dispatch(&les, &LightAction::ColorTemp(371))); // Δ 1
        assert!(!should_dispatch(&les, &LightAction::ColorTemp(373))); // Δ 3
        assert!(should_dispatch(&les, &LightAction::ColorTemp(374))); // Δ 4
    }

    #[test]
    fn dedup_xy_threshold() {
        let les = Some(entry(80, ColorState::Xy { x: 0.4, y: 0.5 }));
        // Distance well below XY_DELTA.
        assert!(!should_dispatch(
            &les,
            &LightAction::ColorXY {
                x: 0.4015,
                y: 0.5015
            }
        ));
        // Distance ~0.0064 — above XY_DELTA.
        assert!(should_dispatch(
            &les,
            &LightAction::ColorXY { x: 0.405, y: 0.504 }
        ));
    }

    #[test]
    fn dedup_mode_switch_always_sends() {
        // LES says CT mode; effect emits xy → must dispatch (no Δ across modes).
        let les = Some(entry(80, ColorState::Ct(370)));
        assert!(should_dispatch(
            &les,
            &LightAction::ColorXY { x: 0.4, y: 0.5 }
        ));
        // And the reverse direction.
        let les = Some(entry(80, ColorState::Xy { x: 0.4, y: 0.5 }));
        assert!(should_dispatch(&les, &LightAction::ColorTemp(370)));
    }

    #[test]
    fn dedup_on_off_always_pass() {
        let les = Some(entry(80, ColorState::None));
        assert!(should_dispatch(&les, &LightAction::On));
        assert!(should_dispatch(&les, &LightAction::Off));
        assert!(should_dispatch(&les, &LightAction::Toggle));
    }

    #[test]
    fn apply_action_brightness_updates_les_and_on_state() {
        let prev = entry(0, ColorState::None);
        let next = apply_action_to_les(prev, &LightAction::Brightness(120));
        assert_eq!(next.brightness, 120);
        assert!(next.on);
        let next = apply_action_to_les(prev, &LightAction::Brightness(0));
        assert_eq!(next.brightness, 0);
        assert!(!next.on);
    }

    #[test]
    fn apply_action_color_temp_records_ct_mode() {
        let prev = entry(80, ColorState::Xy { x: 0.4, y: 0.5 });
        let next = apply_action_to_les(prev, &LightAction::ColorTemp(370));
        assert!(matches!(next.color, ColorState::Ct(370)));
    }

    #[test]
    fn apply_action_color_xy_records_xy_mode() {
        let prev = entry(80, ColorState::Ct(370));
        let next = apply_action_to_les(prev, &LightAction::ColorXY { x: 0.4, y: 0.5 });
        assert!(
            matches!(next.color, ColorState::Xy { x, y } if (x - 0.4).abs() < 1e-6 && (y - 0.5).abs() < 1e-6)
        );
    }

    // ── Cadence drift EWMA ─────────────────────────────────────────────────────

    fn drift_at(
        now_ms: u64,
        declared_ms: u64,
        last_tick_at_ms: Option<u64>,
        ewma: &mut Option<f64>,
        since: &mut Option<u64>,
        warned: &mut Option<u64>,
    ) -> Option<DriftReport> {
        update_drift_state(DriftInputs {
            now_ms,
            declared_ms,
            last_tick_at_ms,
            ewma_interval_ms: ewma,
            drifted_since_ms: since,
            last_warned_ms: warned,
        })
    }

    #[test]
    fn drift_first_tick_has_no_history() {
        let mut ewma = None;
        let mut since = None;
        let mut warned = None;
        let report = drift_at(100, 100, None, &mut ewma, &mut since, &mut warned);
        assert!(report.is_none());
        assert!(ewma.is_none());
    }

    #[test]
    fn drift_on_target_does_not_warn() {
        // Feed exactly the declared interval for a long stretch — EWMA stays
        // on declared, drift_frac stays low, no warn.
        let declared = 100;
        let mut ewma = None;
        let mut since = None;
        let mut warned = None;
        let mut last = 0u64;
        for i in 1..=400 {
            let now = i * declared;
            let _ = drift_at(
                now,
                declared,
                Some(last),
                &mut ewma,
                &mut since,
                &mut warned,
            );
            last = now;
        }
        assert!(warned.is_none(), "no warn expected when on cadence");
        assert!(since.is_none());
        let ewma_val = ewma.unwrap();
        assert!(
            (ewma_val - declared as f64).abs() < 1.0,
            "EWMA should converge to declared: {ewma_val}"
        );
    }

    #[test]
    fn drift_transient_blip_does_not_warn() {
        // One slow tick, then everything recovers. drifted_since clears as soon
        // as the EWMA pulls back inside the band.
        let declared = 100;
        let mut ewma = None;
        let mut since = None;
        let mut warned = None;
        // Warm-up.
        let mut last = 0u64;
        for i in 1..=50 {
            let now = i * declared;
            let _ = drift_at(
                now,
                declared,
                Some(last),
                &mut ewma,
                &mut since,
                &mut warned,
            );
            last = now;
        }
        // One slow tick (200 ms instead of 100).
        let slow_now = last + 200;
        let _ = drift_at(
            slow_now,
            declared,
            Some(last),
            &mut ewma,
            &mut since,
            &mut warned,
        );
        last = slow_now;
        // EWMA may briefly cross the threshold, but as long as subsequent ticks
        // are on time the EWMA decays back inside it before the 30 s window
        // accrues.
        for i in 1..=400 {
            let now = last + i * declared;
            let _ = drift_at(
                now,
                declared,
                Some(last + (i - 1) * declared),
                &mut ewma,
                &mut since,
                &mut warned,
            );
        }
        assert!(warned.is_none(), "transient blip should not warn");
    }

    #[test]
    fn drift_sustained_30s_fires_warn_once() {
        // Steady interval 200 ms against declared 100 ms — sustained 100% drift.
        // First warn should fire at 30 s of drift; subsequent ticks within
        // throttle window should not re-warn.
        let declared = 100;
        let interval = 200;
        let mut ewma = None;
        let mut since = None;
        let mut warned = None;
        let mut warn_times: Vec<u64> = Vec::new();

        let mut last = 0u64;
        // 40 s worth of ticks at 200 ms apart = 200 ticks.
        for i in 1..=200 {
            let now = i * interval;
            if let Some(_r) = drift_at(
                now,
                declared,
                Some(last),
                &mut ewma,
                &mut since,
                &mut warned,
            ) {
                warn_times.push(now);
            }
            last = now;
        }
        assert_eq!(
            warn_times.len(),
            1,
            "expected exactly one warn during the throttle window, got {warn_times:?}"
        );
        // The warn should fire at roughly 30 s — first opportunity once drift
        // has held for DRIFT_WARN_HOLD_MS.
        let first = warn_times[0];
        assert!(
            (30_000..=33_000).contains(&first),
            "first warn should land near 30 s of drift, got {first} ms",
        );
    }
}
