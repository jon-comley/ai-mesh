//! Merging findings that are the same bug reported twice — by two chunks that
//! both carried the file, or by a review and a later sweep of the same code.

use crate::{Finding, Kind};

fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Same kind, same file, and the same line or the same quoted code. Nearby
/// lines are *not* enough: two real bugs three lines apart are two findings.
/// A bug and a refactor of the same line are two findings too.
fn same_bug(a: &Finding, b: &Finding) -> bool {
    a.kind == b.kind
        && a.repo == b.repo
        && a.path == b.path
        && (a.line == b.line || squash(&a.quote) == squash(&b.quote))
}

/// Merge repeats, keeping the higher severity and every reviewer's name, then
/// sort: kind first (bugs, then tests, refactors, improvements, ideas), then
/// severity, then file and line, so the report reads top-down.
pub fn dedupe(findings: Vec<Finding>) -> Vec<Finding> {
    let mut out: Vec<Finding> = Vec::new();
    for f in findings {
        if let Some(existing) = out.iter_mut().find(|e| same_bug(e, &f)) {
            if f.severity < existing.severity {
                existing.severity = f.severity;
            }
            for who in f.found_by {
                if !existing.found_by.contains(&who) {
                    existing.found_by.push(who);
                }
            }
            if existing.scenario.len() < f.scenario.len() {
                existing.scenario = f.scenario;
            }
            if existing.patch.is_empty() {
                existing.patch = f.patch;
            }
        } else {
            out.push(f);
        }
    }
    out.sort_by(|a, b| {
        a.kind
            .cmp(&b.kind)
            .then_with(|| a.severity.cmp(&b.severity))
            .then_with(|| a.repo.cmp(&b.repo))
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.line.cmp(&b.line))
    });
    out
}

/// Keep at most [`Kind::cap`] of each suggestion kind: checked ones before
/// unchecked, in the order given (worst first after [`dedupe`]). Bugs are
/// never capped. Returns the kept findings and how many were left out.
pub fn cap_suggestions(findings: Vec<Finding>) -> (Vec<Finding>, usize) {
    use crate::Verdict;
    let rank = |f: &Finding| match f.verdict {
        Some(Verdict::Confirmed) => 0,
        Some(Verdict::Unsure) => 1,
        None => 2,
        Some(Verdict::Rejected) => 3,
    };
    let mut keep = vec![true; findings.len()];
    for kind in Kind::ALL {
        let Some(cap) = kind.cap() else { continue };
        let mut idx: Vec<usize> = (0..findings.len())
            .filter(|&i| findings[i].kind == kind && findings[i].verdict != Some(Verdict::Rejected))
            .collect();
        idx.sort_by_key(|&i| rank(&findings[i]));
        for &i in idx.iter().skip(cap) {
            keep[i] = false;
        }
    }
    let left_out = keep.iter().filter(|k| !**k).count();
    let kept = findings
        .into_iter()
        .zip(keep)
        .filter_map(|(f, k)| k.then_some(f))
        .collect();
    (kept, left_out)
}

