//! The Markdown report, laid out like the hand-written dashboard review of
//! 10 October 2026: a summary, then the items: bugs by severity, then tests,
//! refactors, improvements and ideas, each with its location, quoted code,
//! an explanation, and the change as a diff. The same file goes to the
//! orchestrator's review inbox on mac1, which fixes or drops each item.

use crate::{Finding, Kind, Severity, Verdict};

/// Everything the report says about the run itself.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RunInfo {
    pub repo: String,
    /// "Nightly: changes from a1b2c3d to e4f5a6b" or "Sunday sweep: src/services".
    pub scope: String,
    /// Local date and time the run finished, already formatted.
    pub finished: String,
    /// `model@node` → tasks done, for the "who did the work" line.
    pub workers: Vec<(String, u32)>,
    pub chunks: u32,
    /// Findings dropped because their quoted code was not in the file.
    pub dropped_by_quote_check: u32,
    pub files_reviewed: u32,
    /// Suggestions past their kind's cap ([`crate::Kind::cap`]), left out.
    #[serde(default)]
    pub left_out_over_cap: u32,
}

fn finding_block(n: usize, f: &Finding, label_kind: bool, out: &mut String) {
    let kind = if label_kind && f.kind != Kind::Bug {
        format!("{}: ", f.kind.as_str())
    } else {
        String::new()
    };
    out.push_str(&format!("### {n}. {kind}{}\n", f.title));
    out.push_str(&format!("- **Where:** `{}`\n", f.location()));
    if !f.scenario.is_empty() {
        let what = if f.kind == Kind::Bug {
            "What goes wrong"
        } else {
            "Why"
        };
        out.push_str(&format!("- **{what}:** {}\n", f.scenario));
    }
    if !f.fix.is_empty() {
        out.push_str(&format!("- **Change:** {}\n", f.fix));
    }
    let checked = match (&f.verdict, &f.checked_by) {
        (Some(Verdict::Confirmed), Some(by)) => format!("confirmed by {by}"),
        (Some(Verdict::Unsure), Some(by)) => format!("{by} could not tell"),
        (Some(Verdict::Rejected), Some(by)) => format!("rejected by {by}"),
        _ => "not checked (no second model was free)".to_string(),
    };
    out.push_str(&format!(
        "- **Found by:** {}; {checked}",
        f.found_by.join(", ")
    ));
    if let Some(reason) = f.verdict_reason.as_deref().filter(|r| !r.is_empty()) {
        out.push_str(&format!(" — {reason}"));
    }
    out.push('\n');
    let lang = f.path.rsplit('.').next().unwrap_or("");
    out.push_str(&format!("\n```{lang}\n{}\n```\n", f.quote.trim_end()));
    if !f.patch.trim().is_empty() {
        let note = match f.patch_applies {
            Some(true) => " (applies cleanly)",
            Some(false) => {
                " (does not apply cleanly to the code as it is now: a starting point only)"
            }
            None => "",
        };
        out.push_str(&format!(
            "\n**Diff**{note}:\n\n```diff\n{}\n```\n",
            f.patch.trim_end()
        ));
    }
    out.push('\n');
}

