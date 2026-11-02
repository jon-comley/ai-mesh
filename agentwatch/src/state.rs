//! Each worker's state light, from what the server found and the clock.

use std::time::Duration;

use chrono::{DateTime, Utc};
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
}

/// How long after Run now a run counts as broken if nothing has landed.
pub const RUN_TIMEOUT: Duration = Duration::from_secs(2 * 3600);

/// One commit on the agent repo's `main`.
#[derive(Debug, Clone, PartialEq)]
pub struct Commit {
    pub at: DateTime<Utc>,
    pub subject: String,
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
/// one of those after the last log line means a run that never finished.
/// Jon's edits to the hub use other messages and do not count.
pub fn cloud(
    f: &CloudFacts,
    prefix: &str,
    every: Duration,
    grace: Duration,
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
        .any(|c| c.subject.starts_with(prefix))
    {
        return State::Broken;
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

/// When the next run is due, if it has run before.
pub fn next_due(last_run: Option<DateTime<Utc>>, every: Duration) -> Option<DateTime<Utc>> {
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
        }
    }

    #[test]
    fn ok_until_every_plus_grace_then_late() {
        let f = facts(Some("2026-11-03T07:40:00Z"));
        assert_eq!(
            cloud(&f, "Desk:", DAY, SIX_H, t("2026-11-04T13:40:00Z")),
            State::Ok
        );
        assert_eq!(
            cloud(&f, "Desk:", DAY, SIX_H, t("2026-11-04T13:41:00Z")),
            State::Late
        );
    }

    #[test]
    fn never_run_is_new() {
        assert_eq!(
            cloud(&facts(None), "Desk:", DAY, SIX_H, t("2026-11-03T00:00:00Z")),
            State::New
        );
    }

    #[test]
    fn its_own_commit_after_the_last_log_line_is_broken() {
        let mut f = facts(Some("2026-11-03T07:40:00Z"));
        f.commits_since = vec![commit("2026-11-04T07:35:00Z", "Desk: patches for guv")];
        assert_eq!(
            cloud(&f, "Desk:", DAY, SIX_H, t("2026-11-04T08:00:00Z")),
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
            cloud(&f, "Desk:", DAY, SIX_H, t("2026-11-03T12:00:00Z")),
            State::Ok
        );
    }

    #[test]
    fn unreachable_beats_everything() {
        let mut f = facts(Some("2026-11-03T07:40:00Z"));
        f.reachable = false;
        assert_eq!(
            cloud(&f, "Desk:", DAY, SIX_H, t("2026-11-03T08:00:00Z")),
            State::Unreachable
        );
    }

    #[test]
    fn run_now_is_running_until_its_log_line_lands_or_it_times_out() {
        let mut f = facts(Some("2026-11-03T07:40:00Z"));
        f.run_started = Some(t("2026-11-03T12:00:00Z"));
        assert_eq!(
            cloud(&f, "Desk:", DAY, SIX_H, t("2026-11-03T12:30:00Z")),
            State::Running
        );
        assert_eq!(
            cloud(&f, "Desk:", DAY, SIX_H, t("2026-11-03T14:00:01Z")),
            State::Broken
        );
        f.last_run = Some(t("2026-11-03T12:20:00Z"));
        assert_eq!(
            cloud(&f, "Desk:", DAY, SIX_H, t("2026-11-03T12:30:00Z")),
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
    fn next_due_is_last_run_plus_every() {
        assert_eq!(
            next_due(Some(t("2026-11-03T10:23:00Z")), 7 * DAY),
            Some(t("2026-11-10T10:23:00Z"))
        );
        assert_eq!(next_due(None, DAY), None);
    }
}
