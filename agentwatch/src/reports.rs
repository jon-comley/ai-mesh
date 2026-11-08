//! A worker that leaves reports rather than a status file: the code reviews on
//! mac1 (`~/.ai-mesh/reviews/reports/<repo>/<date>-run<n>.md`). The page reads
//! the newest reports' headers:
//!
//! ```text
//! # Code review: dashboard
//!
//! - **Scope:** Nightly: 18 new commits (1fc3066..d9d5364)
//! - **Finished:** 2026-11-02 05:14
//! - **Confirmed:** 1 high, 1 medium, 0 low. **Not confirmed:** 0. ...
//! ```

use chrono::{DateTime, Utc};
use serde::Serialize;

/// The marker the server prints before each report's header over ssh.
pub const FILE_MARKER: &str = "@@AGENTWATCH-FILE@@";

/// Before the open findings: `repo|severity|count` rows from the reviews
/// database, the reports folder's sibling `reviews.db`.
pub const OPEN_MARKER: &str = "@@AGENTWATCH-OPEN@@";
/// After them, printed only when the query ran, so an empty list means none
/// are open rather than no sqlite3 on the host.
pub const OPEN_END: &str = "@@AGENTWATCH-OPEN-END@@";

/// The query behind [`OPEN_MARKER`]: confirmed high and medium findings
/// nobody has marked fixed or dismissed yet.
pub const OPEN_QUERY: &str = "select repo, severity, count(*) from findings \
     where status = 'open' and verdict = 'confirmed' and severity in ('high', 'medium') \
     group by repo, severity order by repo";