/// Render the report. Rejected findings are left out (and counted); unchecked
/// and unsure ones go in their own section so they read as leads, not facts.
pub fn render(info: &RunInfo, findings: &[Finding]) -> String {
    let confirmed: Vec<&Finding> = findings
        .iter()
        .filter(|f| f.verdict == Some(Verdict::Confirmed))
        .collect();
    let unconfirmed: Vec<&Finding> = findings
        .iter()
        .filter(|f| f.verdict != Some(Verdict::Confirmed) && f.verdict != Some(Verdict::Rejected))
        .collect();
    let rejected = findings
        .iter()
        .filter(|f| f.verdict == Some(Verdict::Rejected))
        .count();

    let mut out = format!("# Code review: {}\n\n", info.repo);
    out.push_str(&format!("- **Scope:** {}\n", info.scope));
    out.push_str(&format!("- **Finished:** {}\n", info.finished));
    out.push_str(&format!(
        "- **Size:** {} files under review in {} chunks\n",
        info.files_reviewed, info.chunks
    ));
    if !info.workers.is_empty() {
        let who: Vec<String> = info
            .workers
            .iter()
            .map(|(w, n)| format!("{w} ({n} tasks)"))
            .collect();
        out.push_str(&format!("- **Done by:** {}\n", who.join(", ")));
    }
    let bugs = |s: Severity| {
        confirmed
            .iter()
            .filter(|f| f.kind == Kind::Bug && f.severity == s)
            .count()
    };
    let of = |k: Kind| confirmed.iter().filter(|f| f.kind == k).count();
    out.push_str(&format!(
        "- **Confirmed bugs:** {} high, {} medium, {} low. **Suggestions:** {} tests, {} refactors, {} improvements, {} ideas.\n",
        bugs(Severity::High),
        bugs(Severity::Medium),
        bugs(Severity::Low),
        of(Kind::Test),
        of(Kind::Refactor),
        of(Kind::Improvement),
        of(Kind::Feature),
    ));
    out.push_str(&format!(
        "- **Not confirmed:** {}. **Thrown out:** {} rejected by the checker, {} quoting code that is not in the file{}.\n\n",
        unconfirmed.len(),
        rejected,
        info.dropped_by_quote_check,
        if info.left_out_over_cap > 0 {
            format!(", {} suggestions over the per-kind limit", info.left_out_over_cap)
        } else {
            String::new()
        },
    ));
    out.push_str(
        "Written by models. Treat every item as a lead to check, not a verdict: fix each bug, \
         test, refactor and improvement (the diff is a starting point), or drop it with the \
         reason. Ideas are for Jon to choose from.\n\n",
    );
    if confirmed.is_empty() && unconfirmed.is_empty() {
        out.push_str("Nothing found in this run.\n");
        return out;
    }

    let mut n = 0;
    for sev in [Severity::High, Severity::Medium, Severity::Low] {
        let group: Vec<&&Finding> = confirmed
            .iter()
            .filter(|f| f.kind == Kind::Bug && f.severity == sev)
            .collect();
        if group.is_empty() {
            continue;
        }
        out.push_str(&format!("## Bugs: {}\n\n", sev.label().to_lowercase()));
        for f in group {
            n += 1;
            finding_block(n, f, false, &mut out);
        }
    }
    for kind in [Kind::Test, Kind::Refactor, Kind::Improvement, Kind::Feature] {
        let group: Vec<&&Finding> = confirmed.iter().filter(|f| f.kind == kind).collect();
        if group.is_empty() {
            continue;
        }
        out.push_str(&format!("## {}\n\n", kind.heading()));
        for f in group {
            n += 1;
            finding_block(n, f, false, &mut out);
        }
    }
    if !unconfirmed.is_empty() {
        out.push_str("## Not confirmed\n\nThe checker could not tell, or no second model was free. Worth a look, with more doubt.\n\n");
        for f in unconfirmed {
            n += 1;
            finding_block(n, f, true, &mut out);
        }
    }
    out
}

