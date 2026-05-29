//! Last Emitted State cache (per-room, per-device).
//!
//! The runner records what was actually dispatched to each bulb on the most
//! recent successful tick. This single structure earns its keep three times:
//!
//! 1. **Dedup gate** — a tick whose output differs from LES by less than the
//!    perceptual threshold (brightness Δ≥2, xy Δ≥0.005, CT Δ≥4 mireds) is
//!    dropped before hitting the Zigbee dispatch path.
//! 2. **Effect→effect blend anchor** — the incoming effect's first second of
//!    output is blended with the LES so the room never reverts to baseline
//!    between two active effects.
//! 3. **Override-respect input** — bulbs flagged as manually overridden are
//!    skipped in tick output; LES tells the runner what "ignored" looked like.
//!
//! Updated *after* successful dispatch (a dedupped or override-skipped command
//! does **not** update LES). Cleared per-room when the room's effect is
//! disabled with no successor. Seeded from `light_states` on coordinator
//! restart (the runner re-hydrates LES from each bulb's last known state).

use std::collections::HashMap;

/// Colour mode the bulb is currently in, as far as the runner knows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ColorState {
    /// Bulb is off, CT-only, or has no colour information yet.
    None,
    /// Mireds — colour-temperature mode.
    Ct(u16),
    /// CIE xy — full-colour mode.
    Xy { x: f32, y: f32 },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LastEmittedState {
    pub on: bool,
    pub brightness: u8,
    pub color: ColorState,
    pub ts_ms: u64,
}

/// Map device_id → LES entry. One per room held by the runner.
pub type DeviceLes = HashMap<String, LastEmittedState>;

/// The full LES cache: room_id → device_id → LES entry.
#[derive(Debug, Default)]
pub struct RoomLes {
    rooms: HashMap<String, DeviceLes>,
}

impl RoomLes {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, room_id: &str, device_id: &str) -> Option<&LastEmittedState> {
        self.rooms.get(room_id).and_then(|d| d.get(device_id))
    }

    pub fn record(&mut self, room_id: &str, device_id: &str, entry: LastEmittedState) {
        self.rooms
            .entry(room_id.to_string())
            .or_default()
            .insert(device_id.to_string(), entry);
    }

    /// Drop everything the runner remembers about a room — used when the
    /// room's active effect is disabled with no successor.
    pub fn clear_room(&mut self, room_id: &str) {
        self.rooms.remove(room_id);
    }

    pub fn devices(&self, room_id: &str) -> Option<&DeviceLes> {
        self.rooms.get(room_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(brightness: u8) -> LastEmittedState {
        LastEmittedState {
            on: true,
            brightness,
            color: ColorState::Ct(370),
            ts_ms: 0,
        }
    }

    #[test]
    fn record_then_get_round_trips() {
        let mut les = RoomLes::new();
        les.record("room-a", "bulb-1", entry(80));
        assert_eq!(les.get("room-a", "bulb-1"), Some(&entry(80)));
    }

    #[test]
    fn record_overwrites_existing_entry() {
        let mut les = RoomLes::new();
        les.record("room-a", "bulb-1", entry(80));
        les.record("room-a", "bulb-1", entry(120));
        assert_eq!(les.get("room-a", "bulb-1").unwrap().brightness, 120);
    }

    #[test]
    fn get_unknown_room_or_device_returns_none() {
        let les = RoomLes::new();
        assert!(les.get("nope", "nope").is_none());
    }

    #[test]
    fn clear_room_drops_all_entries_for_that_room() {
        let mut les = RoomLes::new();
        les.record("room-a", "bulb-1", entry(80));
        les.record("room-a", "bulb-2", entry(90));
        les.record("room-b", "bulb-3", entry(100));
        les.clear_room("room-a");
        assert!(les.get("room-a", "bulb-1").is_none());
        assert!(les.get("room-a", "bulb-2").is_none());
        assert!(les.get("room-b", "bulb-3").is_some());
    }
}
