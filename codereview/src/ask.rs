//! Answering questions about a repo: choosing which files to show the model,
//! and the prompt. The capability searches the repo (`git grep`) for the
//! question's words; this ranks what it finds.

use crate::chunk::file_cost;
use crate::prompt::{Prompt, numbered};
use crate::{SourceFile, estimate_tokens};
use std::collections::HashMap;

pub const ASK_INSTRUCTIONS: &str = include_str!("../prompts/ask.md");

/// Most search terms taken from one question.
pub const MAX_TERMS: usize = 12;
/// Most files shown for one question.
pub const MAX_FILES: usize = 40;

const STOPWORDS: &[&str] = &[
    "the",
    "and",
    "for",
    "are",
    "was",
    "were",
    "with",
    "that",
    "this",
    "what",
    "where",
    "when",
    "which",
    "who",
    "why",
    "how",
    "does",
    "did",
    "doing",
    "done",
    "can",
    "could",
    "should",
    "would",
    "will",
    "into",
    "from",
    "about",
    "there",
    "their",
    "they",
    "them",
    "then",
    "than",
    "have",
    "has",
    "had",
    "any",
    "all",
    "out",
    "get",
    "got",
    "set",
    "use",
    "used",
    "using",
    "code",
    "file",
    "files",
    "function",
    "functions",
    "work",
    "works",
    "worked",
    "happen",
    "happens",
    "handled",
    "handle",
    "find",
    "show",
    "tell",
    "explain",
    "repo",
    "please",
    "you",
    "your",
    "our",
    "its",
    "not",
    "but",
    "way",
    "each",
    "every",
    "some",
    "make",
    "made",
    "between",
];

fn split_camel(token: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let chars: Vec<char> = token.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        let boundary = i > 0
            && c.is_uppercase()
            && (chars[i - 1].is_lowercase()
                || chars.get(i + 1).is_some_and(|n| n.is_lowercase())
                    && chars[i - 1].is_uppercase());
        if (boundary || c == '_') && !cur.is_empty() {
            parts.push(std::mem::take(&mut cur));
        }
        if c != '_' {
            cur.push(c);
        }
    }
    if !cur.is_empty() {
        parts.push(cur);
    }
    parts
}

/// The words worth searching for: identifiers as written (`parsePrice`),
/// their parts (`parse`, `price`), and other words of three letters or more
/// that are not filler. Lower-cased, de-duplicated, at most [`MAX_TERMS`].
pub fn search_terms(question: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |t: String| {
        let t = t.to_lowercase();
        if t.len() >= 3 && !STOPWORDS.contains(&t.as_str()) && !out.contains(&t) {
            out.push(t);
        }
    };
    for token in
        question.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.' || c == '/'))
    {
        let token = token.trim_matches(|c| c == '.' || c == '/');
        if token.is_empty() {
            continue;
        }
        // Paths and dotted names ("src/lib/money.ts", "jobs.ts") are searched whole.
        push(token.to_string());
        for part in token.split(['.', '/']) {
            let parts = split_camel(part);
            if parts.len() > 1 {
                push(part.to_string());
            }
            for p in parts {
                push(p);
            }
        }
    }
    out.truncate(MAX_TERMS);
    out
}

/// Rank files from per-term match counts (`hits[term][path] = count`).
/// Rarer terms count for more; a term in the file's *path* counts double.
/// Returns paths, best first, with their scores.
pub fn rank_files(
    terms: &[String],
    hits: &HashMap<String, HashMap<String, u32>>,
    all_paths: &[String],
) -> Vec<(String, f32)> {
    let n = all_paths.len().max(1) as f32;
    let mut scores: HashMap<&str, f32> = HashMap::new();
    for term in terms {
        let in_files = hits.get(term);
        let in_paths: Vec<&String> = all_paths
            .iter()
            .filter(|p| p.to_lowercase().contains(term.as_str()))
            .collect();
        let df = in_files.map(|m| m.len()).unwrap_or(0).max(in_paths.len()) as f32;
        let idf = (n / (1.0 + df)).ln().max(0.1);
        if let Some(m) = in_files {
            for (path, &count) in m {
                if let Some(p) = all_paths.iter().find(|p| *p == path) {
                    *scores.entry(p.as_str()).or_insert(0.0) +=
                        idf * (1.0 + (count as f32).ln_1p());
                }
            }
        }
        for p in in_paths {
            *scores.entry(p.as_str()).or_insert(0.0) += 2.0 * idf;
        }
    }
    let mut ranked: Vec<(String, f32)> = scores
        .into_iter()
        .map(|(p, s)| (p.to_string(), s))
        .collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ranked
}

