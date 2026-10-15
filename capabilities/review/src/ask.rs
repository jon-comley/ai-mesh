//! Answering a question about a repo.
//!
//! Search the repo for the question's words (`git grep`), rank the files
//! (`codereview::ask`), show the best of them to the free machine with the
//! largest context, and return its answer with the files it was shown.
//! A question is one task; like review work it gives way to home commands,
//! and it is retried when paused.

use crate::files;
use crate::git::Repo;
use crate::run::Ctx;
use codereview::SourceFile;
use codereview::assign::{self, TaskKind};
use shared::{ChatTurn, WorkInferenceRequest, WorkOutcome};
use std::collections::HashMap;
use std::time::Instant;

#[derive(Debug, Clone, PartialEq)]
pub struct Answer {
    pub text: String,
    /// `repo/path` of each file shown.
    pub sources: Vec<String>,
    /// `model@hostname`, or `None` when no model was needed.
    pub worker: Option<String>,
}

const MAX_PAUSES: u32 = 5;
const MAX_FAILURES: u32 = 2;
const ANSWER_MAX_TOKENS: u32 = 2048;
/// Ranked files read from git before fitting them to the budget.
const READ_LIMIT: usize = 80;

/// Files a question may be answered from: code, plus Markdown docs, which
/// often hold the "why".
fn askable(path: &str) -> bool {
    files::is_context_candidate(path) || path.ends_with(".md")
}

