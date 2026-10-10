//! Prompts for the review and check tasks. The instructions live in
//! `prompts/*.md` so they can be tuned without touching code; this module only
//! lays the files out underneath them.

use crate::check::excerpt;
use crate::chunk::{Chunk, MAX_FILE_CHARS, file_cost, floor_char_boundary};
use crate::{Finding, SourceFile, estimate_tokens};

pub const REVIEW_INSTRUCTIONS: &str = include_str!("../prompts/review.md");
pub const VERIFY_INSTRUCTIONS: &str = include_str!("../prompts/verify.md");

/// A prompt as a (system, user) pair; the capability turns it into chat turns.
#[derive(Debug, Clone, PartialEq)]
pub struct Prompt {
    pub system: String,
    pub user: String,
}

/// Numbered file text, cut at [`MAX_FILE_CHARS`] with a note saying so.
pub fn numbered(content: &str) -> String {
    let cut = floor_char_boundary(content, MAX_FILE_CHARS);
    let mut out: String = content[..cut]
        .lines()
        .enumerate()
        .map(|(i, l)| format!("{:>5}| {l}", i + 1))
        .collect::<Vec<_>>()
        .join("\n");
    if cut < content.len() {
        out.push_str("\n  ...| (file cut here: too long to show in full)");
    }
    out
}

fn render_file(f: &SourceFile, role: &str, out: &mut String) {
    out.push_str(&format!("\n=== FILE: {} ({role}) ===\n", f.display_name()));
    if let Some(diff) = f.diff.as_deref().filter(|d| !d.trim().is_empty()) {
        out.push_str("--- what changed (unified diff) ---\n");
        out.push_str(diff.trim_end());
        out.push_str("\n--- full file as it is now ---\n");
    }
    out.push_str(&numbered(&f.content));
    out.push('\n');
}

/// The review prompt for one chunk. `scope` says what the review covers, e.g.
/// "changes from a1b2c3d to e4f5a6b" or "the src/services folder".
pub fn review_prompt(repo: &str, scope: &str, chunk: &Chunk, files: &[SourceFile]) -> Prompt {
    let mut user = format!("Repository: {repo}\nThis review covers: {scope}\n");
    for &t in &chunk.targets {
        render_file(&files[t], "UNDER REVIEW", &mut user);
    }
    for &c in &chunk.context {
        render_file(&files[c], "CONTEXT", &mut user);
    }
    user.push_str("\nReport the real bugs in the files UNDER REVIEW as a JSON array.");
    Prompt {
        system: REVIEW_INSTRUCTIONS.to_string(),
        user,
    }
}

/// The check prompt for one finding, kept within `budget` tokens: the whole
/// file when it fits in half the budget, otherwise the 120 lines either side
/// of the finding; then whichever `extra` files (what it imports) still fit.
/// Checks go to the smaller machines, which also answer the lights, so they
/// are kept short.
pub fn verify_prompt(
    finding: &Finding,
    file: &SourceFile,
    extra: &[&SourceFile],
    budget: usize,
) -> Prompt {
    let mut user = format!(
        "The finding:\n- location: {}\n- severity: {}\n- title: {}\n- quoted code: {}\n- what goes wrong: {}\n- suggested fix: {}\n",
        finding.location(),
        finding.severity.label(),
        finding.title,
        finding.quote,
        finding.scenario,
        finding.fix,
    );
    if file_cost(file) <= budget / 2 {
        render_file(file, "the file the finding is in", &mut user);
    } else {
        user.push_str(&format!(
            "\n=== FILE: {} (the lines around the finding) ===\n{}\n",
            file.display_name(),
            excerpt(&file.content, finding.line, 120)
        ));
    }
    for f in extra {
        if estimate_tokens(&user) + file_cost(f) > budget {
            continue;
        }
        render_file(f, "CONTEXT", &mut user);
    }
    user.push_str("\nIs this finding a real bug? Answer with the JSON object only.");
    Prompt {
        system: VERIFY_INSTRUCTIONS.to_string(),
        user,
    }
}

