//! Effects subsystem (F-Effects-2).
//!
//! This module defines the trait every effect implements and the supporting
//! types passed into `tick()`. The runtime (`EffectRunner`), per-room state
//! machine, and DB-backed `room_effects` table land in the next slice; this
//! file is the trait + types only.

use std::collections::HashMap;

use shared::messages::LightAction;

pub mod blend;
pub mod les;
pub mod registry;
pub mod runner;
pub mod solar;

// ── Categorisation + cadence ──────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EffectCategory {
    Ambient,
    TimeOfDay,
    Reactive,
    Game,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectCadence {
    OnePerMinute,
    OnePerSecond,
    FivePerSecond,
    TenPerSecond,
}

impl EffectCadence {
    pub fn period_ms(&self) -> u64 {
        match self {
            Self::OnePerMinute => 60_000,
            Self::OnePerSecond => 1_000,
            Self::FivePerSecond => 200,
            Self::TenPerSecond => 100,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistCadence {
    Never,
    OnEnableOnly,
    Periodic(std::time::Duration),
}

// ── Effect output ─────────────────────────────────────────────────────────────

/// One command produced by `Effect::tick()`. The runner attaches a request_id
/// and routes the device → node before dispatching.
#[derive(Debug, Clone, PartialEq)]
pub struct EffectCommand {
    pub device_id: String,
    pub action: LightAction,
}

// ── Bulb description for the tick context ────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixtureType {
    CeilingSpot,
    TableLamp,
    FloorLamp,
    LedStrip,
    Pendant,
    Unknown,
}

impl FixtureType {
    pub fn parse(s: Option<&str>) -> Self {
        match s.unwrap_or("") {
            "ceiling_spot" => Self::CeilingSpot,
            "table_lamp" => Self::TableLamp,
            "floor_lamp" => Self::FloorLamp,
            "led_strip" => Self::LedStrip,
            "pendant" => Self::Pendant,
            _ => Self::Unknown,
        }
    }
}

/// What the runner knows about a bulb at tick time.
#[derive(Debug, Clone)]
pub struct BulbInRoom {
    pub device_id: String,
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub fixture_type: FixtureType,
    pub current: BulbCurrentState,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct BulbCurrentState {
    pub on: bool,
    pub brightness: Option<u8>,
    pub color_xy: Option<(f32, f32)>,
    pub color_temp: Option<u16>,
}

// ── Pre-effect snapshot ──────────────────────────────────────────────────────

/// The room's light state at the moment an effect was activated. Persisted as
/// `room_effects.snapshot_json` so a restart can restore the baseline.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct PreEffectSnapshot {
    pub bulbs: HashMap<String, BulbBaselineState>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct BulbBaselineState {
    pub on: bool,
    pub brightness: Option<u8>,
    pub color_xy: Option<(f32, f32)>,
    pub color_temp: Option<u16>,
}

// ── Tick context ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct SolarSample {
    pub azimuth_degrees: f64,
    pub elevation_degrees: f64,
}

/// Lightweight room metadata the effect needs without dragging in the full
/// registry types.
#[derive(Debug, Clone)]
pub struct RoomContext {
    pub id: String,
    pub orientation_degrees: f32,
    pub width_m: f64,
    pub depth_m: f64,
    pub height_m: f64,
}

/// Lightweight opening descriptor — same fields as `registry::Opening` but
/// effects only need a subset.
#[derive(Debug, Clone)]
pub struct OpeningContext {
    pub wall_edge: String,
    pub width_norm: f32,
    pub transmission: f32,
}

pub struct EffectCtx<'a> {
    pub room: &'a RoomContext,
    pub bulbs: &'a [BulbInRoom],
    pub openings: &'a [OpeningContext],
    pub solar: SolarSample,
    pub now_ms: u64,
    pub started_at_ms: u64,
    pub params: &'a serde_json::Value,
    pub spatial: SpatialHelpers<'a>,
}

/// Geometric helpers that several effects want to share. The bodies are
/// stubbed in this scaffolding slice — real implementations land alongside
/// the effects that need them (Sunset/Sunrise/Aurora).
pub struct SpatialHelpers<'a> {
    _room: &'a RoomContext,
    _openings: &'a [OpeningContext],
}

