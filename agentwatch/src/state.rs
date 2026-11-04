//! Each worker's state light, from what the server found and the clock.

use std::time::Duration;

use chrono::{DateTime, Datelike, Duration as Span, NaiveTime, TimeZone, Utc, Weekday};
use chrono_tz::Europe::London;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// Ran within `every + grace`.
    Ok,
    /// No run within `every + grace`. For a cloud routine this is the only sign
    /// a run failed: one that breaks before it pushes leaves nothing to see.
    Late,
    /// It started and did not finish: committed without a log line, a tick
    /// that ended in an error, or Run now with no result after [`RUN_TIMEOUT`].
    Broken,
    /// Its repo or host could not be read.
    Unreachable,
    /// Run now was pressed and the result has not landed yet.
    Running,
    /// A local worker whose pause file exists.
    Paused,
    /// No run yet.
    New,
    /// The page has not read it yet since it started.
    Checking,
}

/// How long after Run now a run counts as broken if nothing has landed.
pub const RUN_TIMEOUT: Duration = Duration::from_secs(2 * 3600);

/// One commit on the agent repo's `main`.
#[derive(Debug, Clone, PartialEq)]
pub struct Commit {
    pub at: DateTime<Utc>,
    pub subject: String,
    /// The paths it changed.
    pub files: Vec<String>,
}

impl Commit {
    /// Only the hub, `conversations/`, the brief or STATUS.md: a chat, a hand
    /// edit or a check step that found nothing, which also commit under the
    /// agent's own prefix. A run always writes more.
    fn hub_or_chat_only(&self, hub: &str) -> bool {
        !self.files.is_empty()
            && self.files.iter().all(|f| {
                f == hub || f == "agent-brief.md" || f == "STATUS.md" || f.starts_with("conversations/")
            })
    }
}

/// What the server found for a cloud agent.
#[derive(Debug, Clone, PartialEq)]
pub struct CloudFacts {
    pub reachable: bool,
    /// When the commit that added the newest log line landed.
    pub last_run: Option<DateTime<Utc>>,
    /// Commits after that one, oldest first.
    pub commits_since: Vec<Commit>,
    /// When Run now was last pressed.
    pub run_started: Option<DateTime<Utc>>,
}

/// A routine's scheduled time in UK time, daily or on one weekday.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Due {
    pub weekday: Option<Weekday>,
    pub time: NaiveTime,
}

impl Due {
    /// `07:30` or `tue 10:23`.
    pub fn parse(text: &str) -> Option<Due> {
        let mut parts = text.split_whitespace();
        let (day, time) = match (parts.next()?, parts.next(), parts.next()) {
            (t, None, None) => (None, t),
            (d, Some(t), None) => (Some(d.parse::<Weekday>().ok()?), t),
            _ => return None,
        };
        Some(Due {
            weekday: day,
            time: NaiveTime::parse_from_str(time, "%H:%M").ok()?,
        })
    }

    /// The latest scheduled time at or before `now`.
    pub fn last_before(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        (0..8).find_map(|back| self.on(now, -back).filter(|&at| at <= now))
    }

    /// The first scheduled time after `now`.
    pub fn next_after(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        (0..8).find_map(|ahead| self.on(now, ahead).filter(|&at| at > now))
    }

    /// The scheduled time on the UK day `offset` days from `now`'s, if it runs that day.
    fn on(&self, now: DateTime<Utc>, offset: i64) -> Option<DateTime<Utc>> {
        let day = now.with_timezone(&London).date_naive() + Span::days(offset);
        if self.weekday.is_some_and(|w| day.weekday() != w) {
            return None;
        }
        London
            .from_local_datetime(&day.and_time(self.time))
            .earliest()
            .map(|at| at.with_timezone(&Utc))
    }
}

/// What the server found for a local worker.
#[derive(Debug, Clone, PartialEq)]
pub struct LocalFacts {
    pub reachable: bool,
    pub paused: bool,
    /// The newest step in its status file.
    pub last_run: Option<DateTime<Utc>>,
    /// The latest tick log ended in an error.
    pub last_tick_failed: bool,
    pub run_started: Option<DateTime<Utc>>,
}

