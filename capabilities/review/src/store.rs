//! mac1's review database, `~/.ai-mesh/reviews/reviews.db`: repos, runs,
//! findings and settings. The coordinator only ever sees snapshots of it.

use codereview::{Finding, Verdict};
use rusqlite::{Connection, OptionalExtension, params};
use shared::{ReviewCounts, ReviewFindingView, ReviewRepoSpec, ReviewRunView};
use std::path::Path;

pub struct Store {
    conn: Connection,
}

/// A repo row: what the dashboard set, plus mac1's own bookkeeping.
#[derive(Debug, Clone, PartialEq)]
pub struct RepoRow {
    pub spec: ReviewRepoSpec,
    pub last_reviewed_commit: Option<String>,
    pub sweep_cursor: Option<String>,
    /// Unix seconds of the last *scheduled* run.
    pub last_scheduled_at: Option<i64>,
}

/// Fields of a run that change while it goes.
#[derive(Debug, Clone, Default)]
pub struct RunUpdate {
    pub status: Option<String>,
    pub scope: Option<String>,
    pub tasks_total: Option<u32>,
    pub tasks_done: Option<u32>,
    pub counts: Option<ReviewCounts>,
    pub error: Option<String>,
    pub finished_at: Option<i64>,
    pub report_path: Option<String>,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS repos (
    name TEXT PRIMARY KEY,
    spec_json TEXT NOT NULL,
    last_reviewed_commit TEXT,
    sweep_cursor TEXT,
    last_scheduled_at INTEGER
);
CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS runs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    repo TEXT NOT NULL,
    kind TEXT NOT NULL,
    scope TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    finished_at INTEGER,
    tasks_total INTEGER NOT NULL DEFAULT 0,
    tasks_done INTEGER NOT NULL DEFAULT 0,
    counts_json TEXT NOT NULL DEFAULT '{}',
    error TEXT,
    report_path TEXT
);
CREATE TABLE IF NOT EXISTS findings (
    id TEXT PRIMARY KEY,
    run_id INTEGER NOT NULL,
    repo TEXT NOT NULL,
    path TEXT NOT NULL,
    line INTEGER NOT NULL,
    severity TEXT NOT NULL,
    title TEXT NOT NULL,
    quote TEXT NOT NULL,
    scenario TEXT NOT NULL,
    fix TEXT NOT NULL,
    verdict TEXT,
    checked_by TEXT,
    found_by_json TEXT NOT NULL DEFAULT '[]',
    status TEXT NOT NULL DEFAULT 'open',
    first_seen INTEGER NOT NULL
);
";

fn sev_str(f: &Finding) -> &'static str {
    match f.severity {
        codereview::Severity::High => "high",
        codereview::Severity::Medium => "medium",
        codereview::Severity::Low => "low",
    }
}

