//! Wire types for review work (wire v13).
//!
//! Two halves:
//! * **Work inference** lets an agent (mac1, which runs the reviews) ask the
//!   coordinator to run a completion on a chosen machine. The coordinator stays
//!   the only router, because only it sees every machine's busy state, and it
//!   gives home commands priority: a work request is paused (cancelled and
//!   reported as [`WorkOutcome::Preempted`]) when a home command needs its
//!   machine.
//! * **Review state** is mac1's view of the reviews — repos, runs, findings —
//!   pushed to the coordinator for the dashboard, and the commands the
//!   dashboard sends back.

use crate::ChatTurn;
use serde::{Deserialize, Serialize};

/// Agent → coordinator: run one completion as background work.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkInferenceRequest {
    pub request_id: String,
    /// The machine to run it on. `None` lets the coordinator pick an idle
    /// machine with a work model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    /// The model to use. `None` means whatever work model the machine has ready.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    pub messages: Vec<ChatTurn>,
    pub max_tokens: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkOutcome {
    Finished,
    /// Stopped so a home command could use the machine. Send it again later.
    Preempted,
    Failed {
        reason: String,
    },
    /// Nothing could take it: the machine is gone, busy with a home command,
    /// resting, or has no work model ready.
    NoWorker {
        reason: String,
    },
}

/// Coordinator → agent: how a [`WorkInferenceRequest`] ended.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkInferenceDone {
    pub request_id: String,
    /// The machine that served it (empty when none did).
    #[serde(default)]
    pub node_id: String,
    #[serde(default)]
    pub model_name: String,
    pub outcome: WorkOutcome,
    #[serde(default)]
    pub output: String,
    #[serde(default)]
    pub prompt_tokens: u32,
    #[serde(default)]
    pub tokens_generated: u32,
    /// Wall time from dispatch to the last token, measured by the coordinator.
    #[serde(default)]
    pub duration_ms: u64,
}

/// One machine as a possible worker.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkerInfo {
    pub node_id: String,
    pub hostname: String,
    /// The model it has ready.
    pub model_name: String,
    /// Its model server's context, in tokens, when the agent reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctx_size: Option<u32>,
    /// The model may answer home commands.
    pub control: bool,
    /// The model may do review work.
    pub work: bool,
    /// Something is running on it now.
    pub busy: bool,
    /// Resting after a work request was paused for a home command.
    pub resting: bool,
}

/// Coordinator → agent: the machines that could take work right now.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct WorkerSnapshot {
    pub workers: Vec<WorkerInfo>,
}

/// An import prefix that points into another repo (dashboard's `@app/` → guv's `src/`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReviewAlias {
    pub prefix: String,
    pub repo: String,
    pub dir: String,
}

/// A repo mac1 reviews, as the dashboard edits it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReviewRepoSpec {
    /// Short name, used in reports and as the clone's folder name.
    pub name: String,
    /// `git@github-<alias>:owner/repo.git` (a deploy-key host alias) or
    /// `https://github.com/owner/repo`.
    pub url: String,
    #[serde(default = "default_branch")]
    pub branch: String,
    /// Nightly review times, minutes since local midnight.
    #[serde(default)]
    pub timeslots: Vec<u16>,
    /// Weekly sweep day, 0 = Monday … 6 = Sunday. `None` = no sweep.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sweep_day: Option<u8>,
    /// Weekly sweep time, minutes since local midnight.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sweep_slot: Option<u16>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub aliases: Vec<ReviewAlias>,
}