/// A cloud agent's state. `prefix` is how its own commits start (`Desk:`):
/// one of those after the last log line means a run that never finished,
/// unless it only touched the hub (`hub`) or `conversations/`.
pub fn cloud(
    f: &CloudFacts,
    prefix: &str,
    hub: &str,
    every: Duration,
    grace: Duration,
    due: Option<Due>,
    now: DateTime<Utc>,
) -> State {
    if !f.reachable {
        return State::Unreachable;
    }
    if let Some(s) = running(f.run_started, f.last_run, now) {
        return s;
    }
    if f.commits_since
        .iter()
        .any(|c| c.subject.starts_with(prefix) && !c.hub_or_chat_only(hub))
    {
        return State::Broken;
    }
    if let Some(fire) = due.and_then(|d| d.last_before(now)) {
        let missed = f.last_run.is_none_or(|l| l < fire);
        if missed && (now - fire).to_std().unwrap_or_default() > grace {
            return State::Late;
        }
    }
    on_time(f.last_run, every, grace, now)
}

pub fn local(f: &LocalFacts, every: Duration, grace: Duration, now: DateTime<Utc>) -> State {
    if !f.reachable {
        return State::Unreachable;
    }
    if f.paused {
        return State::Paused;
    }
    if let Some(s) = running(f.run_started, f.last_run, now) {
        return s;
    }
    if f.last_tick_failed {
        return State::Broken;
    }
    on_time(f.last_run, every, grace, now)
}

/// When the next run is due: the next scheduled time if it has one, otherwise
/// `every` after the last run.
pub fn next_due(
    last_run: Option<DateTime<Utc>>,
    every: Duration,
    due: Option<Due>,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    if let Some(d) = due {
        return d.next_after(now);
    }
    Some(last_run? + chrono::Duration::from_std(every).ok()?)
}