/// The one-line ntfy body: counts and the repo name, never code.
pub fn headline(repo: &str, findings: &[Finding]) -> Option<String> {
    let confirmed: Vec<&Finding> = findings
        .iter()
        .filter(|f| f.verdict == Some(Verdict::Confirmed))
        .collect();
    let bugs = |s: Severity| {
        confirmed
            .iter()
            .filter(|f| f.kind == Kind::Bug && f.severity == s)
            .count()
    };
    let (h, m, l) = (
        bugs(Severity::High),
        bugs(Severity::Medium),
        bugs(Severity::Low),
    );
    let suggestions = confirmed.iter().filter(|f| f.kind != Kind::Bug).count();
    if h + m + l + suggestions == 0 {
        return None;
    }
    let mut line = format!("{repo}: {h} high, {m} medium, {l} low");
    if suggestions > 0 {
        line.push_str(&format!(
            "; {suggestions} suggestion{}",
            if suggestions == 1 { "" } else { "s" }
        ));
    }
    Some(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(sev: Severity, verdict: Option<Verdict>, title: &str) -> Finding {
        Finding {
            id: title.into(),
            kind: Kind::Bug,
            repo: "dashboard".into(),
            path: "src/pages/JobDetailPage.tsx".into(),
            line: 219,
            severity: sev,
            title: title.into(),
            quote: "const agreed = pricing?.amount;".into(),
            scenario: "accepted £1,900, invoice drafted at £1,200".into(),
            fix: "use the accepted amount".into(),
            patch: String::new(),
            patch_applies: None,
            found_by: vec!["qwen3-coder-30b@mac1".into()],
            verdict,
            verdict_reason: Some("line 125 takes the minimum".into()),
            checked_by: Some("qwen2.5:7b@beelink1".into()),
        }
    }

    #[test]
    fn report_groups_by_severity_and_leaves_rejected_out() {
        let info = RunInfo {
            repo: "dashboard".into(),
            scope: "Nightly: changes from aaa to bbb".into(),
            finished: "2026-10-11 02:31".into(),
            workers: vec![("qwen3-coder-30b@mac1".into(), 4)],
            chunks: 2,
            dropped_by_quote_check: 3,
            files_reviewed: 7,
            left_out_over_cap: 0,
        };
        let md = render(
            &info,
            &[
                f(
                    Severity::High,
                    Some(Verdict::Confirmed),
                    "Invoice bills the cheapest option",
                ),
                f(Severity::Low, Some(Verdict::Rejected), "Imaginary bug"),
                f(Severity::Medium, Some(Verdict::Unsure), "Maybe a race"),
            ],
        );
        assert!(md.starts_with("# Code review: dashboard"));
        assert!(md.contains("## Bugs: high\n\n### 1. Invoice bills the cheapest option"));
        assert!(md.contains("`dashboard/src/pages/JobDetailPage.tsx:219`"));
        assert!(md.contains("```tsx\nconst agreed = pricing?.amount;\n```"));
        assert!(md.contains("confirmed by qwen2.5:7b@beelink1 — line 125 takes the minimum"));
        assert!(md.contains("## Not confirmed"));
        assert!(!md.contains("Imaginary bug"));
        assert!(md.contains("1 rejected by the checker, 3 quoting code that is not in the file."));
        assert!(md.contains("qwen3-coder-30b@mac1 (4 tasks)"));
    }

    #[test]
    fn empty_run_says_so() {
        let md = render(&RunInfo::default(), &[]);
        assert!(md.contains("Nothing found in this run."));
    }

    #[test]
    fn headline_counts_only_confirmed_and_is_none_when_quiet() {
        assert_eq!(
            headline("d", &[f(Severity::High, Some(Verdict::Unsure), "x")]),
            None
        );
        assert_eq!(
            headline("d", &[f(Severity::High, Some(Verdict::Confirmed), "x")]),
            Some("d: 1 high, 0 medium, 0 low".into())
        );
        let mut idea = f(Severity::Low, Some(Verdict::Confirmed), "y");
        idea.kind = Kind::Feature;
        assert_eq!(
            headline("d", &[idea]),
            Some("d: 0 high, 0 medium, 0 low; 1 suggestion".into())
        );
    }

    #[test]
    fn suggestions_follow_the_bugs_with_their_diffs() {
        let mut test = f(
            Severity::Low,
            Some(Verdict::Confirmed),
            "Test the VAT rounding",
        );
        test.kind = Kind::Test;
        test.patch =
            "--- /dev/null\n+++ b/src/vat.test.ts\n@@ -0,0 +1 @@\n+it('rounds', () => {});".into();
        test.patch_applies = Some(true);
        let mut idea = f(
            Severity::Low,
            Some(Verdict::Confirmed),
            "Show the VAT on the quote",
        );
        idea.kind = Kind::Feature;
        let mut maybe = f(
            Severity::Low,
            Some(Verdict::Unsure),
            "Merge the two loaders",
        );
        maybe.kind = Kind::Refactor;
        let md = render(
            &RunInfo::default(),
            &[
                f(
                    Severity::High,
                    Some(Verdict::Confirmed),
                    "Invoice bills the cheapest option",
                ),
                test,
                idea,
                maybe,
            ],
        );
        let bugs = md.find("## Bugs: high").unwrap();
        let tests = md.find("## Tests to add").unwrap();
        let ideas = md.find("## Ideas").unwrap();
        assert!(bugs < tests && tests < ideas);
        assert!(md.contains("**Diff** (applies cleanly):\n\n```diff\n--- /dev/null"));
        assert!(md.contains("**Why:**"));
        assert!(md.contains("### 4. refactor: Merge the two loaders"));
        assert!(md.contains("1 tests, 0 refactors, 0 improvements, 1 ideas"));
    }
}
