//! The Markdown report, laid out like the hand-written dashboard review of
//! 10 October 2026: a summary, then findings by severity with location, quoted
//! code, what goes wrong and the suggested fix.

use crate::{Finding, Severity, Verdict};

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
}

fn finding_block(n: usize, f: &Finding, out: &mut String) {
    out.push_str(&format!("### {n}. {}\n", f.title));
    out.push_str(&format!("- **Where:** `{}`\n", f.location()));
    if !f.scenario.is_empty() {
        out.push_str(&format!("- **What goes wrong:** {}\n", f.scenario));
    }
    if !f.fix.is_empty() {
        out.push_str(&format!("- **Suggested fix:** {}\n", f.fix));
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
    out.push_str(&format!("\n```{lang}\n{}\n```\n\n", f.quote.trim_end()));
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
    let count = |s: Severity| confirmed.iter().filter(|f| f.severity == s).count();
    out.push_str(&format!(
        "- **Confirmed:** {} high, {} medium, {} low. **Not confirmed:** {}. **Thrown out:** {} rejected by the checker, {} quoting code that is not in the file.\n\n",
        count(Severity::High),
        count(Severity::Medium),
        count(Severity::Low),
        unconfirmed.len(),
        rejected,
        info.dropped_by_quote_check,
    ));
    out.push_str(
        "Written by local models. Treat every finding as a lead to check, not a verdict.\n\n",
    );

    if confirmed.is_empty() && unconfirmed.is_empty() {
        out.push_str("No bugs found in this run.\n");
        return out;
    }
    let mut n = 0;
    for sev in [Severity::High, Severity::Medium, Severity::Low] {
        let group: Vec<&&Finding> = confirmed.iter().filter(|f| f.severity == sev).collect();
        if group.is_empty() {
            continue;
        }
        out.push_str(&format!("## {}\n\n", sev.label()));
        for f in group {
            n += 1;
            finding_block(n, f, &mut out);
        }
    }
    if !unconfirmed.is_empty() {
        out.push_str("## Not confirmed\n\nThe checker could not tell, or no second model was free. Worth a look, with more doubt.\n\n");
        for f in unconfirmed {
            n += 1;
            finding_block(n, f, &mut out);
        }
    }
    out
}

/// The one-line ntfy body: counts and the repo name, never code.
pub fn headline(repo: &str, findings: &[Finding]) -> Option<String> {
    let confirmed = |s: Severity| {
        findings
            .iter()
            .filter(|f| f.verdict == Some(Verdict::Confirmed) && f.severity == s)
            .count()
    };
    let (h, m, l) = (
        confirmed(Severity::High),
        confirmed(Severity::Medium),
        confirmed(Severity::Low),
    );
    if h + m + l == 0 {
        return None;
    }
    Some(format!("{repo}: {h} high, {m} medium, {l} low"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(sev: Severity, verdict: Option<Verdict>, title: &str) -> Finding {
        Finding {
            id: title.into(),
            repo: "dashboard".into(),
            path: "src/pages/JobDetailPage.tsx".into(),
            line: 219,
            severity: sev,
            title: title.into(),
            quote: "const agreed = pricing?.amount;".into(),
            scenario: "accepted £1,900, invoice drafted at £1,200".into(),
            fix: "use the accepted amount".into(),
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
        assert!(md.contains("## High\n\n### 1. Invoice bills the cheapest option"));
        assert!(md.contains("`dashboard/src/pages/JobDetailPage.tsx:219`"));
        assert!(md.contains("```tsx\nconst agreed = pricing?.amount;\n```"));
        assert!(md.contains("confirmed by qwen2.5:7b@beelink1 — line 125 takes the minimum"));
        assert!(md.contains("## Not confirmed"));
        assert!(!md.contains("Imaginary bug"));
        assert!(md.contains("1 rejected by the checker, 3 quoting code that is not in the file"));
        assert!(md.contains("qwen3-coder-30b@mac1 (4 tasks)"));
    }

    #[test]
    fn empty_run_says_so() {
        let md = render(&RunInfo::default(), &[]);
        assert!(md.contains("No bugs found in this run."));
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
    }
}
