//! Pure timeslot math for hunts — no wall-clock access here so it's
//! trivially unit-testable. The coordinator supplies real "now" (in local
//! time, so BST/GMT transitions are handled by the OS timezone database
//! rather than a naive UTC assumption silently drifting hunts by an hour
//! twice a year) via a thin wrapper that lives coordinator-side.

use std::time::Duration;

const SECS_PER_DAY: u32 = 24 * 60 * 60;

/// How long until the next slot fires, given `now_secs_since_midnight` and a
/// set of slots expressed as minutes-since-midnight. Picks the earliest slot
/// still ahead of `now` today, or the earliest slot at all (i.e. tomorrow) if
/// every slot today has already passed. Returns `Duration::MAX` if `slots` is
/// empty — callers should treat that as "never wake up" (a hunt with no
/// timeslots configured yet).
pub fn next_wakeup(now_secs_since_midnight: u32, slots: &[u16]) -> Duration {
    if slots.is_empty() {
        return Duration::MAX;
    }
    let now = now_secs_since_midnight % SECS_PER_DAY;
    let slot_secs: Vec<u32> = slots.iter().map(|&m| u32::from(m) * 60).collect();

    if let Some(&next) = slot_secs.iter().filter(|&&s| s > now).min() {
        return Duration::from_secs(u64::from(next - now));
    }
    // Every slot today has passed (or fires exactly now, which we also treat
    // as "already had this one") — wrap to the earliest slot tomorrow.
    let earliest = *slot_secs.iter().min().expect("slots is non-empty");
    let remaining_today = SECS_PER_DAY - now;
    Duration::from_secs(u64::from(remaining_today + earliest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_next_slot_later_today() {
        let now = 9 * 3600; // 09:00
        let slots = [8 * 60, 12 * 60, 18 * 60]; // 08:00, 12:00, 18:00
        assert_eq!(next_wakeup(now, &slots), Duration::from_secs(3 * 3600));
    }

    #[test]
    fn wraps_to_tomorrow_when_all_slots_today_have_passed() {
        let now = 20 * 3600; // 20:00
        let slots = [8 * 60, 12 * 60, 18 * 60];
        // Earliest tomorrow is 08:00 -> 4h remaining today + 8h tomorrow = 12h.
        assert_eq!(next_wakeup(now, &slots), Duration::from_secs(12 * 3600));
    }

    #[test]
    fn empty_slots_returns_max() {
        assert_eq!(next_wakeup(9 * 3600, &[]), Duration::MAX);
    }

    #[test]
    fn exact_boundary_at_a_slot_counts_as_passed() {
        // "Now" is exactly a slot's time — that slot doesn't fire again today.
        let now = 12 * 3600;
        let slots = [12 * 60, 18 * 60];
        assert_eq!(next_wakeup(now, &slots), Duration::from_secs(6 * 3600));
    }

    #[test]
    fn single_slot_wraps_correctly() {
        let now = 23 * 3600;
        let slots = [9 * 60]; // 09:00
        // 1h remaining today + 9h tomorrow.
        assert_eq!(next_wakeup(now, &slots), Duration::from_secs(10 * 3600));
    }
}