/// A stable id for a finding, so dismissing it in the dashboard survives the
/// next run reporting the same thing: repo, path and the squashed quote, plus
/// the kind for anything but a bug (a bug's id is what it always was).
pub fn finding_key(kind: Kind, repo: &str, path: &str, quote: &str) -> String {
    let kind = match kind {
        Kind::Bug => String::new(),
        k => format!("{}\u{0}", k.as_str()),
    };
    // FNV-1a: small, stable across builds and platforms (unlike `DefaultHasher`).
    let mut h: u64 = 0xcbf29ce484222325;
    for b in format!("{kind}{repo}\u{0}{path}\u{0}{}", squash(quote)).bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Severity;

    fn f(path: &str, line: u32, sev: Severity, quote: &str, by: &str) -> Finding {
        Finding {
            id: finding_key(Kind::Bug, "r", path, quote),
            kind: Kind::Bug,
            repo: "r".into(),
            path: path.into(),
            line,
            severity: sev,
            title: format!("t{line}"),
            quote: quote.into(),
            scenario: "s".into(),
            fix: "x".into(),
            patch: String::new(),
            patch_applies: None,
            found_by: vec![by.into()],
            verdict: None,
            verdict_reason: None,
            checked_by: None,
        }
    }

    #[test]
    fn the_same_line_merges_keeping_the_worst_severity() {
        let out = dedupe(vec![
            f("a.ts", 10, Severity::Medium, "x = 1", "m1@n1"),
            f("a.ts", 10, Severity::High, "y = 2", "m2@n2"),
            f("a.ts", 12, Severity::Low, "z = 3", "m2@n2"),
        ]);
        assert_eq!(
            out.len(),
            2,
            "a different bug two lines down stays separate"
        );
        assert_eq!(out[0].severity, Severity::High);
        assert_eq!(out[0].found_by, vec!["m1@n1", "m2@n2"]);
    }

    #[test]
    fn same_quote_far_apart_still_merges_but_other_files_do_not() {
        let out = dedupe(vec![
            f("a.ts", 10, Severity::Low, "const z = 3;", "m"),
            f("a.ts", 90, Severity::Low, "const  z = 3;", "m"),
            f("b.ts", 10, Severity::Low, "const z = 3;", "m"),
        ]);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn sorted_by_severity_then_location() {
        let out = dedupe(vec![
            f("b.ts", 1, Severity::Low, "aaaa aaaa", "m"),
            f("a.ts", 50, Severity::High, "bbbb bbbb", "m"),
            f("a.ts", 5, Severity::High, "cccc cccc", "m"),
        ]);
        let order: Vec<(u32, Severity)> = out.iter().map(|f| (f.line, f.severity)).collect();
        assert_eq!(
            order,
            vec![
                (5, Severity::High),
                (50, Severity::High),
                (1, Severity::Low)
            ]
        );
    }

    #[test]
    fn finding_key_ignores_whitespace_and_is_stable() {
        assert_eq!(
            finding_key(Kind::Bug, "r", "a", "x  =\n 1"),
            finding_key(Kind::Bug, "r", "a", "x = 1")
        );
        assert_ne!(
            finding_key(Kind::Bug, "r", "a", "x = 1"),
            finding_key(Kind::Bug, "r", "b", "x = 1")
        );
        assert_ne!(
            finding_key(Kind::Bug, "r", "a", "x = 1"),
            finding_key(Kind::Refactor, "r", "a", "x = 1")
        );
        // A bug's id is unchanged from before kinds existed, so a finding
        // dismissed then stays dismissed.
        assert_eq!(
            finding_key(Kind::Bug, "r", "a", "x = 1"),
            "08fdfd61b4b9ae76"
        );
        assert_eq!(finding_key(Kind::Bug, "r", "a", "x = 1").len(), 16);
    }

    #[test]
    fn kinds_stay_apart_sort_after_bugs_and_are_capped() {
        let mut refactor = f("a.ts", 10, Severity::Low, "x = 1", "m");
        refactor.kind = Kind::Refactor;
        let out = dedupe(vec![refactor, f("a.ts", 10, Severity::Low, "x = 1", "m")]);
        assert_eq!(
            out.len(),
            2,
            "a bug and a refactor of one line are two findings"
        );
        assert_eq!(out[0].kind, Kind::Bug);

        let ideas: Vec<Finding> = (0..4)
            .map(|i| {
                let mut x = f("a.ts", i, Severity::Low, &format!("idea number {i}"), "m");
                x.kind = Kind::Feature;
                x.verdict = (i == 3).then_some(crate::Verdict::Confirmed);
                x
            })
            .collect();
        let (kept, left_out) = cap_suggestions(ideas);
        assert_eq!((kept.len(), left_out), (2, 2));
        assert!(
            kept.iter().any(|x| x.line == 3),
            "a confirmed idea is kept first"
        );
    }
}