/// Take files best first while they fit in `budget` tokens.
pub fn fit_files(files: Vec<SourceFile>, budget: usize) -> Vec<SourceFile> {
    let mut used = 0;
    let mut out = Vec::new();
    for f in files {
        if out.len() >= MAX_FILES {
            break;
        }
        let cost = file_cost(&f);
        if used + cost > budget {
            continue;
        }
        used += cost;
        out.push(f);
    }
    out
}

/// The prompt for one question over the chosen files.
pub fn ask_prompt(repo: &str, question: &str, files: &[SourceFile]) -> Prompt {
    let mut user = format!("Repository: {repo}\n");
    for f in files {
        user.push_str(&format!("\n=== FILE: {} ===\n", f.display_name()));
        user.push_str(&numbered(&f.content));
        user.push('\n');
    }
    user.push_str(&format!("\nThe question: {question}\n"));
    Prompt {
        system: ASK_INSTRUCTIONS.to_string(),
        user,
    }
}

/// Room left for files once the instructions and question are in.
pub fn file_budget(total: usize, question: &str) -> usize {
    total.saturating_sub(estimate_tokens(ASK_INSTRUCTIONS) + estimate_tokens(question) + 200)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terms_keep_identifiers_and_their_parts_and_drop_filler() {
        let t = search_terms("Where is the invoice total worked out in parsePrice?");
        assert_eq!(t, vec!["invoice", "total", "parseprice", "parse", "price"]);
    }

    #[test]
    fn terms_keep_paths_and_snake_case() {
        let t = search_terms("What does src/lib/money.ts do with max_job_price?");
        assert!(t.contains(&"src/lib/money.ts".to_string()));
        assert!(t.contains(&"money".to_string()));
        assert!(t.contains(&"max_job_price".to_string()));
        assert!(t.contains(&"price".to_string()));
    }

    #[test]
    fn terms_are_capped() {
        let q = (0..30)
            .map(|i| format!("word{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(search_terms(&q).len(), MAX_TERMS);
    }

    #[test]
    fn rare_terms_and_path_matches_rank_higher() {
        let paths: Vec<String> = ["src/invoice.ts", "src/a.ts", "src/b.ts", "src/c.ts"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let terms = vec!["invoice".to_string(), "total".to_string()];
        let mut hits = HashMap::new();
        // "total" is everywhere (common), "invoice" only in a.ts and invoice.ts.
        hits.insert(
            "total".to_string(),
            paths.iter().map(|p| (p.clone(), 5)).collect(),
        );
        hits.insert(
            "invoice".to_string(),
            [
                ("src/a.ts".to_string(), 3),
                ("src/invoice.ts".to_string(), 1),
            ]
            .into(),
        );
        let ranked = rank_files(&terms, &hits, &paths);
        assert_eq!(ranked[0].0, "src/invoice.ts");
        assert_eq!(ranked[1].0, "src/a.ts");
        assert!(ranked[1].1 > ranked[2].1);
    }

    #[test]
    fn files_are_taken_best_first_within_budget() {
        let f = |p: &str, n: usize| SourceFile {
            repo: "r".into(),
            path: p.into(),
            content: "x\n".repeat(n),
            diff: None,
            target: false,
        };
        let kept = fit_files(vec![f("big", 5000), f("small", 10), f("mid", 500)], 2_000);
        let names: Vec<&str> = kept.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(names, vec!["small", "mid"]);
    }

    #[test]
    fn prompt_numbers_lines_and_ends_with_the_question() {
        let files = vec![SourceFile {
            repo: "dashboard".into(),
            path: "src/a.ts".into(),
            content: "const a = 1;".into(),
            diff: None,
            target: false,
        }];
        let p = ask_prompt("dashboard", "What is a?", &files);
        assert!(p.user.contains("=== FILE: dashboard/src/a.ts ==="));
        assert!(p.user.contains("    1| const a = 1;"));
        assert!(p.user.trim_end().ends_with("The question: What is a?"));
        assert!(p.system.contains("repo/path:line"));
        assert!(file_budget(10_000, "What is a?") < 10_000);
    }
}
