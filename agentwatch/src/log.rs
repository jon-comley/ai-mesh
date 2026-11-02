//! A cloud agent's `agent-log.md`: newest first, one entry per run:
//!
//! ```text
//! - **2026-11-02** <summary>. [Report](reports/2026-11-02.md)
//! ```
//!
//! An entry may wrap onto indented lines below it.

use chrono::NaiveDate;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Entry {
    pub date: NaiveDate,
    /// The entry's text with the report link taken out, wrapped lines joined.
    pub summary: String,
    /// The report's path in the repo, when the entry links one.
    pub report: Option<String>,
}

/// Every entry in the file, in file order (newest first). Lines that are not
/// part of an entry are skipped.
pub fn parse(text: &str) -> Vec<Entry> {
    let mut raw: Vec<(NaiveDate, String)> = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("- **")
            && let Some((date, after)) = rest.split_once("**")
            && let Ok(date) = NaiveDate::parse_from_str(date.trim(), "%Y-%m-%d")
        {
            raw.push((date, after.trim().to_string()));
            continue;
        }
        // An indented line carries on the entry above it; anything else is skipped.
        let continues = line.starts_with(' ') || line.starts_with('\t');
        if continues
            && !line.trim().is_empty()
            && let Some((_, body)) = raw.last_mut()
        {
            body.push(' ');
            body.push_str(line.trim());
        }
    }
    raw.into_iter()
        .map(|(date, body)| entry(date, &body))
        .collect()
}

fn entry(date: NaiveDate, body: &str) -> Entry {
    let (summary, report) = match body.find("[Report](") {
        Some(start) => {
            let after = &body[start + "[Report](".len()..];
            match after.find(')') {
                Some(end) => {
                    let summary = format!("{} {}", &body[..start], &after[end + 1..]);
                    (summary, Some(after[..end].to_string()))
                }
                None => (body.to_string(), None),
            }
        }
        None => (body.to_string(), None),
    };
    Entry {
        date,
        summary: summary.split_whitespace().collect::<Vec<_>>().join(" "),
        report,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn reads_the_fixture_newest_first_with_wrapped_lines() {
        let entries = parse(include_str!("../fixtures/agent-log.md"));
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].date, d("2026-11-09"));
        assert_eq!(
            entries[0].summary,
            "wrote DOC-002, answered two replies, raised one question."
        );
        assert_eq!(entries[0].report.as_deref(), Some("reports/2026-11-09.md"));
        assert_eq!(entries[1].date, d("2026-11-02"));
        assert_eq!(
            entries[1].summary,
            "set-up, with Jon: register, DOC-001 drafted, the hub."
        );
    }

    #[test]
    fn an_entry_without_a_report_keeps_its_whole_text() {
        let e = parse("- **2026-11-03** nothing changed anywhere.\n");
        assert_eq!(e[0].summary, "nothing changed anywhere.");
        assert_eq!(e[0].report, None);
    }

    #[test]
    fn skips_lines_that_are_not_entries() {
        let e = parse("# Log\n\n- not an entry\n- **soon** not a date\n  indented under nothing\n");
        assert!(e.is_empty());
    }

    #[test]
    fn an_empty_log_has_no_entries() {
        assert!(parse("").is_empty());
    }
}