impl Prompt {
    /// Rough size of the whole prompt in tokens.
    pub fn tokens(&self) -> usize {
        estimate_tokens(&self.system) + estimate_tokens(&self.user)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Severity, chunk::plan_chunks};

    fn file(path: &str, content: &str, target: bool) -> SourceFile {
        SourceFile {
            repo: "dashboard".into(),
            path: path.into(),
            content: content.into(),
            diff: None,
            target,
        }
    }

    #[test]
    fn review_prompt_marks_targets_and_context_and_numbers_lines() {
        let files = vec![
            file("src/a.ts", "import './b';\nconst x = 1;", true),
            file("src/b.ts", "export const b = 2;", false),
        ];
        let chunks = plan_chunks(&files, &[], 10_000);
        let p = review_prompt("dashboard", "changes from aaa to bbb", &chunks[0], &files);
        assert!(p.system.contains("JSON array"));
        assert!(
            p.user
                .contains("=== FILE: dashboard/src/a.ts (UNDER REVIEW) ===")
        );
        assert!(
            p.user
                .contains("=== FILE: dashboard/src/b.ts (CONTEXT) ===")
        );
        assert!(p.user.contains("    2| const x = 1;"));
        assert!(p.user.contains("changes from aaa to bbb"));
    }

    #[test]
    fn a_diff_is_shown_before_the_full_file() {
        let mut f = file("src/a.ts", "const x = 2;", true);
        f.diff = Some("-const x = 1;\n+const x = 2;".into());
        let files = vec![f];
        let chunks = plan_chunks(&files, &[], 10_000);
        let p = review_prompt("d", "s", &chunks[0], &files);
        let diff_at = p.user.find("what changed").unwrap();
        let full_at = p.user.find("full file as it is now").unwrap();
        assert!(diff_at < full_at);
    }

    #[test]
    fn long_files_are_cut_with_a_note() {
        let text = "a\n".repeat(MAX_FILE_CHARS);
        assert!(numbered(&text).ends_with("(file cut here: too long to show in full)"));
    }

    #[test]
    fn verify_prompt_carries_the_finding() {
        let f = file("src/a.ts", "const agreed = pricing?.amount;", true);
        let finding = Finding {
            id: "f1".into(),
            repo: "dashboard".into(),
            path: "src/a.ts".into(),
            line: 1,
            severity: Severity::High,
            title: "Bills the cheapest option".into(),
            quote: "const agreed = pricing?.amount;".into(),
            scenario: "s".into(),
            fix: "x".into(),
            found_by: vec!["qwen3-coder@mac1".into()],
            verdict: None,
            verdict_reason: None,
            checked_by: None,
        };
        let p = verify_prompt(&finding, &f, &[], 8_000);
        assert!(p.tokens() > 0);
        assert!(p.user.contains("dashboard/src/a.ts:1"));
        assert!(p.user.contains("Bills the cheapest option"));
        assert!(p.system.contains("\"verdict\""));
    }

    #[test]
    fn verify_prompt_falls_back_to_an_excerpt_for_big_files() {
        let mut content = String::new();
        for i in 1..=2000 {
            content.push_str(&format!("const line{i} = {i};\n"));
        }
        let f = file("src/big.ts", &content, true);
        let finding = Finding {
            id: "f".into(),
            repo: "dashboard".into(),
            path: "src/big.ts".into(),
            line: 1000,
            severity: Severity::Medium,
            title: "t".into(),
            quote: "const line1000 = 1000;".into(),
            scenario: String::new(),
            fix: String::new(),
            found_by: vec![],
            verdict: None,
            verdict_reason: None,
            checked_by: None,
        };
        let p = verify_prompt(&finding, &f, &[], 4_000);
        assert!(p.user.contains(" 1000| const line1000 = 1000;"));
        assert!(!p.user.contains("const line1 = 1;"));
        assert!(p.tokens() < 6_000);
    }
}