impl<'a> SpatialHelpers<'a> {
    pub fn new(room: &'a RoomContext, openings: &'a [OpeningContext]) -> Self {
        Self {
            _room: room,
            _openings: openings,
        }
    }
}

// ── The trait ────────────────────────────────────────────────────────────────

pub trait Effect: Send + Sync {
    fn id(&self) -> &'static str;

    fn display_name(&self) -> &'static str;

    fn description(&self) -> &'static str;

    fn category(&self) -> EffectCategory;

    fn params_schema(&self) -> serde_json::Value;

    fn default_params(&self) -> serde_json::Value;

    fn cadence(&self) -> EffectCadence;

    fn tick(&mut self, ctx: &EffectCtx) -> Vec<EffectCommand>;

    fn on_enable(&mut self, ctx: &EffectCtx) -> PreEffectSnapshot {
        let mut bulbs = HashMap::new();
        for b in ctx.bulbs {
            bulbs.insert(
                b.device_id.clone(),
                BulbBaselineState {
                    on: b.current.on,
                    brightness: b.current.brightness,
                    color_xy: b.current.color_xy,
                    color_temp: b.current.color_temp,
                },
            );
        }
        PreEffectSnapshot { bulbs }
    }

    fn on_handoff(&mut self, _ctx: &EffectCtx) -> Vec<EffectCommand> {
        vec![]
    }

    fn on_disable(&mut self, _ctx: &EffectCtx, snap: &PreEffectSnapshot) -> Vec<EffectCommand> {
        // Default restore with 0.8s transition. Brightness + colour-temp where known.
        let mut out = Vec::new();
        for (device_id, baseline) in &snap.bulbs {
            if let Some(b) = baseline.brightness {
                out.push(EffectCommand {
                    device_id: device_id.clone(),
                    action: LightAction::BrightnessTransition {
                        value: b,
                        transition_secs: 0.8,
                    },
                });
            }
            if let Some(ct) = baseline.color_temp {
                out.push(EffectCommand {
                    device_id: device_id.clone(),
                    action: LightAction::ColorTempTransition {
                        value: ct,
                        transition_secs: 0.8,
                    },
                });
            } else if let Some((x, y)) = baseline.color_xy {
                out.push(EffectCommand {
                    device_id: device_id.clone(),
                    action: LightAction::ColorXYTransition {
                        x,
                        y,
                        transition_secs: 0.8,
                    },
                });
            }
        }
        out
    }

    fn respects_overrides(&self) -> bool {
        true
    }

    fn persist_cadence(&self) -> PersistCadence {
        PersistCadence::Never
    }

    fn serialize_internal_state(&self) -> Option<serde_json::Value> {
        None
    }

    fn deserialize_internal_state(&mut self, _state: serde_json::Value) -> anyhow::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cadence_periods() {
        assert_eq!(EffectCadence::OnePerMinute.period_ms(), 60_000);
        assert_eq!(EffectCadence::OnePerSecond.period_ms(), 1_000);
        assert_eq!(EffectCadence::FivePerSecond.period_ms(), 200);
        assert_eq!(EffectCadence::TenPerSecond.period_ms(), 100);
    }

    #[test]
    fn fixture_type_parses_known_strings() {
        assert_eq!(
            FixtureType::parse(Some("ceiling_spot")),
            FixtureType::CeilingSpot
        );
        assert_eq!(
            FixtureType::parse(Some("table_lamp")),
            FixtureType::TableLamp
        );
        assert_eq!(
            FixtureType::parse(Some("floor_lamp")),
            FixtureType::FloorLamp
        );
        assert_eq!(FixtureType::parse(Some("led_strip")), FixtureType::LedStrip);
        assert_eq!(FixtureType::parse(Some("pendant")), FixtureType::Pendant);
        assert_eq!(FixtureType::parse(Some("wat")), FixtureType::Unknown);
        assert_eq!(FixtureType::parse(None), FixtureType::Unknown);
    }
}