/// One repo's open findings: (repo, high, medium), by repo. None when the
/// query didn't run, so the page falls back to the report headers.
pub fn parse_open(text: &str) -> Option<Vec<(String, usize, usize)>> {
    let (rows, _) = text.split_once(OPEN_END)?;
    let mut out: Vec<(String, usize, usize)> = Vec::new();
    for line in rows.lines() {
        let mut parts = line.trim().split('|');
        let (Some(repo), Some(level), Some(n)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let Ok(n) = n.parse::<usize>() else { continue };
        if out.last().is_none_or(|(r, _, _)| r != repo) {
            out.push((repo.to_string(), 0, 0));
        }
        let row = out.last_mut().expect("pushed above");
        match level {
            "high" => row.1 += n,
            "medium" => row.2 += n,
            _ => {}
        }
    }
    Some(out)
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Report {
    /// Full path on the host.
    pub path: String,
    pub at: DateTime<Utc>,
    pub repo: String,
    pub scope: Option<String>,
    /// "1 high, 1 medium, 0 low"
    pub confirmed: Option<String>,
    pub high: usize,
    pub medium: usize,
}

impl Report {
    pub fn line(&self) -> String {
        let confirmed = self.confirmed.as_deref().unwrap_or("no findings line");
        format!("{}: {confirmed}", self.repo)
    }
}

/// Reads what the server's ssh script printed: for each report, newest first,
/// `@@AGENTWATCH-FILE@@ <unix seconds> <path>` then the report's first lines.
pub fn parse_listing(text: &str) -> Vec<Report> {
    let mut out = Vec::new();
    for chunk in text.split(FILE_MARKER).skip(1) {
        let (first, body) = chunk.split_once('\n').unwrap_or((chunk, ""));
        let mut parts = first.trim().splitn(2, ' ');
        let (Some(secs), Some(path)) = (parts.next(), parts.next()) else {
            continue;
        };
        let Some(at) = secs
            .parse::<i64>()
            .ok()
            .and_then(|s| DateTime::from_timestamp(s, 0))
        else {
            continue;
        };
        out.push(header(path.trim(), at, body));
    }
    out
}

fn header(path: &str, at: DateTime<Utc>, body: &str) -> Report {
    let folder = path.rsplit('/').nth(1).unwrap_or("").to_string();
    let mut r = Report {
        path: path.to_string(),
        at,
        repo: folder,
        scope: None,
        confirmed: None,
        high: 0,
        medium: 0,
    };
    for line in body.lines() {
        let line = line.trim();
        if let Some(repo) = line.strip_prefix("# Code review:") {
            r.repo = repo.trim().to_string();
        } else if let Some(scope) = line.strip_prefix("- **Scope:**") {
            r.scope = Some(scope.trim().to_string());
        } else if let Some(c) = line.strip_prefix("- **Confirmed:**") {
            let c = c
                .split("**")
                .next()
                .unwrap_or("")
                .trim()
                .trim_end_matches('.');
            r.high = count(c, "high");
            r.medium = count(c, "medium");
            r.confirmed = Some(c.to_string());
        }
    }
    r
}

fn count(text: &str, level: &str) -> usize {
    text.split(',')
        .map(str::trim)
        .find_map(|part| part.strip_suffix(level).and_then(|n| n.trim().parse().ok()))
        .unwrap_or(0)
}

/// The newest report for each repo, newest first.
pub fn latest_per_repo(reports: &[Report]) -> Vec<&Report> {
    let mut seen = std::collections::HashSet::new();
    reports
        .iter()
        .filter(|r| seen.insert(r.repo.clone()))
        .collect()
}

/// A path the page may read back over ssh: one of the listed reports, with
/// nothing in it a shell would act on.
pub fn safe_path(path: &str) -> bool {
    !path.contains("..")
        && path.ends_with(".md")
        && path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "~._/-".contains(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LISTING: &str = "@@AGENTWATCH-FILE@@ 1793596440 /Users/jonno/.ai-mesh/reviews/reports/dashboard/2026-11-02-run201.md
# Code review: dashboard

- **Scope:** Nightly: 18 new commits (1fc3066..d9d5364)
- **Finished:** 2026-11-02 05:14
- **Confirmed:** 1 high, 1 medium, 0 low. **Not confirmed:** 0. **Thrown out:** 7 rejected by the checker.
@@AGENTWATCH-FILE@@ 1793593620 /Users/jonno/.ai-mesh/reviews/reports/guv/2026-11-02-run200.md
# Code review: guv

- **Confirmed:** 0 high, 3 medium, 2 low.
@@AGENTWATCH-FILE@@ 1793510000 /Users/jonno/.ai-mesh/reviews/reports/dashboard/2026-11-01-run192.md
# Code review: dashboard
- **Confirmed:** 2 high, 0 medium, 0 low.
@@AGENTWATCH-FILE@@ not-a-time /x.md
";

    #[test]
    fn reads_each_report_header() {
        let r = parse_listing(LISTING);
        assert_eq!(r.len(), 3, "the line with no time is skipped");
        assert_eq!(r[0].repo, "dashboard");
        assert_eq!(
            r[0].scope.as_deref(),
            Some("Nightly: 18 new commits (1fc3066..d9d5364)")
        );
        assert_eq!(r[0].confirmed.as_deref(), Some("1 high, 1 medium, 0 low"));
        assert_eq!((r[0].high, r[0].medium), (1, 1));
        assert_eq!(r[0].at, DateTime::from_timestamp(1793596440, 0).unwrap());
        assert_eq!(r[1].line(), "guv: 0 high, 3 medium, 2 low");
        assert_eq!((r[1].high, r[1].medium), (0, 3));
    }

    #[test]
    fn keeps_only_the_newest_per_repo() {
        let r = parse_listing(LISTING);
        let latest: Vec<_> = latest_per_repo(&r)
            .iter()
            .map(|r| (r.repo.as_str(), r.high))
            .collect();
        assert_eq!(latest, [("dashboard", 1), ("guv", 0)]);
    }

    #[test]
    fn a_report_without_a_header_takes_its_folder_name() {
        let r = parse_listing("@@AGENTWATCH-FILE@@ 1793596440 /a/reports/admin/x.md\nno header\n");
        assert_eq!(r[0].repo, "admin");
        assert_eq!(r[0].line(), "admin: no findings line");
    }

    #[test]
    fn reads_open_findings_per_repo() {
        let text = format!("\nai-mesh|high|1\nai-mesh|medium|4\nguv|medium|2\n{OPEN_END}\n");
        assert_eq!(
            parse_open(&text),
            Some(vec![("ai-mesh".into(), 1, 4), ("guv".into(), 0, 2)])
        );
        assert_eq!(parse_open(&format!("\n{OPEN_END}\n")), Some(vec![]), "none open");
        assert_eq!(parse_open("\n"), None, "the query didn't run");
    }

    #[test]
    fn only_plain_report_paths_are_safe() {
        assert!(safe_path(
            "/Users/jonno/.ai-mesh/reviews/reports/guv/2026-11-02-run200.md"
        ));
        assert!(!safe_path(
            "/Users/jonno/.ai-mesh/reviews/reports/../../.ssh/id_ed25519"
        ));
        assert!(!safe_path("/x/y.md; rm -rf ~"));
        assert!(!safe_path("/x/y.txt"));
    }
}
