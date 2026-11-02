//! The rows in an agent's `HUB.md` that are waiting on Jon, and writing his
//! reply into one.
//!
//! The rows are the Markdown table under one `## <heading>` (`Needs you` for
//! Standards, `Only you` for Desk). The first column is the row's number and the
//! last is "Your reply"; an empty reply means it is still waiting on Jon.

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Row {
    /// The `#` column, which is how a reply names its row.
    pub number: String,
    /// Every cell, header order, trimmed.
    pub cells: Vec<String>,
    /// The header row, so the page can label the cells.
    pub headers: Vec<String>,
    /// The "Your reply" cell.
    pub reply: String,
}

impl Row {
    pub fn waiting(&self) -> bool {
        self.reply.is_empty()
    }
}

/// The table rows under `## <heading>`, in file order.
pub fn rows(hub: &str, heading: &str) -> Vec<Row> {
    let Some(table) = section_table(hub, heading) else {
        return Vec::new();
    };
    let mut lines = table.iter().map(|(_, l)| *l);
    let Some(header) = lines.next() else {
        return Vec::new();
    };
    let headers = cells(header);
    lines
        .filter(|l| !is_separator(l))
        .map(|l| {
            let cells = cells(l);
            Row {
                number: cells.first().cloned().unwrap_or_default(),
                reply: cells.last().cloned().unwrap_or_default(),
                headers: headers.clone(),
                cells,
            }
        })
        .collect()
}

/// How many rows under `heading` have no reply yet.
pub fn waiting_count(hub: &str, heading: &str) -> usize {
    rows(hub, heading).iter().filter(|r| r.waiting()).count()
}

#[derive(Debug, PartialEq)]
pub enum ReplyError {
    NoSuchRow,
    /// A reply cannot hold a `|` or a line break: it would break the table.
    BadText,
}

/// Puts `text` in the "Your reply" cell of row `number` under `heading`,
/// replacing whatever was there. Nothing else in the file changes.
pub fn set_reply(hub: &str, heading: &str, number: &str, text: &str) -> Result<String, ReplyError> {
    let text = text.trim();
    if text.is_empty() || text.contains('|') || text.contains('\n') || text.contains('\r') {
        return Err(ReplyError::BadText);
    }
    let table = section_table(hub, heading).ok_or(ReplyError::NoSuchRow)?;
    let (index, line) = table
        .iter()
        .skip(1)
        .filter(|(_, l)| !is_separator(l))
        .find(|(_, l)| cells(l).first().map(String::as_str) == Some(number))
        .ok_or(ReplyError::NoSuchRow)?;
    let mut parts: Vec<&str> = line.trim().trim_matches('|').split('|').collect();
    let reply = format!(" {text} ");
    *parts.last_mut().expect("a table row has cells") = &reply;
    let rewritten = format!("|{}|", parts.join("|"));

    let mut out = String::with_capacity(hub.len() + text.len());
    for (i, l) in hub.split_inclusive('\n').enumerate() {
        if i == *index {
            out.push_str(&rewritten);
            if l.ends_with('\n') {
                out.push('\n');
            }
        } else {
            out.push_str(l);
        }
    }
    Ok(out)
}

/// The table lines (with their line index in `hub`) of the first table under
/// `## <heading>`, header first. Stops at the end of the table or the next heading.
fn section_table<'a>(hub: &'a str, heading: &str) -> Option<Vec<(usize, &'a str)>> {
    let mut in_section = false;
    let mut table = Vec::new();
    for (i, line) in hub.lines().enumerate() {
        let t = line.trim();
        if let Some(h) = t.strip_prefix("## ") {
            if in_section {
                break;
            }
            in_section = h.trim().eq_ignore_ascii_case(heading);
            continue;
        }
        if !in_section {
            continue;
        }
        if t.starts_with('|') {
            table.push((i, line));
        } else if !table.is_empty() {
            break;
        }
    }
    in_section.then_some(table).filter(|t| !t.is_empty())
}

fn cells(line: &str) -> Vec<String> {
    line.trim()
        .trim_matches('|')
        .split('|')
        .map(|c| c.trim().to_string())
        .collect()
}

fn is_separator(line: &str) -> bool {
    line.trim().trim_matches('|').split('|').all(|c| {
        let c = c.trim();
        !c.is_empty() && c.chars().all(|ch| ch == '-' || ch == ':')
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const HUB: &str = include_str!("../fixtures/hub.md");

    #[test]
    fn reads_the_rows_under_the_heading() {
        let r = rows(HUB, "Needs you");
        assert_eq!(r.len(), 3);
        assert_eq!(r[0].number, "3");
        assert_eq!(r[0].headers, ["#", "Raised", "What", "Type", "Your reply"]);
        assert_eq!(r[0].cells[2], "Pick a supplier for the widgets");
        assert!(r[0].waiting());
        assert_eq!(r[1].reply, "Approved");
        assert!(!r[1].waiting());
        assert!(r[2].waiting(), "a reply cell of spaces is still empty");
    }

    #[test]
    fn counts_only_rows_without_a_reply() {
        assert_eq!(waiting_count(HUB, "Needs you"), 2);
        assert_eq!(
            waiting_count(HUB, "needs YOU"),
            2,
            "the heading match ignores case"
        );
    }

    #[test]
    fn a_missing_heading_or_an_empty_table_has_no_rows() {
        assert!(rows(HUB, "Only you").is_empty());
        let empty = "## Only you\n| # | What | Your reply |\n|---|---|---|\n\n## Done\n";
        assert!(rows(empty, "Only you").is_empty());
    }

    #[test]
    fn does_not_read_into_the_next_section() {
        let r = rows(HUB, "Done");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].reply, "The first one");
    }

    #[test]
    fn writes_a_reply_into_that_row_only() {
        let out = set_reply(HUB, "Needs you", "3", "Go with the cheaper one").unwrap();
        assert_eq!(rows(&out, "Needs you")[0].reply, "Go with the cheaper one");
        assert_eq!(waiting_count(&out, "Needs you"), 1);
        let changed: Vec<_> = HUB
            .lines()
            .zip(out.lines())
            .filter(|(a, b)| a != b)
            .collect();
        assert_eq!(changed.len(), 1);
        assert_eq!(
            changed[0].1,
            "| 3 | 2026-11-02 | Pick a supplier for the widgets | Decision | Go with the cheaper one |"
        );
    }

    #[test]
    fn replaces_an_existing_reply() {
        let out = set_reply(HUB, "Needs you", "2", "Not yet").unwrap();
        assert_eq!(rows(&out, "Needs you")[1].reply, "Not yet");
    }

    #[test]
    fn refuses_a_row_that_is_not_there_or_text_that_would_break_the_table() {
        assert_eq!(
            set_reply(HUB, "Needs you", "9", "x"),
            Err(ReplyError::NoSuchRow)
        );
        assert_eq!(
            set_reply(HUB, "Only you", "1", "x"),
            Err(ReplyError::NoSuchRow)
        );
        assert_eq!(
            set_reply(HUB, "Needs you", "3", "a | b"),
            Err(ReplyError::BadText)
        );
        assert_eq!(
            set_reply(HUB, "Needs you", "3", "two\nlines"),
            Err(ReplyError::BadText)
        );
        assert_eq!(
            set_reply(HUB, "Needs you", "3", "  "),
            Err(ReplyError::BadText)
        );
    }
}