fn default_branch() -> String {
    "main".into()
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReviewRepoView {
    #[serde(flatten)]
    pub spec: ReviewRepoSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reviewed_commit: Option<String>,
    /// The folder the last weekly sweep covered; the next sweep takes the one after.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_sweep_folder: Option<String>,
}

/// One task running right now, for the live view.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReviewTaskView {
    /// "review" or "check".
    pub kind: String,
    pub node_id: String,
    /// `model@hostname`.
    pub worker: String,
    /// What it is looking at, e.g. "src/pages/JobDetailPage.tsx +3 files".
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ReviewCounts {
    pub high: u32,
    pub medium: u32,
    pub low: u32,
    pub unconfirmed: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReviewRunView {
    pub id: i64,
    pub repo: String,
    /// "nightly", "sweep" or "manual".
    pub kind: String,
    pub scope: String,
    /// "queued", "running", "done", "nothing_new" or "failed".
    pub status: String,
    pub started_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<u64>,
    pub tasks_total: u32,
    pub tasks_done: u32,
    #[serde(default)]
    pub running: Vec<ReviewTaskView>,
    #[serde(default)]
    pub counts: ReviewCounts,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReviewFindingView {
    pub id: String,
    pub run_id: i64,
    pub repo: String,
    pub path: String,
    pub line: u32,
    /// "high", "medium" or "low".
    pub severity: String,
    pub title: String,
    pub quote: String,
    pub scenario: String,
    pub fix: String,
    /// "confirmed", "unsure" or absent (not checked). Rejected findings are not sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_by: Option<String>,
    #[serde(default)]
    pub found_by: Vec<String>,
    /// "open", "dismissed" or "fixed".
    pub status: String,
    pub first_seen: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ReviewSettingsView {
    pub ntfy_topic_set: bool,
    /// Last few characters of the topic, so it can be recognised but not copied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ntfy_hint: Option<String>,
    pub max_review_tokens: u32,
    pub evening_max_tokens: u32,
    /// GitHub owners whose repos may be added (empty = any).
    #[serde(default)]
    pub allowed_owners: Vec<String>,
}

/// mac1 → coordinator: the whole review state, sent whenever it changes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ReviewSnapshot {
    pub node_id: String,
    pub hostname: String,
    pub generated_at: u64,
    pub repos: Vec<ReviewRepoView>,
    #[serde(default)]
    pub runs: Vec<ReviewRunView>,
    /// Open findings, newest runs first, capped so the message stays small.
    #[serde(default)]
    pub findings: Vec<ReviewFindingView>,
    #[serde(default)]
    pub settings: ReviewSettingsView,
    /// The last thing that went wrong with a dashboard command (a refused repo
    /// URL, say), shown once in the tab.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
    /// Recent questions, newest first.
    #[serde(default)]
    pub questions: Vec<ReviewQuestionView>,
}

/// A question about a repo and, once ready, its answer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReviewQuestionView {
    pub id: String,
    pub repo: String,
    pub question: String,
    /// "waiting", "thinking", "answered" or "failed".
    pub status: String,
    /// Markdown, citing `repo/path:line`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    /// The files the model was shown.
    #[serde(default)]
    pub sources: Vec<String>,
    /// `model@hostname` that answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub asked_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered_at: Option<u64>,
}

/// Coordinator → mac1: something the dashboard asked for.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum ReviewCommand {
    RunNow {
        repo: String,
        /// Review the whole repo, one folder at a time, instead of new commits.
        #[serde(default)]
        sweep: bool,
        /// Review everything under this folder or file instead.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        /// Review what this branch changes compared with the repo's main branch.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
    },
    /// Answer a question about a repo's code. The answer arrives in the
    /// snapshot's `questions`.
    Ask {
        id: String,
        repo: String,
        question: String,
    },
    UpsertRepo {
        spec: ReviewRepoSpec,
    },
    RemoveRepo {
        name: String,
    },
    SetFindingStatus {
        id: String,
        status: String,
    },
    SetSettings {
        /// Empty string clears it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ntfy_topic_url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_review_tokens: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        evening_max_tokens: Option<u32>,
    },
    RequestSnapshot,
    FetchReport {
        request_id: String,
        run_id: i64,
    },
}

/// mac1 → coordinator: the answer to [`ReviewCommand::FetchReport`], or to a
/// command that failed (`markdown: None`, `error` set).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReviewReply {
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub markdown: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MeshMessage;

    fn roundtrip(msg: MeshMessage) {
        let json = serde_json::to_string(&msg).unwrap();
        let back: MeshMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn work_messages_roundtrip() {
        roundtrip(MeshMessage::WorkInferenceRequest(WorkInferenceRequest {
            request_id: "review-1".into(),
            node_id: Some("mac1".into()),
            model_name: None,
            messages: vec![ChatTurn::system("s"), ChatTurn::user("u")],
            max_tokens: 4096,
            temperature: Some(0.1),
        }));
        roundtrip(MeshMessage::WorkInferenceDone(WorkInferenceDone {
            request_id: "review-1".into(),
            node_id: "mac1".into(),
            model_name: "qwen3-coder".into(),
            outcome: WorkOutcome::Preempted,
            output: String::new(),
            prompt_tokens: 0,
            tokens_generated: 0,
            duration_ms: 12,
        }));
        roundtrip(MeshMessage::RequestWorkers);
        roundtrip(MeshMessage::WorkerSnapshot(WorkerSnapshot {
            workers: vec![WorkerInfo {
                node_id: "beelink1".into(),
                hostname: "beelink1".into(),
                model_name: "qwen2.5:7b".into(),
                ctx_size: Some(32768),
                control: true,
                work: true,
                busy: false,
                resting: false,
            }],
        }));
    }

    #[test]
    fn outcome_wire_shape_is_tagged() {
        let json = serde_json::to_value(WorkOutcome::Failed { reason: "x".into() }).unwrap();
        assert_eq!(json, serde_json::json!({"kind": "failed", "reason": "x"}));
    }

    #[test]
    fn review_messages_roundtrip() {
        roundtrip(MeshMessage::ReviewCommand(ReviewCommand::RunNow {
            repo: "dashboard".into(),
            sweep: false,
            path: None,
            branch: Some("feature-x".into()),
        }));
        roundtrip(MeshMessage::ReviewCommand(ReviewCommand::Ask {
            id: "q1".into(),
            repo: "dashboard".into(),
            question: "Where is the invoice total worked out?".into(),
        }));
        roundtrip(MeshMessage::ReviewCommand(ReviewCommand::SetSettings {
            ntfy_topic_url: Some(String::new()),
            max_review_tokens: None,
            evening_max_tokens: Some(32_000),
        }));
        roundtrip(MeshMessage::ReviewReply(ReviewReply {
            request_id: "r".into(),
            markdown: Some("# Code review".into()),
            error: None,
        }));
        roundtrip(MeshMessage::ReviewSnapshot(Box::new(ReviewSnapshot {
            node_id: "mac1".into(),
            hostname: "mac1".into(),
            generated_at: 1,
            repos: vec![ReviewRepoView {
                spec: ReviewRepoSpec {
                    name: "dashboard".into(),
                    url: "git@github-dashboard:jon-comley/dashboard.git".into(),
                    branch: "main".into(),
                    timeslots: vec![135],
                    sweep_day: Some(6),
                    sweep_slot: Some(195),
                    enabled: true,
                    aliases: vec![ReviewAlias {
                        prefix: "@app/".into(),
                        repo: "guv".into(),
                        dir: "src".into(),
                    }],
                },
                last_reviewed_commit: Some("cdcf9e8".into()),
                last_sweep_folder: None,
            }],
            ..Default::default()
        })));
    }

    #[test]
    fn repo_spec_defaults_fill_in() {
        let spec: ReviewRepoSpec =
            serde_json::from_str(r#"{"name":"guv","url":"https://github.com/jon-comley/guv"}"#)
                .unwrap();
        assert_eq!(spec.branch, "main");
        assert!(spec.enabled);
        assert!(spec.timeslots.is_empty());
    }
}
