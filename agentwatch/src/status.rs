//! A local worker's status file: newest first, one heading per step:
//!
//! ```text
//! # STEP 2 Nov 20:23 (check): NO CHANGE. HEAD = origin/main 213993d8, ...
//! ```
//!
//! The date has no year; the worker writes it in UK local time.

use chrono::{DateTime, Datelike, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Europe::London;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Step {
    /// When the step ran; midnight of its day when the heading gives no time.
    pub at: DateTime<Utc>,
    /// The heading gave a time, not just a day.
    pub timed: bool,
    /// What the step was: `check`, `work`, ...
    pub kind: String,
    pub summary: String,
}

/// Every step heading in the file, in file order (newest first). `now` gives
/// the year: a date that would be in the future this year is last year's.
pub fn parse(text: &str, now: DateTime<Utc>) -> Vec<Step> {
    text.lines().filter_map(|l| step(l, now)).collect()
}

fn step(line: &str, now: DateTime<Utc>) -> Option<Step> {
    let rest = line.strip_prefix("# STEP ")?;
    let (when, rest) = rest.split_once(" (")?;
    let (kind, summary) = rest.split_once("):")?;
    let when = when.trim();
    // Some headings give only the day (`3 Nov`): read those as its midnight.
    let timed = when.contains(':');
    let full = if timed {
        when.to_string()
    } else {
        format!("{when} 00:00")
    };
    let at = when_to_utc(&full, now)?;
    Some(Step {
        at,
        timed,
        kind: kind.trim().to_string(),
        summary: summary.trim().to_string(),
    })
}

fn when_to_utc(when: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let year = now.with_timezone(&London).year();
    let at = |y: i32| {
        NaiveDateTime::parse_from_str(&format!("{when} {y}"), "%d %b %H:%M %Y")
            .ok()
            .and_then(|n| London.from_local_datetime(&n).earliest())
            .map(|t| t.with_timezone(&Utc))
    };
    let this_year = at(year)?;
    // A day's grace for clocks a little apart.
    if this_year > now + chrono::Duration::days(1) {
        at(year - 1)
    } else {
        Some(this_year)
    }
}

/// The lines of the newest step's summary that say what it waits on from Jon.
pub fn waiting_on_jon(step: &Step) -> Vec<String> {
    step.summary
        .split(['.', ';'])
        .map(str::trim)
        .filter(|s| s.to_lowercase().contains("waiting on jon"))
        .map(str::to_string)
        .collect()
}

/// When a tick ran, from its log's name (`20261103-0244.log`, UK time).
pub fn tick_time(file_name: &str) -> Option<DateTime<Utc>> {
    let stem = file_name.trim().trim_end_matches(".log");
    let n = NaiveDateTime::parse_from_str(stem, "%Y%m%d-%H%M").ok()?;
    London
        .from_local_datetime(&n)
        .earliest()
        .map(|t| t.with_timezone(&Utc))
}

/// Whether a tick log's tail shows the tick failed: empty, or `claude` gave
/// up with an error instead of an answer.
pub fn tick_failed(log_tail: &str) -> bool {
    let t = log_tail.trim();
    t.is_empty()
        || t.starts_with("Error")
        || t.contains("API Error")
        || t.contains("Execution error")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn reads_the_fixture_newest_first() {
        let steps = parse(
            include_str!("../fixtures/status.md"),
            utc("2026-11-02T21:00:00Z"),
        );
        assert_eq!(steps.len(), 3);
        // November is GMT, so local time is UTC.
        assert_eq!(steps[0].at, utc("2026-11-02T20:23:00Z"));
        assert_eq!(steps[0].kind, "check");
        assert!(
            steps[0]
                .summary
                .starts_with("NO CHANGE. HEAD = origin/main 213993d8")
        );
        assert_eq!(steps[2].kind, "work");
        assert_eq!(steps[2].at, utc("2026-11-01T09:15:00Z"));
    }

    #[test]
    fn summer_times_are_bst() {
        let s = parse(
            "# STEP 1 Jul 09:00 (check): fine.",
            utc("2026-07-01T12:00:00Z"),
        );
        assert_eq!(s[0].at, utc("2026-07-01T08:00:00Z"));
    }

    #[test]
    fn a_date_ahead_of_now_is_last_year() {
        let s = parse(
            "# STEP 31 Dec 23:50 (check): fine.",
            utc("2027-01-01T00:10:00Z"),
        );
        assert_eq!(s[0].at, utc("2026-12-31T23:50:00Z"));
        let s = parse(
            "# STEP 20 Dec 10:00 (check): fine.",
            utc("2027-01-05T00:00:00Z"),
        );
        assert_eq!(s[0].at, utc("2026-12-20T10:00:00Z"));
    }

    #[test]
    fn skips_headings_that_are_not_steps() {
        let s = parse(
            "# Notes\n## STEP 2 Nov 10:00 (x): y\n# STEP soon (x): y",
            utc("2026-11-02T21:00:00Z"),
        );
        assert!(s.is_empty());
    }

    #[test]
    fn a_heading_with_only_the_day_reads_as_its_midnight() {
        let s = parse(
            "# STEP 3 Nov (check): NO CHANGE.",
            utc("2026-11-03T03:00:00Z"),
        );
        assert_eq!(s[0].at, utc("2026-11-03T00:00:00Z"));
        assert!(!s[0].timed);
        assert!(parse("# STEP 3 Nov 02:08 (check): x", utc("2026-11-03T03:00:00Z"))[0].timed);
    }

    #[test]
    fn reads_a_tick_time_from_its_log_name() {
        assert_eq!(
            tick_time("20261103-0244.log"),
            Some(utc("2026-11-03T02:44:00Z"))
        );
        assert_eq!(
            tick_time("20260701-0900.log"),
            Some(utc("2026-07-01T08:00:00Z"))
        );
        assert_eq!(tick_time("latest.log"), None);
    }

    #[test]
    fn a_tick_failed_when_its_log_is_empty_or_an_error() {
        assert!(!tick_failed(
            "No change since the last step, so I added a NO CHANGE entry."
        ));
        assert!(tick_failed(""));
        assert!(tick_failed("  \n"));
        assert!(tick_failed("Error: Reached max turns (40)"));
        assert!(tick_failed("API Error: 529 overloaded"));
    }

    #[test]
    fn finds_what_it_is_waiting_on_jon_for() {
        let steps = parse(
            include_str!("../fixtures/status.md"),
            utc("2026-11-02T21:00:00Z"),
        );
        assert_eq!(
            waiting_on_jon(&steps[0]),
            ["Waiting on Jon to say start the gated run"]
        );
        assert!(waiting_on_jon(&steps[1]).is_empty());
    }
}