fn running(
    started: Option<DateTime<Utc>>,
    last_run: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Option<State> {
    let started = started?;
    if last_run.is_some_and(|l| l >= started) {
        return None;
    }
    let waited = (now - started).to_std().unwrap_or_default();
    Some(if waited < RUN_TIMEOUT {
        State::Running
    } else {
        State::Broken
    })
}

fn on_time(
    last_run: Option<DateTime<Utc>>,
    every: Duration,
    grace: Duration,
    now: DateTime<Utc>,
) -> State {
    let Some(last) = last_run else {
        return State::New;
    };
    let since = (now - last).to_std().unwrap_or_default();
    if since <= every + grace {
        State::Ok
    } else {
        State::Late
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: Duration = Duration::from_secs(86_400);
    const SIX_H: Duration = Duration::from_secs(6 * 3600);

    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn facts(last: Option<&str>) -> CloudFacts {
        CloudFacts {
            reachable: true,
            last_run: last.map(t),
            commits_since: vec![],
            run_started: None,
        }
    }

    fn commit(at: &str, subject: &str) -> Commit {
        Commit {
            at: t(at),
            subject: subject.into(),
            files: vec!["reports/2026-11-04.md".into()],
        }
    }

    #[test]
    fn ok_until_every_plus_grace_then_late() {
        let f = facts(Some("2026-11-03T07:40:00Z"));
        assert_eq!(
            cloud(
                &f,
                "Desk:",
                "HUB.md",
                DAY,
                SIX_H,
                None,
                t("2026-11-04T13:40:00Z")
            ),
            State::Ok
        );
        assert_eq!(
            cloud(
                &f,
                "Desk:",
                "HUB.md",
                DAY,
                SIX_H,
                None,
                t("2026-11-04T13:41:00Z")
            ),
            State::Late
        );
    }

    #[test]
    fn never_run_is_new() {
        assert_eq!(
            cloud(
                &facts(None),
                "Desk:",
                "HUB.md",
                DAY,
                SIX_H,
                None,
                t("2026-11-03T00:00:00Z")
            ),
            State::New
        );
    }

    #[test]
    fn its_own_commit_after_the_last_log_line_is_broken() {
        let mut f = facts(Some("2026-11-03T07:40:00Z"));
        f.commits_since = vec![commit("2026-11-04T07:35:00Z", "Desk: patches for guv")];
        assert_eq!(
            cloud(
                &f,
                "Desk:",
                "HUB.md",
                DAY,
                SIX_H,
                None,
                t("2026-11-04T08:00:00Z")
            ),
            State::Broken
        );
    }

    #[test]
    fn jons_hub_edits_after_the_last_log_line_are_not_broken() {
        let mut f = facts(Some("2026-11-03T07:40:00Z"));
        f.commits_since = vec![
            commit("2026-11-03T09:00:00Z", "Update HUB.md"),
            commit("2026-11-03T10:00:00Z", "Hub: reply from Jon"),
        ];
        assert_eq!(
            cloud(
                &f,
                "Desk:",
                "HUB.md",
                DAY,
                SIX_H,
                None,
                t("2026-11-03T12:00:00Z")
            ),
            State::Ok
        );
    }

    #[test]
    fn its_own_prefix_on_a_hub_or_chat_edit_is_not_broken() {
        let mut f = facts(Some("2026-11-03T19:57:00Z"));
        let mut edit = commit("2026-11-03T20:14:00Z", "Desk: pension letter sent");
        edit.files = vec!["HUB.md".into(), "conversations/2026-11-03.md".into()];
        f.commits_since = vec![edit];
        assert_eq!(
            cloud(
                &f,
                "Desk:",
                "HUB.md",
                DAY,
                SIX_H,
                None,
                t("2026-11-03T21:00:00Z")
            ),
            State::Ok
        );
        let mut doc = commit("2026-11-03T20:30:00Z", "Desk: half a run");
        doc.files = vec!["HUB.md".into(), "IT-PRO-002.md".into()];
        f.commits_since.push(doc);
        assert_eq!(
            cloud(
                &f,
                "Desk:",
                "HUB.md",
                DAY,
                SIX_H,
                None,
                t("2026-11-03T21:00:00Z")
            ),
            State::Broken
        );
    }

    #[test]
    fn unreachable_beats_everything() {
        let mut f = facts(Some("2026-11-03T07:40:00Z"));
        f.reachable = false;
        assert_eq!(
            cloud(
                &f,
                "Desk:",
                "HUB.md",
                DAY,
                SIX_H,
                None,
                t("2026-11-03T08:00:00Z")
            ),
            State::Unreachable
        );
    }

    #[test]
    fn run_now_is_running_until_its_log_line_lands_or_it_times_out() {
        let mut f = facts(Some("2026-11-03T07:40:00Z"));
        f.run_started = Some(t("2026-11-03T12:00:00Z"));
        assert_eq!(
            cloud(
                &f,
                "Desk:",
                "HUB.md",
                DAY,
                SIX_H,
                None,
                t("2026-11-03T12:30:00Z")
            ),
            State::Running
        );
        assert_eq!(
            cloud(
                &f,
                "Desk:",
                "HUB.md",
                DAY,
                SIX_H,
                None,
                t("2026-11-03T14:00:01Z")
            ),
            State::Broken
        );
        f.last_run = Some(t("2026-11-03T12:20:00Z"));
        assert_eq!(
            cloud(
                &f,
                "Desk:",
                "HUB.md",
                DAY,
                SIX_H,
                None,
                t("2026-11-03T12:30:00Z")
            ),
            State::Ok
        );
    }

    #[test]
    fn local_worker_states() {
        let twenty = Duration::from_secs(20 * 60);
        let ten = Duration::from_secs(10 * 60);
        let mut f = LocalFacts {
            reachable: true,
            paused: false,
            last_run: Some(t("2026-11-02T20:23:00Z")),
            last_tick_failed: false,
            run_started: None,
        };
        assert_eq!(local(&f, twenty, ten, t("2026-11-02T20:50:00Z")), State::Ok);
        assert_eq!(
            local(&f, twenty, ten, t("2026-11-02T20:54:00Z")),
            State::Late
        );
        f.last_tick_failed = true;
        assert_eq!(
            local(&f, twenty, ten, t("2026-11-02T20:30:00Z")),
            State::Broken
        );
        f.paused = true;
        assert_eq!(
            local(&f, twenty, ten, t("2026-11-03T20:30:00Z")),
            State::Paused,
            "paused is never late"
        );
        f.reachable = false;
        assert_eq!(
            local(&f, twenty, ten, t("2026-11-02T20:30:00Z")),
            State::Unreachable
        );
    }

    #[test]
    fn a_missed_scheduled_run_is_late_after_grace_that_day() {
        let three_h = Duration::from_secs(3 * 3600);
        let due = Due::parse("07:30");
        // The last run was a manual one the evening before. November is GMT, so 07:30 is 07:30Z.
        let f = facts(Some("2026-11-02T22:15:00Z"));
        assert_eq!(
            cloud(
                &f,
                "Desk:",
                "HUB.md",
                DAY,
                three_h,
                due,
                t("2026-11-03T10:30:00Z")
            ),
            State::Ok
        );
        assert_eq!(
            cloud(
                &f,
                "Desk:",
                "HUB.md",
                DAY,
                three_h,
                due,
                t("2026-11-03T10:31:00Z")
            ),
            State::Late
        );
        let f = facts(Some("2026-11-03T07:40:00Z"));
        assert_eq!(
            cloud(
                &f,
                "Desk:",
                "HUB.md",
                DAY,
                three_h,
                due,
                t("2026-11-03T20:00:00Z")
            ),
            State::Ok
        );
    }

    #[test]
    fn weekly_due_finds_the_last_tuesday_in_uk_time() {
        let due = Due::parse("tue 10:23").unwrap();
        assert_eq!(
            due.last_before(t("2026-11-05T12:00:00Z")),
            Some(t("2026-11-03T10:23:00Z"))
        );
        assert_eq!(
            due.last_before(t("2026-11-03T10:22:00Z")),
            Some(t("2026-10-27T10:23:00Z"))
        );
        // British Summer Time: 10:23 in London is 09:23Z.
        assert_eq!(
            due.last_before(t("2026-07-08T00:00:00Z")),
            Some(t("2026-07-07T09:23:00Z"))
        );
        assert_eq!(Due::parse("07:30").map(|d| d.weekday), Some(None));
        assert!(Due::parse("tuesday-ish").is_none());
        assert!(Due::parse("25:00").is_none());
    }

    #[test]
    fn next_due_is_last_run_plus_every() {
        let now = t("2026-11-03T12:00:00Z");
        assert_eq!(
            next_due(Some(t("2026-11-03T10:23:00Z")), 7 * DAY, None, now),
            Some(t("2026-11-10T10:23:00Z"))
        );
        assert_eq!(next_due(None, DAY, None, now), None);
        assert_eq!(
            next_due(None, DAY, Due::parse("07:30"), now),
            Some(t("2026-11-04T07:30:00Z"))
        );
        // Across the March clock change: 07:30 London on 29 March 2026 is 06:30Z.
        assert_eq!(
            next_due(None, DAY, Due::parse("07:30"), t("2026-03-28T12:00:00Z")),
            Some(t("2026-03-29T06:30:00Z"))
        );
    }

    #[test]
    fn a_brief_or_status_edit_is_not_an_unfinished_run() {
        let c = Commit {
            at: Utc::now(),
            subject: "Desk: Haiku check first".into(),
            files: vec!["agent-brief.md".into(), "STATUS.md".into()],
        };
        assert!(c.hub_or_chat_only("HUB.md"));
        let run = Commit { at: Utc::now(), subject: "Desk: run".into(), files: vec!["STATUS.md".into(), "reports/x.md".into()] };
        assert!(!run.hub_or_chat_only("HUB.md"));
    }
}
