//! Reading model replies. Local models wrap JSON in prose and code fences,
//! use slightly different field names, and sometimes return one object where
//! an array was asked for; all of that is tolerated here rather than wasting
//! a task on a retry.

use crate::{RawFinding, Severity, Verdict};
use serde_json::Value;

/// The first JSON value of the wanted kind (`[` or `{`) in `text`, found by
/// bracket matching that respects strings. `None` when there isn't one.
pub fn extract_json(text: &str, open: char) -> Option<Value> {
    let close = if open == '[' { ']' } else { '}' };
    let bytes: Vec<char> = text.chars().collect();
    let mut start = 0;
    while let Some(rel) = bytes[start..].iter().position(|&c| c == open) {
        let s = start + rel;
        let mut depth = 0i32;
        let mut in_str = false;
        let mut escaped = false;
        for (i, &c) in bytes.iter().enumerate().skip(s) {
            if in_str {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == '"' {
                    in_str = false;
                }
                continue;
            }
            match c {
                '"' => in_str = true,
                c if c == open => depth += 1,
                c if c == close => {
                    depth -= 1;
                    if depth == 0 {
                        let candidate: String = bytes[s..=i].iter().collect();
                        if let Ok(v) = serde_json::from_str::<Value>(&candidate) {
                            return Some(v);
                        }
                        break;
                    }
                }
                _ => {}
            }
        }
        start = s + 1;
    }
    None
}

fn str_field(v: &Value, names: &[&str]) -> String {
    for n in names {
        match v.get(*n) {
            Some(Value::String(s)) => return s.trim().to_string(),
            Some(Value::Number(n)) => return n.to_string(),
            _ => {}
        }
    }
    String::new()
}

fn line_field(v: &Value) -> u32 {
    for n in ["line", "line_number", "lineNumber"] {
        match v.get(n) {
            Some(Value::Number(x)) => return x.as_u64().unwrap_or(0) as u32,
            Some(Value::String(s)) => {
                let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
                if let Ok(x) = digits.parse() {
                    return x;
                }
            }
            _ => {}
        }
    }
    0
}

/// Findings from a review reply. Entries missing a file, a title or a quote
/// are dropped: without a quote the finding cannot be checked.
pub fn parse_findings(text: &str) -> Vec<RawFinding> {
    let value = extract_json(text, '[').or_else(|| {
        extract_json(text, '{').map(|obj| match obj.get("findings") {
            Some(arr @ Value::Array(_)) => arr.clone(),
            _ => Value::Array(vec![obj]),
        })
    });
    let Some(Value::Array(items)) = value else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|v| {
            let mut file = str_field(v, &["file", "path", "filename"]);
            // "dashboard/src/x.ts:12" — keep the path, take the line if none given.
            let mut line = line_field(v);
            if let Some((p, l)) = file.rsplit_once(':')
                && let Ok(n) = l.parse::<u32>()
            {
                if line == 0 {
                    line = n;
                }
                file = p.to_string();
            }
            let title = str_field(v, &["title", "summary", "issue"]);
            let quote = str_field(v, &["quote", "code", "snippet"]);
            if file.is_empty() || title.is_empty() || quote.is_empty() {
                return None;
            }
            Some(RawFinding {
                file,
                line,
                severity: Severity::parse(&str_field(v, &["severity", "priority"])),
                title,
                quote,
                scenario: str_field(v, &["scenario", "failure_scenario", "impact", "why"]),
                fix: str_field(v, &["fix", "suggested_fix", "suggestion"]),
            })
        })
        .collect()
}

/// A checker's verdict and its reason. An unreadable reply counts as unsure,
/// never as confirmed.
pub fn parse_verdict(text: &str) -> (Verdict, String) {
    if let Some(v) = extract_json(text, '{') {
        let reason = str_field(&v, &["reason", "why", "explanation"]);
        let verdict = match str_field(&v, &["verdict", "answer", "result"])
            .to_ascii_lowercase()
            .as_str()
        {
            "confirmed" | "yes" | "real" | "true" => Verdict::Confirmed,
            "rejected" | "no" | "false" | "not real" => Verdict::Rejected,
            _ => Verdict::Unsure,
        };
        return (verdict, reason);
    }
    let lower = text.trim().to_ascii_lowercase();
    let verdict = if lower.starts_with("yes") || lower.starts_with("confirmed") {
        Verdict::Confirmed
    } else if lower.starts_with("no") || lower.starts_with("rejected") {
        Verdict::Rejected
    } else {
        Verdict::Unsure
    };
    (verdict, text.trim().chars().take(300).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_fenced_array_with_prose_around_it() {
        let reply = "Here are the issues:\n```json\n[{\"file\":\"dashboard/src/a.ts\",\"line\":12,\"severity\":\"high\",\"title\":\"Bills the cheapest option\",\"quote\":\"const agreed = pricing?.amount;\",\"scenario\":\"accepted £1,900, billed £1,200\",\"fix\":\"use acceptance.amount\"}]\n```\nThat's all.";
        let f = parse_findings(reply);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].file, "dashboard/src/a.ts");
        assert_eq!(f[0].line, 12);
        assert_eq!(f[0].severity, Severity::High);
        assert_eq!(f[0].fix, "use acceptance.amount");
    }

    #[test]
    fn accepts_an_object_with_a_findings_key_and_file_colon_line() {
        let reply = r#"{"findings":[{"path":"src/b.ts:40","priority":"low","summary":"x","code":"let y = 1;"}]}"#;
        let f = parse_findings(reply);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].file, "src/b.ts");
        assert_eq!(f[0].line, 40);
        assert_eq!(f[0].severity, Severity::Low);
    }

    #[test]
    fn drops_findings_without_a_quote() {
        let reply = r#"[{"file":"a.ts","line":1,"title":"vague worry"}]"#;
        assert!(parse_findings(reply).is_empty());
    }

    #[test]
    fn empty_array_and_garbage_give_nothing() {
        assert!(parse_findings("[]").is_empty());
        assert!(parse_findings("No issues found.").is_empty());
    }

    #[test]
    fn brackets_inside_strings_do_not_confuse_extraction() {
        let reply = r#"[{"file":"a.ts","line":3,"title":"t","quote":"arr[0] = \"]\";"}]"#;
        assert_eq!(parse_findings(reply)[0].quote, "arr[0] = \"]\";");
    }

    #[test]
    fn verdicts_from_json_and_from_plain_words() {
        assert_eq!(
            parse_verdict(r#"{"verdict":"confirmed","reason":"line 219 uses min"}"#),
            (Verdict::Confirmed, "line 219 uses min".to_string())
        );
        assert_eq!(
            parse_verdict(r#"{"verdict":"rejected"}"#).0,
            Verdict::Rejected
        );
        assert_eq!(
            parse_verdict("No — the guard on line 3 prevents it").0,
            Verdict::Rejected
        );
        assert_eq!(parse_verdict("I can't tell").0, Verdict::Unsure);
    }
}