pub async fn answer(ctx: &Ctx, repo_name: &str, question: &str) -> Result<Answer, String> {
    let row = ctx
        .store
        .lock()
        .unwrap()
        .repo(repo_name)
        .ok_or_else(|| format!("'{repo_name}' is not in the review list"))?;
    if ctx.limits.check_urls {
        files::validate_repo_url(&row.spec.url, &ctx.limits.allowed_owners)?;
    }
    let repo = Repo::sync(
        &ctx.home.join("repos").join(&row.spec.name),
        &row.spec.url,
        &row.spec.branch,
    )
    .await?;
    let head = repo.head().await?;
    let paths: Vec<String> = repo
        .files(&head)
        .await?
        .into_iter()
        .filter(|p| askable(p))
        .collect();

    let terms = codereview::ask::search_terms(question);
    if terms.is_empty() {
        return Ok(Answer {
            text: "That question has no words to search the code for. Try naming the \
                   feature, a function, a file or an error message."
                .into(),
            sources: Vec::new(),
            worker: None,
        });
    }
    let mut hits: HashMap<String, HashMap<String, u32>> = HashMap::new();
    for term in &terms {
        let found: HashMap<String, u32> = repo
            .grep_count(&head, term)
            .await?
            .into_iter()
            .filter(|(p, _)| askable(p))
            .collect();
        hits.insert(term.clone(), found);
    }
    let ranked = codereview::ask::rank_files(&terms, &hits, &paths);
    if ranked.is_empty() {
        return Ok(Answer {
            text: format!(
                "No file in {repo_name} mentions any of: {}. Try other words, such as a \
                 function or file name.",
                terms.join(", ")
            ),
            sources: Vec::new(),
            worker: None,
        });
    }

    let mut candidates = Vec::new();
    for (path, _) in ranked.iter().take(READ_LIMIT) {
        if let Ok(Some(content)) = repo.read(&head, path).await
            && content.len() <= files::MAX_TARGET_BYTES
        {
            candidates.push(SourceFile {
                repo: row.spec.name.clone(),
                path: path.clone(),
                content,
                diff: None,
                target: false,
            });
        }
    }

    let mut pauses = 0;
    let mut failures = 0;
    let started = Instant::now();
    loop {
        // The free machine with the most room.
        let workers = ctx.workers().await;
        let Some(w) = workers
            .iter()
            .filter(|w| !w.busy && !w.resting)
            .max_by_key(|w| assign::capacity(w, TaskKind::Review))
            .cloned()
        else {
            if started.elapsed() >= ctx.limits.stall {
                return Err("no machine with a work model came free".into());
            }
            tokio::time::sleep(ctx.limits.poll).await;
            continue;
        };
        let budget = assign::capacity(&w, TaskKind::Review).min(ctx.limits.max_review_tokens);
        let files = codereview::ask::fit_files(
            candidates.clone(),
            codereview::ask::file_budget(budget, question),
        );
        if files.is_empty() {
            return Err("the matching files are too big for any machine's context".into());
        }
        let prompt = codereview::ask::ask_prompt(&row.spec.name, question, &files);
        let done = ctx
            .mesh
            .infer(WorkInferenceRequest {
                request_id: format!("ask-{}", uuid::Uuid::new_v4()),
                node_id: Some(w.node_id.clone()),
                model_name: Some(w.model.clone()),
                messages: vec![ChatTurn::system(prompt.system), ChatTurn::user(prompt.user)],
                max_tokens: ANSWER_MAX_TOKENS,
                temperature: Some(0.2),
            })
            .await;
        match done.outcome {
            WorkOutcome::Finished => {
                let text = done.output.trim().to_string();
                if text.is_empty() {
                    failures += 1;
                    if failures > MAX_FAILURES {
                        return Err("the model gave an empty answer".into());
                    }
                    continue;
                }
                return Ok(Answer {
                    text,
                    sources: files.iter().map(|f| f.display_name()).collect(),
                    worker: Some(w.label()),
                });
            }
            WorkOutcome::Preempted => {
                pauses += 1;
                if pauses > MAX_PAUSES {
                    return Err("paused too often for home commands; ask again later".into());
                }
            }
            WorkOutcome::NoWorker { .. } => tokio::time::sleep(ctx.limits.poll).await,
            WorkOutcome::Failed { reason } => {
                failures += 1;
                if failures > MAX_FAILURES {
                    return Err(reason);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run::{Limits, Mesh, Progress};
    use crate::store::Store;
    use async_trait::async_trait;
    use shared::{ReviewRepoSpec, WorkInferenceDone, WorkerInfo};
    use std::path::Path;
    use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tokio::sync::Notify;

    struct AskMesh {
        calls: AtomicU32,
        pause_first: bool,
        prompt: Mutex<String>,
    }

    #[async_trait]
    impl Mesh for AskMesh {
        async fn workers(&self) -> Result<Vec<WorkerInfo>, String> {
            Ok(vec![
                WorkerInfo {
                    node_id: "beelink1".into(),
                    hostname: "beelink1".into(),
                    model_name: "qwen2.5:7b".into(),
                    ctx_size: Some(32_768),
                    control: true,
                    work: true,
                    busy: false,
                    resting: false,
                },
                WorkerInfo {
                    node_id: "mac1".into(),
                    hostname: "mac1".into(),
                    model_name: "qwen3-coder:30b".into(),
                    ctx_size: Some(262_144),
                    control: true,
                    work: true,
                    busy: false,
                    resting: false,
                },
            ])
        }

        async fn infer(&self, req: WorkInferenceRequest) -> WorkInferenceDone {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            *self.prompt.lock().unwrap() = req.messages[1].content.clone();
            let outcome = if self.pause_first && n == 0 {
                WorkOutcome::Preempted
            } else {
                WorkOutcome::Finished
            };
            WorkInferenceDone {
                request_id: req.request_id,
                node_id: req.node_id.unwrap_or_default(),
                model_name: req.model_name.unwrap_or_default(),
                outcome,
                output: "The total is worked out in `app/src/invoice.ts:2`.".into(),
                prompt_tokens: 100,
                tokens_generated: 20,
                duration_ms: 5,
            }
        }
    }

    fn sh(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn setup(pause_first: bool) -> (tempfile::TempDir, Ctx, Arc<AskMesh>) {
        let tmp = tempfile::tempdir().unwrap();
        let origin = tmp.path().join("origin");
        std::fs::create_dir_all(origin.join("src")).unwrap();
        sh(&origin, &["init", "-q", "-b", "main"]);
        std::fs::write(
            origin.join("src/invoice.ts"),
            "export function invoiceTotal(lines) {\n  return lines.reduce((t, l) => t + l.net + l.vat, 0);\n}\n",
        )
        .unwrap();
        std::fs::write(origin.join("src/other.ts"), "export const unrelated = 1;\n").unwrap();
        std::fs::write(origin.join("logo.png"), [0u8, 1, 2]).unwrap();
        sh(&origin, &["add", "."]);
        sh(&origin, &["commit", "-qm", "init"]);
        let store = Store::open_in_memory().unwrap();
        store.upsert_repo(&ReviewRepoSpec {
            name: "app".into(),
            url: origin.to_string_lossy().into_owned(),
            branch: "main".into(),
            timeslots: vec![],
            sweep_day: None,
            sweep_slot: None,
            enabled: true,
            aliases: vec![],
        });
        let mesh = Arc::new(AskMesh {
            calls: AtomicU32::new(0),
            pause_first,
            prompt: Mutex::new(String::new()),
        });
        let ctx = Ctx {
            store: Arc::new(Mutex::new(store)),
            home: tmp.path().join("home"),
            mesh: mesh.clone(),
            progress: Arc::new(Mutex::new(Progress::default())),
            changed: Arc::new(Notify::new()),
            speeds: Arc::new(Mutex::new(HashMap::new())),
            questions_waiting: Arc::new(AtomicUsize::new(0)),
            limits: Limits {
                poll: Duration::from_millis(10),
                stall: Duration::from_millis(200),
                check_urls: false,
                ..Limits::default()
            },
            now: || chrono::Local::now().naive_local(),
        };
        (tmp, ctx, mesh)
    }

    #[tokio::test]
    async fn a_question_is_answered_from_the_matching_files_on_the_biggest_machine() {
        let (_tmp, ctx, mesh) = setup(false);
        let a = answer(&ctx, "app", "Where is the invoice total worked out?")
            .await
            .unwrap();
        assert!(a.text.contains("app/src/invoice.ts:2"));
        assert_eq!(a.worker.as_deref(), Some("qwen3-coder:30b@mac1"));
        assert_eq!(a.sources[0], "app/src/invoice.ts");
        assert!(!a.sources.contains(&"app/src/other.ts".to_string()));
        let prompt = mesh.prompt.lock().unwrap().clone();
        assert!(prompt.contains("    2|   return lines.reduce"));
        assert!(prompt.contains("The question: Where is the invoice total worked out?"));
    }

    #[tokio::test]
    async fn a_paused_question_is_asked_again() {
        let (_tmp, ctx, mesh) = setup(true);
        let a = answer(&ctx, "app", "invoiceTotal?").await.unwrap();
        assert_eq!(mesh.calls.load(Ordering::SeqCst), 2);
        assert!(a.worker.is_some());
    }

    #[tokio::test]
    async fn no_match_is_said_plainly_without_asking_a_model() {
        let (_tmp, ctx, mesh) = setup(false);
        let a = answer(&ctx, "app", "Where do we handle zigbee pairing?")
            .await
            .unwrap();
        assert!(a.text.contains("No file in app mentions"));
        assert!(a.worker.is_none());
        assert_eq!(mesh.calls.load(Ordering::SeqCst), 0);
        assert!(answer(&ctx, "nope", "x").await.is_err());
    }
}