fn verdict_str(v: Option<Verdict>) -> Option<&'static str> {
    v.map(|v| match v {
        Verdict::Confirmed => "confirmed",
        Verdict::Rejected => "rejected",
        Verdict::Unsure => "unsure",
    })
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let conn = Connection::open(path).map_err(|e| e.to_string())?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self, String> {
        Self::init(Connection::open_in_memory().map_err(|e| e.to_string())?)
    }

    fn init(conn: Connection) -> Result<Self, String> {
        conn.execute_batch(SCHEMA).map_err(|e| e.to_string())?;
        Ok(Self { conn })
    }

    // ── settings ─────────────────────────────────────────────────────────────

    pub fn setting(&self, key: &str) -> Option<String> {
        self.conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .optional()
            .ok()
            .flatten()
    }

    pub fn set_setting(&self, key: &str, value: &str) {
        let _ = self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![key, value],
        );
    }

    // ── repos ────────────────────────────────────────────────────────────────

    pub fn repos(&self) -> Vec<RepoRow> {
        let mut stmt = match self.conn.prepare(
            "SELECT spec_json, last_reviewed_commit, sweep_cursor, last_scheduled_at
             FROM repos ORDER BY name",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<i64>>(3)?,
            ))
        })
        .map(|rows| {
            rows.filter_map(Result::ok)
                .filter_map(|(spec, commit, cursor, sched)| {
                    Some(RepoRow {
                        spec: serde_json::from_str(&spec).ok()?,
                        last_reviewed_commit: commit,
                        sweep_cursor: cursor,
                        last_scheduled_at: sched,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
    }

    pub fn repo(&self, name: &str) -> Option<RepoRow> {
        self.repos().into_iter().find(|r| r.spec.name == name)
    }

    /// Add a repo or replace its settings, keeping its review history.
    pub fn upsert_repo(&self, spec: &ReviewRepoSpec) {
        let json = serde_json::to_string(spec).unwrap_or_default();
        let _ = self.conn.execute(
            "INSERT INTO repos (name, spec_json) VALUES (?1, ?2)
             ON CONFLICT (name) DO UPDATE SET spec_json = excluded.spec_json",
            params![spec.name, json],
        );
    }

    pub fn remove_repo(&self, name: &str) -> bool {
        self.conn
            .execute("DELETE FROM repos WHERE name = ?1", [name])
            .map(|n| n > 0)
            .unwrap_or(false)
    }

    pub fn set_last_reviewed(&self, name: &str, commit: &str) {
        let _ = self.conn.execute(
            "UPDATE repos SET last_reviewed_commit = ?2 WHERE name = ?1",
            params![name, commit],
        );
    }

    pub fn set_sweep_cursor(&self, name: &str, folder: &str) {
        let _ = self.conn.execute(
            "UPDATE repos SET sweep_cursor = ?2 WHERE name = ?1",
            params![name, folder],
        );
    }

    pub fn set_last_scheduled(&self, name: &str, at: i64) {
        let _ = self.conn.execute(
            "UPDATE repos SET last_scheduled_at = ?2 WHERE name = ?1",
            params![name, at],
        );
    }

    // ── runs ─────────────────────────────────────────────────────────────────

    pub fn create_run(&self, repo: &str, kind: &str, scope: &str, status: &str, now: i64) -> i64 {
        let _ = self.conn.execute(
            "INSERT INTO runs (repo, kind, scope, status, started_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![repo, kind, scope, status, now],
        );
        self.conn.last_insert_rowid()
    }

    pub fn update_run(&self, id: i64, u: &RunUpdate) {
        let counts = u
            .counts
            .as_ref()
            .and_then(|c| serde_json::to_string(c).ok());
        let _ = self.conn.execute(
            "UPDATE runs SET
                status = COALESCE(?2, status),
                scope = COALESCE(?3, scope),
                tasks_total = COALESCE(?4, tasks_total),
                tasks_done = COALESCE(?5, tasks_done),
                counts_json = COALESCE(?6, counts_json),
                error = COALESCE(?7, error),
                finished_at = COALESCE(?8, finished_at),
                report_path = COALESCE(?9, report_path)
             WHERE id = ?1",
            params![
                id,
                u.status,
                u.scope,
                u.tasks_total,
                u.tasks_done,
                counts,
                u.error,
                u.finished_at,
                u.report_path
            ],
        );
    }

    pub fn runs(&self, limit: u32) -> Vec<ReviewRunView> {
        let mut stmt = match self.conn.prepare(
            "SELECT id, repo, kind, scope, status, started_at, finished_at, tasks_total,
                    tasks_done, counts_json, error
             FROM runs ORDER BY id DESC LIMIT ?1",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map([limit], |r| {
            Ok(ReviewRunView {
                id: r.get(0)?,
                repo: r.get(1)?,
                kind: r.get(2)?,
                scope: r.get(3)?,
                status: r.get(4)?,
                started_at: r.get::<_, i64>(5)? as u64,
                finished_at: r.get::<_, Option<i64>>(6)?.map(|v| v as u64),
                tasks_total: r.get(7)?,
                tasks_done: r.get(8)?,
                running: Vec::new(),
                counts: serde_json::from_str(&r.get::<_, String>(9)?).unwrap_or_default(),
                error: r.get(10)?,
            })
        })
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default()
    }

    pub fn report_path(&self, run_id: i64) -> Option<String> {
        self.conn
            .query_row(
                "SELECT report_path FROM runs WHERE id = ?1",
                [run_id],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten()
            .flatten()
    }

    /// Runs left "queued" or "running" by a restart: mark them failed and
    /// return `(repo, kind)` so they can be queued again.
    pub fn take_interrupted(&self, now: i64) -> Vec<(String, String)> {
        let mut out = Vec::new();
        if let Ok(mut stmt) = self
            .conn
            .prepare("SELECT repo, kind FROM runs WHERE status IN ('queued', 'running')")
            && let Ok(rows) = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        {
            out = rows.filter_map(Result::ok).collect();
        }
        let _ = self.conn.execute(
            "UPDATE runs SET status = 'failed', error = 'interrupted: mac1 restarted', finished_at = ?1
             WHERE status IN ('queued', 'running')",
            [now],
        );
        out
    }

    // ── findings ─────────────────────────────────────────────────────────────

    /// Save a finding. The id is stable across runs (repo, path, quote), so a
    /// finding dismissed once stays dismissed when a later run reports it again.
    pub fn upsert_finding(&self, run_id: i64, f: &Finding, now: i64) {
        let found_by = serde_json::to_string(&f.found_by).unwrap_or_else(|_| "[]".into());
        let _ = self.conn.execute(
            "INSERT INTO findings (id, run_id, repo, path, line, severity, title, quote, scenario,
                                   fix, verdict, checked_by, found_by_json, status, first_seen)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, 'open', ?14)
             ON CONFLICT (id) DO UPDATE SET
                run_id = excluded.run_id, line = excluded.line, severity = excluded.severity,
                title = excluded.title, scenario = excluded.scenario, fix = excluded.fix,
                verdict = excluded.verdict, checked_by = excluded.checked_by,
                found_by_json = excluded.found_by_json",
            params![
                f.id,
                run_id,
                f.repo,
                f.path,
                f.line,
                sev_str(f),
                f.title,
                f.quote,
                f.scenario,
                f.fix,
                verdict_str(f.verdict),
                f.checked_by,
                found_by,
                now
            ],
        );
    }

    pub fn set_finding_status(&self, id: &str, status: &str) -> bool {
        if !matches!(status, "open" | "dismissed" | "fixed") {
            return false;
        }
        self.conn
            .execute(
                "UPDATE findings SET status = ?2 WHERE id = ?1",
                params![id, status],
            )
            .map(|n| n > 0)
            .unwrap_or(false)
    }

    /// Open findings, worst first, newest run first, up to `limit`.
    pub fn open_findings(&self, limit: u32) -> Vec<ReviewFindingView> {
        let mut stmt = match self.conn.prepare(
            "SELECT id, run_id, repo, path, line, severity, title, quote, scenario, fix, verdict,
                    checked_by, found_by_json, status, first_seen
             FROM findings WHERE status = 'open' AND COALESCE(verdict, '') != 'rejected'
             ORDER BY CASE severity WHEN 'high' THEN 0 WHEN 'medium' THEN 1 ELSE 2 END,
                      run_id DESC, repo, path, line
             LIMIT ?1",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map([limit], |r| {
            Ok(ReviewFindingView {
                id: r.get(0)?,
                run_id: r.get(1)?,
                repo: r.get(2)?,
                path: r.get(3)?,
                line: r.get(4)?,
                severity: r.get(5)?,
                title: r.get(6)?,
                quote: r.get(7)?,
                scenario: r.get(8)?,
                fix: r.get(9)?,
                verdict: r.get(10)?,
                checked_by: r.get(11)?,
                found_by: serde_json::from_str(&r.get::<_, String>(12)?).unwrap_or_default(),
                status: r.get(13)?,
                first_seen: r.get::<_, i64>(14)? as u64,
            })
        })
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codereview::Severity;

    fn spec(name: &str) -> ReviewRepoSpec {
        ReviewRepoSpec {
            name: name.into(),
            url: format!("https://github.com/jon-comley/{name}"),
            branch: "main".into(),
            timeslots: vec![135],
            sweep_day: Some(6),
            sweep_slot: Some(195),
            enabled: true,
            aliases: vec![],
        }
    }

    fn finding(quote: &str, verdict: Option<Verdict>) -> Finding {
        Finding {
            id: codereview::dedupe::finding_key("dashboard", "src/a.ts", quote),
            repo: "dashboard".into(),
            path: "src/a.ts".into(),
            line: 3,
            severity: Severity::High,
            title: "t".into(),
            quote: quote.into(),
            scenario: "s".into(),
            fix: "f".into(),
            found_by: vec!["m@mac1".into()],
            verdict,
            verdict_reason: None,
            checked_by: Some("q@beelink1".into()),
        }
    }

    #[test]
    fn repos_keep_their_history_when_settings_change() {
        let s = Store::open_in_memory().unwrap();
        s.upsert_repo(&spec("dashboard"));
        s.set_last_reviewed("dashboard", "abc123");
        let mut changed = spec("dashboard");
        changed.timeslots = vec![60];
        s.upsert_repo(&changed);
        let row = s.repo("dashboard").unwrap();
        assert_eq!(row.spec.timeslots, vec![60]);
        assert_eq!(row.last_reviewed_commit.as_deref(), Some("abc123"));
        assert!(s.remove_repo("dashboard"));
        assert!(s.repos().is_empty());
    }

    #[test]
    fn runs_update_partially_and_list_newest_first() {
        let s = Store::open_in_memory().unwrap();
        let a = s.create_run("guv", "nightly", "", "running", 10);
        let b = s.create_run("dashboard", "manual", "x", "queued", 20);
        s.update_run(
            a,
            &RunUpdate {
                status: Some("done".into()),
                tasks_done: Some(4),
                counts: Some(ReviewCounts {
                    high: 1,
                    ..Default::default()
                }),
                report_path: Some("/r.md".into()),
                ..Default::default()
            },
        );
        let runs = s.runs(10);
        assert_eq!(runs[0].id, b);
        assert_eq!(runs[1].status, "done");
        assert_eq!(runs[1].tasks_done, 4);
        assert_eq!(runs[1].counts.high, 1);
        assert_eq!(s.report_path(a).as_deref(), Some("/r.md"));
        assert_eq!(s.report_path(b), None);
    }

    #[test]
    fn interrupted_runs_are_failed_and_returned() {
        let s = Store::open_in_memory().unwrap();
        s.create_run("guv", "nightly", "", "running", 1);
        s.create_run("guv", "sweep", "", "done", 1);
        assert_eq!(
            s.take_interrupted(5),
            vec![("guv".to_string(), "nightly".to_string())]
        );
        assert!(s.take_interrupted(6).is_empty());
    }

    #[test]
    fn a_dismissed_finding_stays_dismissed_when_reported_again() {
        let s = Store::open_in_memory().unwrap();
        let f = finding("const agreed = pricing?.amount;", Some(Verdict::Confirmed));
        s.upsert_finding(1, &f, 100);
        assert_eq!(s.open_findings(10).len(), 1);
        assert!(s.set_finding_status(&f.id, "dismissed"));
        s.upsert_finding(2, &f, 200);
        assert!(s.open_findings(10).is_empty());
        assert!(!s.set_finding_status(&f.id, "deleted"));
    }

    #[test]
    fn rejected_findings_are_not_listed() {
        let s = Store::open_in_memory().unwrap();
        s.upsert_finding(1, &finding("aaaa bbbb", Some(Verdict::Rejected)), 1);
        s.upsert_finding(1, &finding("cccc dddd", None), 1);
        let open = s.open_findings(10);
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].verdict, None);
        assert_eq!(open[0].found_by, vec!["m@mac1"]);
    }

    #[test]
    fn settings_roundtrip() {
        let s = Store::open_in_memory().unwrap();
        assert_eq!(s.setting("ntfy_topic_url"), None);
        s.set_setting("ntfy_topic_url", "https://ntfy.sh/x");
        s.set_setting("ntfy_topic_url", "https://ntfy.sh/y");
        assert_eq!(
            s.setting("ntfy_topic_url").as_deref(),
            Some("https://ntfy.sh/y")
        );
    }
}
