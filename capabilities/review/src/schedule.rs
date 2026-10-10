//! When a repo is due for a run, in local time (BST/GMT handled by chrono's
//! `Local`). Pure: the capability passes in "now" and the last time it ran.
//!
//! Rather than sleeping until the next slot, the capability checks every
//! minute whether a slot has passed since the last scheduled run. That way a
//! slot missed while mac1 was asleep or off still runs, once, when it wakes.

use chrono::{Datelike, Duration, NaiveDateTime, Timelike};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Due {
    Nightly,
    Sweep,
}

fn at_minutes(day: NaiveDateTime, minutes: u16) -> NaiveDateTime {
    day.date()
        .and_hms_opt(u32::from(minutes / 60) % 24, u32::from(minutes % 60), 0)
        .expect("valid time")
}

/// The most recent occurrence of each nightly slot at or before `now`,
/// looking back at most a day.
fn last_nightly(now: NaiveDateTime, slots: &[u16]) -> Option<NaiveDateTime> {
    slots
        .iter()
        .flat_map(|&m| {
            let today = at_minutes(now, m);
            [today, today - Duration::days(1)]
        })
        .filter(|t| *t <= now)
        .max()
}

/// The most recent weekly sweep time at or before `now` (`day` 0 = Monday).
fn last_sweep(now: NaiveDateTime, day: u8, slot: u16) -> Option<NaiveDateTime> {
    let today = i64::from(now.weekday().num_days_from_monday());
    let back = (today - i64::from(day % 7)).rem_euclid(7);
    let t = at_minutes(now - Duration::days(back), slot);
    if t <= now {
        Some(t)
    } else {
        Some(t - Duration::days(7))
    }
}

/// What, if anything, is due: a slot that has come round since `last_run`
/// (the last *scheduled* run; manual runs do not count). A sweep wins over a
/// nightly due at the same time, because the sweep covers more. With no
/// previous run, only a slot in the last hour counts, so adding a repo does
/// not set off a run for a slot from yesterday.
pub fn due(
    now: NaiveDateTime,
    last_run: Option<NaiveDateTime>,
    slots: &[u16],
    sweep: Option<(u8, u16)>,
) -> Option<Due> {
    let since = last_run.unwrap_or(now - Duration::hours(1));
    if let Some((day, slot)) = sweep
        && let Some(t) = last_sweep(now, day, slot)
        && t > since
    {
        return Some(Due::Sweep);
    }
    match last_nightly(now, slots) {
        Some(t) if t > since => Some(Due::Nightly),
        _ => None,
    }
}

/// Whether `now` is in the evening, when home commands are likely and review
/// tasks are kept small so a pause costs less (17:00–23:00).
pub fn is_evening(now: NaiveDateTime) -> bool {
    (17..23).contains(&now.hour())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn t(d: u32, h: u32, m: u32) -> NaiveDateTime {
        // October 2026: the 11th is a Sunday.
        NaiveDate::from_ymd_opt(2026, 10, d)
            .unwrap()
            .and_hms_opt(h, m, 0)
            .unwrap()
    }

    const SLOT_0215: u16 = 2 * 60 + 15;

    #[test]
    fn nightly_slot_fires_once() {
        assert_eq!(
            due(t(10, 2, 10), Some(t(9, 2, 16)), &[SLOT_0215], None),
            None
        );
        assert_eq!(
            due(t(10, 2, 16), Some(t(9, 2, 16)), &[SLOT_0215], None),
            Some(Due::Nightly)
        );
        assert_eq!(
            due(t(10, 2, 17), Some(t(10, 2, 16)), &[SLOT_0215], None),
            None
        );
    }

    #[test]
    fn a_slot_missed_while_asleep_runs_on_waking() {
        assert_eq!(
            due(t(10, 9, 0), Some(t(9, 2, 16)), &[SLOT_0215], None),
            Some(Due::Nightly)
        );
    }

    #[test]
    fn a_new_repo_does_not_run_for_yesterdays_slot() {
        assert_eq!(due(t(10, 9, 0), None, &[SLOT_0215], None), None);
        assert_eq!(
            due(t(10, 2, 30), None, &[SLOT_0215], None),
            Some(Due::Nightly)
        );
    }

    #[test]
    fn sunday_sweep_beats_the_nightly() {
        let sweep = Some((6, 3 * 60 + 15)); // Sunday 03:15
        assert_eq!(
            due(t(11, 3, 20), Some(t(10, 2, 16)), &[SLOT_0215], sweep),
            Some(Due::Sweep)
        );
        // Saturday: no sweep, the nightly as usual.
        assert_eq!(
            due(t(10, 3, 20), Some(t(9, 2, 16)), &[SLOT_0215], sweep),
            Some(Due::Nightly)
        );
    }

    #[test]
    fn no_slots_means_never() {
        assert_eq!(due(t(10, 2, 30), Some(t(1, 0, 0)), &[], None), None);
    }

    #[test]
    fn evening_window() {
        assert!(is_evening(t(10, 17, 0)));
        assert!(is_evening(t(10, 22, 59)));
        assert!(!is_evening(t(10, 23, 0)));
        assert!(!is_evening(t(10, 2, 15)));
    }
}
