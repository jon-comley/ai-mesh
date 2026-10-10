//! Merging findings that are the same bug reported twice — by two chunks that
//! both carried the file, or by a review and a later sweep of the same code.

use crate::Finding;

fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Same file, and the same line or the same quoted code. Nearby lines are
/// *not* enough: two real bugs three lines apart are two findings.
fn same_bug(a: &Finding, b: &Finding) -> bool {
    a.repo == b.repo
        && a.path == b.path
        && (a.line == b.line || squash(&a.quote) == squash(&b.quote))
}

/// Merge repeats, keeping the higher severity and every reviewer's name, then
/// sort: severity first, then file and line, so the report reads top-down.
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
        } else {
            out.push(f);
        }
    }
    out.sort_by(|a, b| {
        a.severity
            .cmp(&b.severity)
            .then_with(|| a.repo.cmp(&b.repo))
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.line.cmp(&b.line))
    });
    out
}

/// A stable id for a finding, so dismissing it in the dashboard survives the
/// next run reporting the same thing: repo, path and the squashed quote.
pub fn finding_key(repo: &str, path: &str, quote: &str) -> String {
    // FNV-1a: small, stable across builds and platforms (unlike `DefaultHasher`).
    let mut h: u64 = 0xcbf29ce484222325;
    for b in format!("{repo}\u{0}{path}\u{0}{}", squash(quote)).bytes() {
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
            id: finding_key("r", path, quote),
            repo: "r".into(),
            path: path.into(),
            line,
            severity: sev,
            title: format!("t{line}"),
            quote: quote.into(),
            scenario: "s".into(),
            fix: "x".into(),
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
            finding_key("r", "a", "x  =\n 1"),
            finding_key("r", "a", "x = 1")
        );
        assert_ne!(
            finding_key("r", "a", "x = 1"),
            finding_key("r", "b", "x = 1")
        );
        assert_eq!(finding_key("r", "a", "x = 1").len(), 16);
    }
}
