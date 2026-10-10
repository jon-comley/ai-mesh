//! The quote check: a finding must quote code that is really in the file.
//!
//! Local models invent code. A finding whose quote cannot be found is dropped
//! here, before any model is asked to check it, and a finding whose quote *is*
//! found gets its line number moved to where the quote actually starts —
//! models count lines badly even when they read the code right.

fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Where `quote` starts in `content` (1-based line), or `None` if it is not
/// there. Whitespace is ignored, so re-indented or re-wrapped quotes still
/// match. When the quote occurs more than once, the occurrence nearest
/// `reported_line` wins.
pub fn locate_quote(content: &str, quote: &str, reported_line: u32) -> Option<u32> {
    let q = squash(quote);
    // Too short to identify anything ("}" or "return x;" would match anywhere).
    if q.len() < 8 {
        return None;
    }
    let lines: Vec<&str> = content.lines().collect();
    let first_line = quote.lines().map(str::trim).find(|l| !l.is_empty())?;
    let first = squash(first_line);
    let mut best: Option<u32> = None;
    for (i, line) in lines.iter().enumerate() {
        if !squash(line).contains(&first) {
            continue;
        }
        // Gather enough following lines to hold the whole quote, then compare.
        let mut window = String::new();
        for l in lines.iter().skip(i) {
            if !window.is_empty() {
                window.push(' ');
            }
            window.push_str(&squash(l));
            if window.len() >= q.len() + first.len() + 200 {
                break;
            }
        }
        if window.contains(&q) {
            let ln = i as u32 + 1;
            best = match best {
                Some(b) if b.abs_diff(reported_line) <= ln.abs_diff(reported_line) => Some(b),
                _ => Some(ln),
            };
        }
    }
    best
}

/// The lines around `line`, numbered, for a checking prompt.
pub fn excerpt(content: &str, line: u32, radius: u32) -> String {
    let start = line.saturating_sub(radius).max(1);
    let end = line + radius;
    content
        .lines()
        .enumerate()
        .map(|(i, l)| (i as u32 + 1, l))
        .filter(|(n, _)| *n >= start && *n <= end)
        .map(|(n, l)| format!("{n:>5}| {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "function a() {\n  const agreed = pricing?.amount;\n  return agreed;\n}\n\nfunction b() {\n  const agreed = pricing?.amount;\n}\n";

    #[test]
    fn finds_a_single_line_quote_and_corrects_the_line() {
        assert_eq!(
            locate_quote(FILE, "const agreed = pricing?.amount;", 40),
            Some(7)
        );
        assert_eq!(
            locate_quote(FILE, "const agreed = pricing?.amount;", 1),
            Some(2)
        );
    }

    #[test]
    fn ignores_whitespace_differences_and_spans_lines() {
        let q = "const agreed =   pricing?.amount;\n    return agreed;";
        assert_eq!(locate_quote(FILE, q, 0), Some(2));
    }

    #[test]
    fn invented_code_is_not_found() {
        assert_eq!(
            locate_quote(FILE, "const agreed = acceptance.amount;", 2),
            None
        );
    }

    #[test]
    fn trivially_short_quotes_are_rejected() {
        assert_eq!(locate_quote(FILE, "}", 4), None);
    }

    #[test]
    fn excerpt_numbers_lines_and_clamps_at_the_start() {
        let e = excerpt(FILE, 2, 1);
        assert_eq!(
            e,
            "    1| function a() {\n    2|   const agreed = pricing?.amount;\n    3|   return agreed;"
        );
    }
}
