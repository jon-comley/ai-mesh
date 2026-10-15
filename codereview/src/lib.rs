//! Code review logic for the mesh's review capability (runs on mac1).
//!
//! Everything here is pure: the capability hands in file contents and model
//! replies, and gets back chunks, prompts, findings, task assignments and the
//! report. That keeps the parts that decide *what a review says* testable
//! without git, a model or a clock — the same split `ebay` makes.
//!
//! The pipeline, in order:
//! 1. [`chunk::plan_chunks`] groups the files under review into feature-sized
//!    chunks, each carrying the files it imports as context.
//! 2. [`prompt::review_prompt`] asks a worker for findings on one chunk;
//!    [`parse::parse_findings`] reads the reply.
//! 3. [`check::locate_quote`] drops any finding whose quoted code is not in the
//!    file, and corrects its line number when it is.
//! 4. [`prompt::verify_prompt`] asks a *different* model to confirm each one;
//!    [`parse::parse_verdict`] reads that.
//! 5. [`dedupe::dedupe`] merges repeats, and [`report::render`] writes the
//!    Markdown report.
//!
//! [`assign::assign`] decides which idle worker takes which task.

pub mod ask;
pub mod assign;
pub mod check;
pub mod chunk;
pub mod dedupe;
pub mod parse;
pub mod prompt;
pub mod report;

use serde::{Deserialize, Serialize};

/// One file handed to the planner.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceFile {
    /// The repo it came from (`dashboard`, `guv`…). Context files can come
    /// from a companion repo, so the repo is part of a file's identity.
    pub repo: String,
    /// Path inside the repo, forward slashes.
    pub path: String,
    pub content: String,
    /// The change under review, as a unified diff, when this is a nightly run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    /// True for files under review; false for files included only as context.
    pub target: bool,
}

impl SourceFile {
    /// `repo/path`, the name used in prompts and in the report.
    pub fn display_name(&self) -> String {
        format!("{}/{}", self.repo, self.path)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    High,
    Medium,
    Low,
}

impl Severity {
    pub fn parse(s: &str) -> Severity {
        match s.trim().to_ascii_lowercase().as_str() {
            "high" | "critical" | "blocker" | "major" => Severity::High,
            "low" | "minor" | "nit" | "trivial" | "info" => Severity::Low,
            _ => Severity::Medium,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Severity::High => "High",
            Severity::Medium => "Medium",
            Severity::Low => "Low",
        }
    }
}

/// What the checking model said about a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Confirmed,
    Rejected,
    Unsure,
}

/// A finding as the reviewing model reported it, before any checking.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawFinding {
    pub file: String,
    pub line: u32,
    pub severity: Severity,
    pub title: String,
    pub quote: String,
    pub scenario: String,
    pub fix: String,
}

/// A finding that passed the quote check, with who found and who checked it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    pub id: String,
    pub repo: String,
    /// Path inside `repo`.
    pub path: String,
    /// Line number, corrected to where the quote actually is.
    pub line: u32,
    pub severity: Severity,
    pub title: String,
    pub quote: String,
    pub scenario: String,
    pub fix: String,
    /// `model@node` of each reviewer that reported it (more than one after a merge).
    pub found_by: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<Verdict>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict_reason: Option<String>,
    /// `model@node` of the checker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_by: Option<String>,
}

impl Finding {
    pub fn location(&self) -> String {
        format!("{}/{}:{}", self.repo, self.path, self.line)
    }
}

/// Rough token count for budgeting. Code averages about 3.5 characters a
/// token on the Qwen tokenizers; rounding up keeps chunks safely under the
/// context rather than exactly at it.
pub fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(7) * 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_parse_maps_synonyms() {
        assert_eq!(Severity::parse("HIGH"), Severity::High);
        assert_eq!(Severity::parse("critical"), Severity::High);
        assert_eq!(Severity::parse("nit"), Severity::Low);
        assert_eq!(Severity::parse("medium"), Severity::Medium);
        assert_eq!(Severity::parse("whatever"), Severity::Medium);
    }

    #[test]
    fn estimate_tokens_is_about_chars_over_three_and_a_half() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens(&"x".repeat(7000)), 2000);
    }

    #[test]
    fn severity_orders_high_first() {
        let mut v = vec![Severity::Low, Severity::High, Severity::Medium];
        v.sort();
        assert_eq!(v, vec![Severity::High, Severity::Medium, Severity::Low]);
    }
}
