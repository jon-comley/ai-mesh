//! One review run, start to finish: fetch, choose the files, plan chunks, hand
//! tasks to machines across the mesh, check findings with a second model,
//! write the report.
//!
//! The mesh is behind the [`Mesh`] trait so a whole run can be tested with a
//! fake: the real one sends `WorkInferenceRequest`s through the coordinator.

use crate::files;
use crate::git::Repo;
use crate::store::{RunUpdate, Store};
use async_trait::async_trait;
use codereview::assign::{self, TaskKind, TaskNeed, Worker};
use codereview::chunk::{Chunk, ImportAlias, plan_chunks, resolve_imports};
use codereview::report::{RunInfo, headline, render};
use codereview::{Finding, SourceFile, Verdict};
use shared::{
    ChatTurn, ReviewCounts, ReviewTaskView, WorkInferenceDone, WorkInferenceRequest, WorkOutcome,
    WorkerInfo,
};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{Notify, mpsc};
use tracing::{info, warn};

/// What the run needs from the mesh.
#[async_trait]
pub trait Mesh: Send + Sync {
    /// The machines that could take work right now.
    async fn workers(&self) -> Result<Vec<WorkerInfo>, String>;
    /// Run one task on a machine and wait for it to end.
    async fn infer(&self, req: WorkInferenceRequest) -> WorkInferenceDone;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunKind {
    Nightly,
    Sweep,
    /// "Run now" from the dashboard: new commits since the last review.
    Manual,
    /// On demand: everything under one folder or file.
    Path(String),
    /// On demand: what a branch changes compared with the repo's main branch.
    Branch(String),
}

impl RunKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            RunKind::Nightly => "nightly",
            RunKind::Sweep => "sweep",
            RunKind::Manual => "manual",
            RunKind::Path(_) => "path",
            RunKind::Branch(_) => "branch",
        }
    }

    /// Back from the database after a restart. Path and branch runs are not
    /// queued again: they were asked for once, by hand.
    pub fn parse(s: &str) -> Option<RunKind> {
        match s {
            "nightly" => Some(RunKind::Nightly),
            "sweep" => Some(RunKind::Sweep),
            "manual" => Some(RunKind::Manual),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RunRequest {
    pub repo: String,
    pub kind: RunKind,
}

/// The active run, for the live view.
#[derive(Debug, Default)]
pub struct Progress {
    pub run_id: Option<i64>,
    pub running: BTreeMap<u64, ReviewTaskView>,
    pub total: u32,
    pub done: u32,
}

/// Limits that come from settings.
#[derive(Debug, Clone)]
pub struct Limits {
    pub max_review_tokens: usize,
    pub evening_max_tokens: usize,
    pub allowed_owners: Vec<String>,
    /// How many commits back a repo's first nightly run looks.
    pub first_run_commits: u32,
    /// Give up on waiting tasks when nothing could be handed out for this long.
    pub stall: Duration,
    /// How often to ask for the worker snapshot while tasks are waiting.
    pub poll: Duration,
    /// Refuse repo URLs that are not GitHub. Only tests turn this off, to
    /// clone from a local folder.
    pub check_urls: bool,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            // Reading slows steeply with length on mac1 (qwen3-coder:30b,
            // 2026-10-10): 32k in ~100 s, 100k in over 15 minutes. Bigger
            // tasks also outlast any pause-and-retry, so keep them small.
            max_review_tokens: 32_000,
            evening_max_tokens: 16_000,
            allowed_owners: Vec::new(),
            first_run_commits: 20,
            stall: Duration::from_secs(20 * 60),
            poll: Duration::from_secs(5),
            check_urls: true,
        }
    }
}

pub struct Ctx {
    pub store: Arc<Mutex<Store>>,
    /// `~/.ai-mesh/reviews`
    pub home: PathBuf,
    pub mesh: Arc<dyn Mesh>,
    pub progress: Arc<Mutex<Progress>>,
    /// Woken whenever something the dashboard shows has changed.
    pub changed: Arc<Notify>,
    /// Measured prompt-reading speed per node, tokens a second.
    pub speeds: Arc<Mutex<HashMap<String, f32>>>,
    pub limits: Limits,
    /// Local "now", injectable for tests.
    pub now: fn() -> chrono::NaiveDateTime,
    /// Questions waiting for an answer. While any are, a run hands out no new
    /// tasks, so a question is answered in minutes rather than after the run.
    pub questions_waiting: Arc<std::sync::atomic::AtomicUsize>,
}

pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn short(c: &str) -> &str {
    &c[..c.len().min(7)]
}

const MAX_ATTEMPTS_PAUSED: u32 = 5;
const MAX_ATTEMPTS_FAILED: u32 = 3;
const REVIEW_MAX_TOKENS_OUT: u32 = 4096;
const VERIFY_MAX_TOKENS_OUT: u32 = 512;
/// A check holds the finding, its file (or the lines round it) and what fits
/// of its imports. Kept small: checks run on the machines that answer lights.
const VERIFY_BUDGET: usize = 12_000;
/// Context files are read two imports deep, but never more than this many.
const MAX_CONTEXT_FILES: usize = 400;

#[derive(Debug, Clone)]
enum TState {
    Pending,
    Running,
    Done,
    GaveUp,
}

#[derive(Debug, Clone)]
struct Task {
    id: u64,
    kind: TaskKind,
    chunk: Option<usize>,
    finding: Option<usize>,
    tokens: usize,
    attempts: u32,
    state: TState,
    avoid_model: Option<String>,
    label: String,
}

/// What a run produced, for the caller's logs and tests.
#[derive(Debug, Clone, PartialEq)]
pub struct RunOutcome {
    pub run_id: i64,
    pub status: String,
    pub findings: Vec<Finding>,
    pub report_path: Option<PathBuf>,
}

impl Ctx {
    fn store<T>(&self, f: impl FnOnce(&Store) -> T) -> T {
        f(&self.store.lock().unwrap())
    }

    fn touch(&self) {
        self.changed.notify_one();
    }

    fn set_progress(&self, f: impl FnOnce(&mut Progress)) {
        f(&mut self.progress.lock().unwrap());
        self.touch();
    }

    pub(crate) async fn workers(&self) -> Vec<Worker> {
        let speeds = self.speeds.lock().unwrap().clone();
        match self.mesh.workers().await {
            Ok(ws) => ws
                .into_iter()
                .filter(|w| w.work)
                .map(|w| Worker {
                    prompt_tps: speeds.get(&w.node_id).copied(),
                    ctx_tokens: w.ctx_size.unwrap_or(4096) as usize,
                    node_id: w.node_id,
                    hostname: w.hostname,
                    model: w.model_name,
                    busy: w.busy,
                    resting: w.resting,
                })
                .collect(),
            Err(e) => {
                warn!(error = %e, "could not get the worker snapshot");
                Vec::new()
            }
        }
    }

    fn record_speed(&self, node: &str, done: &WorkInferenceDone) {
        if done.prompt_tokens < 2_000 || done.duration_ms == 0 {
            return;
        }
        // Includes generation time, so it under-reads: a safe direction.
        let tps = done.prompt_tokens as f32 / (done.duration_ms as f32 / 1000.0);
        let mut speeds = self.speeds.lock().unwrap();
        let e = speeds.entry(node.to_string()).or_insert(tps);
        *e = *e * 0.7 + tps * 0.3;
    }
}

/// Load `path` from `repo` at `rev` if it is text and not too big.
async fn load(repo: &Repo, rev: &str, path: &str) -> Option<String> {
    match repo.read(rev, path).await {
        Ok(Some(text)) if text.len() <= files::MAX_TARGET_BYTES => Some(text),
        _ => None,
    }
}

/// Run one review. Errors are recorded on the run and returned.
pub async fn execute(ctx: &Ctx, req: &RunRequest) -> Result<RunOutcome, String> {
    let now = unix_now();
    let run_id = ctx.store(|s| s.create_run(&req.repo, req.kind.as_str(), "", "running", now));
    ctx.set_progress(|p| {
        *p = Progress {
            run_id: Some(run_id),
            ..Default::default()
        }
    });
    let result = execute_inner(ctx, req, run_id).await;
    ctx.set_progress(|p| *p = Progress::default());
    match result {
        Ok(out) => Ok(out),
        Err(e) => {
            warn!(repo = %req.repo, error = %e, "review run failed");
            ctx.store(|s| {
                s.update_run(
                    run_id,
                    &RunUpdate {
                        status: Some("failed".into()),
                        error: Some(e.clone()),
                        finished_at: Some(unix_now()),
                        ..Default::default()
                    },
                )
            });
            ctx.touch();
            Err(e)
        }
    }
}

async fn execute_inner(ctx: &Ctx, req: &RunRequest, run_id: i64) -> Result<RunOutcome, String> {
    let row = ctx
        .store(|s| s.repo(&req.repo))
        .ok_or_else(|| format!("'{}' is not in the review list", req.repo))?;
    let spec = row.spec.clone();
    if ctx.limits.check_urls {
        files::validate_repo_url(&spec.url, &ctx.limits.allowed_owners)?;
    }
    let repos_dir = ctx.home.join("repos");
    let repo = Repo::sync(&repos_dir.join(&spec.name), &spec.url, &spec.branch).await?;
    let head = repo.head().await?;
    // What is reviewed: the main branch, or the branch asked for.
    let (rev, branch_base) = match &req.kind {
        RunKind::Branch(b) => {
            if !files::valid_branch(b) {
                return Err(format!("'{b}' is not a valid branch name"));
            }
            let bhead = repo.fetch_branch(b).await?;
            let base = repo.merge_base(&head, &bhead).await?;
            (bhead, Some(base))
        }
        _ => (head.clone(), None),
    };

    // Companion repos named by import aliases (dashboard → guv), for context.
    let mut companions: HashMap<String, (Repo, String)> = HashMap::new();
    for alias in &spec.aliases {
        if companions.contains_key(&alias.repo) || alias.repo == spec.name {
            continue;
        }
        let Some(other) = ctx.store(|s| s.repo(&alias.repo)) else {
            warn!(alias = %alias.repo, "alias names a repo that is not in the review list");
            continue;
        };
        if ctx.limits.check_urls
            && files::validate_repo_url(&other.spec.url, &ctx.limits.allowed_owners).is_err()
        {
            continue;
        }
        match Repo::sync(
            &repos_dir.join(&other.spec.name),
            &other.spec.url,
            &other.spec.branch,
        )
        .await
        {
            Ok(r) => {
                if let Ok(h) = r.head().await {
                    companions.insert(alias.repo.clone(), (r, h));
                }
            }
            Err(e) => warn!(alias = %alias.repo, error = %e, "could not fetch companion repo"),
        }
    }

    // ── what to review ──────────────────────────────────────────────────────
    let all_paths = repo.files(&rev).await?;
    let mut targets: Vec<SourceFile> = Vec::new();
    let mut sweep_folder = None;
    let scope = match &req.kind {
        RunKind::Nightly | RunKind::Manual => {
            let base = match row.last_reviewed_commit.as_deref() {
                Some(c) if repo.has_commit(c).await => c.to_string(),
                _ => {
                    repo.commit_before(&head, ctx.limits.first_run_commits)
                        .await?
                }
            };
            if base == head {
                return finish_quiet(ctx, run_id, "nothing new since the last review");
            }
            let changed = repo.changed_files(&base, &head).await?;
            for path in changed.iter().filter(|p| files::is_reviewable(p)) {
                if let Some(content) = load(&repo, &head, path).await {
                    let diff = repo.diff(&base, &head, path).await.ok();
                    targets.push(SourceFile {
                        repo: spec.name.clone(),
                        path: path.clone(),
                        content,
                        diff,
                        target: true,
                    });
                }
            }
            let n = repo.commit_count(&base, &head).await.unwrap_or(0);
            let label = if req.kind == RunKind::Manual {
                "Run now"
            } else {
                "Nightly"
            };
            format!(
                "{label}: {n} new commit{} ({}..{})",
                if n == 1 { "" } else { "s" },
                short(&base),
                short(&head)
            )
        }
        RunKind::Sweep => {
            let folders = files::sweep_folders(&all_paths);
            let folder = files::next_folder(&folders, row.sweep_cursor.as_deref())
                .ok_or_else(|| "nothing reviewable in this repo".to_string())?;
            for path in all_paths
                .iter()
                .filter(|p| files::is_reviewable(p) && files::in_folder(p, &folder))
            {
                if let Some(content) = load(&repo, &head, path).await {
                    targets.push(SourceFile {
                        repo: spec.name.clone(),
                        path: path.clone(),
                        content,
                        diff: None,
                        target: true,
                    });
                }
            }
            sweep_folder = Some(folder.clone());
            let shown = if folder == "." {
                "the top level"
            } else {
                &folder
            };
            format!("Weekly sweep: {shown} at {}", short(&head))
        }
        RunKind::Path(path) => {
            let path = path.trim_matches('/');
            if !files::valid_review_path(path) {
                return Err(format!("'{path}' is not a path inside the repo"));
            }
            for p in all_paths.iter().filter(|p| {
                files::is_reviewable(p) && (*p == path || p.starts_with(&format!("{path}/")))
            }) {
                if let Some(content) = load(&repo, &head, p).await {
                    targets.push(SourceFile {
                        repo: spec.name.clone(),
                        path: p.clone(),
                        content,
                        diff: None,
                        target: true,
                    });
                }
            }
            if targets.is_empty() {
                return Err(format!("nothing reviewable under '{path}'"));
            }
            format!("On demand: {path} at {}", short(&head))
        }
        RunKind::Branch(b) => {
            let base = branch_base.clone().unwrap_or_default();
            if base == rev {
                return finish_quiet(ctx, run_id, "the branch has nothing the main branch lacks");
            }
            let changed = repo.changed_files(&base, &rev).await?;
            for path in changed.iter().filter(|p| files::is_reviewable(p)) {
                if let Some(content) = load(&repo, &rev, path).await {
                    let diff = repo.diff(&base, &rev, path).await.ok();
                    targets.push(SourceFile {
                        repo: spec.name.clone(),
                        path: path.clone(),
                        content,
                        diff,
                        target: true,
                    });
                }
            }
            let n = repo.commit_count(&base, &rev).await.unwrap_or(0);
            format!(
                "Branch {b}: {n} commit{} not on {} ({}..{})",
                if n == 1 { "" } else { "s" },
                spec.branch,
                short(&base),
                short(&rev)
            )
        }
    };
    ctx.store(|s| {
        s.update_run(
            run_id,
            &RunUpdate {
                scope: Some(scope.clone()),
                ..Default::default()
            },
        )
    });
    if targets.is_empty() {
        let out = finish_quiet(ctx, run_id, "no reviewable files changed");
        match &req.kind {
            RunKind::Nightly | RunKind::Manual => {
                ctx.store(|s| s.set_last_reviewed(&spec.name, &head))
            }
            RunKind::Sweep => {
                if let Some(f) = &sweep_folder {
                    ctx.store(|s| s.set_sweep_cursor(&spec.name, f));
                }
            }
            RunKind::Path(_) | RunKind::Branch(_) => {}
        }
        return out;
    }

    // ── context: what the targets import, two levels deep ───────────────────
    let aliases: Vec<ImportAlias> = spec
        .aliases
        .iter()
        .map(|a| ImportAlias {
            prefix: a.prefix.clone(),
            repo: a.repo.clone(),
            dir: a.dir.clone(),
        })
        .collect();
    let mut known: HashSet<(String, String)> = all_paths
        .iter()
        .filter(|p| files::is_context_candidate(p))
        .map(|p| (spec.name.clone(), p.clone()))
        .collect();
    for (name, (r, h)) in &companions {
        if let Ok(list) = r.files(h).await {
            known.extend(
                list.into_iter()
                    .filter(|p| files::is_context_candidate(p))
                    .map(|p| (name.clone(), p)),
            );
        }
    }
    let exists = |r: &str, p: &str| known.contains(&(r.to_string(), p.to_string()));
    let mut all_files = targets.clone();
    let mut have: HashSet<(String, String)> = all_files
        .iter()
        .map(|f| (f.repo.clone(), f.path.clone()))
        .collect();
    let mut frontier: Vec<usize> = (0..all_files.len()).collect();
    for _ in 0..2 {
        let mut next = Vec::new();
        for i in frontier {
            for (r, p) in resolve_imports(&all_files[i].clone(), &aliases, &exists) {
                if all_files.len() >= targets.len() + MAX_CONTEXT_FILES
                    || !have.insert((r.clone(), p.clone()))
                {
                    continue;
                }
                let content = if r == spec.name {
                    load(&repo, &rev, &p).await
                } else if let Some((cr, ch)) = companions.get(&r) {
                    load(cr, ch, &p).await
                } else {
                    None
                };
                if let Some(content) = content {
                    all_files.push(SourceFile {
                        repo: r,
                        path: p,
                        content,
                        diff: None,
                        target: false,
                    });
                    next.push(all_files.len() - 1);
                }
            }
        }
        frontier = next;
    }

    // ── machines ────────────────────────────────────────────────────────────
    let waiting_since = Instant::now();
    let workers = loop {
        let ws = ctx.workers().await;
        if !ws.is_empty() {
            break ws;
        }
        if waiting_since.elapsed() >= ctx.limits.stall {
            return Err("no machine with a work model was available".into());
        }
        tokio::time::sleep(ctx.limits.poll).await;
    };
    let cap = if crate::schedule::is_evening((ctx.now)()) {
        ctx.limits.evening_max_tokens
    } else {
        ctx.limits.max_review_tokens
    };
    let system_tokens = codereview::estimate_tokens(codereview::prompt::REVIEW_INSTRUCTIONS);
    let budget = assign::review_budget(&workers, cap).saturating_sub(system_tokens + 200);
    if budget < 2_000 {
        return Err(format!(
            "the largest machine's context is too small for a review ({budget} tokens)"
        ));
    }
    let chunks = plan_chunks(&all_files, &aliases, budget);
    info!(repo = %spec.name, chunks = chunks.len(), budget, "review planned");

    let mut state = RunState {
        tasks: Vec::new(),
        findings: Vec::new(),
        dropped: 0,
        worker_tasks: BTreeMap::new(),
        next_id: 1,
    };
    for (i, c) in chunks.iter().enumerate() {
        let id = state.next_id;
        state.next_id += 1;
        state.tasks.push(Task {
            id,
            kind: TaskKind::Review,
            chunk: Some(i),
            finding: None,
            tokens: c.tokens + system_tokens + 200,
            attempts: 0,
            state: TState::Pending,
            avoid_model: None,
            label: chunk_label(c, &all_files),
        });
    }

    dispatch_all(
        ctx, &spec.name, &scope, &chunks, &all_files, &aliases, &mut state,
    )
    .await;

    // ── results ─────────────────────────────────────────────────────────────
    let gave_up_reviews = state
        .tasks
        .iter()
        .filter(|t| t.kind == TaskKind::Review && matches!(t.state, TState::GaveUp))
        .count();
    if gave_up_reviews == chunks.len() {
        return Err("every review task failed or could not be handed out".into());
    }
    let findings = codereview::dedupe::dedupe(state.findings);
    let finished = (ctx.now)().format("%Y-%m-%d %H:%M").to_string();
    let info = RunInfo {
        repo: spec.name.clone(),
        scope: if gave_up_reviews > 0 {
            format!(
                "{scope} ({gave_up_reviews} of {} chunks could not be reviewed)",
                chunks.len()
            )
        } else {
            scope.clone()
        },
        finished: finished.clone(),
        workers: state.worker_tasks.into_iter().collect(),
        chunks: chunks.len() as u32,
        dropped_by_quote_check: state.dropped,
        files_reviewed: targets.len() as u32,
    };
    let markdown = render(&info, &findings);
    let dir = ctx.home.join("reports").join(&spec.name);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let report_path = dir.join(format!("{}-run{run_id}.md", (ctx.now)().format("%Y-%m-%d")));
    std::fs::write(&report_path, &markdown).map_err(|e| e.to_string())?;
    let _ = std::fs::write(dir.join("latest.md"), &markdown);

    let counts = counts_of(&findings);
    let now = unix_now();
    ctx.store(|s| {
        for f in &findings {
            s.upsert_finding(run_id, f, now);
        }
        s.update_run(
            run_id,
            &RunUpdate {
                status: Some("done".into()),
                counts: Some(counts.clone()),
                finished_at: Some(now),
                report_path: Some(report_path.to_string_lossy().into_owned()),
                ..Default::default()
            },
        );
        match &req.kind {
            RunKind::Sweep => {
                if let Some(f) = &sweep_folder {
                    s.set_sweep_cursor(&spec.name, f);
                }
            }
            RunKind::Nightly | RunKind::Manual => s.set_last_reviewed(&spec.name, &head),
            // On-demand runs leave the nightly and sweep bookkeeping alone.
            RunKind::Path(_) | RunKind::Branch(_) => {}
        }
    });
    ctx.touch();

    if let Some(body) = headline(&spec.name, &findings)
        && let Some(topic) = ctx
            .store(|s| s.setting("ntfy_topic_url"))
            .filter(|t| !t.is_empty())
        && let Err(e) = ebay::ntfy::send_ntfy(&topic, "Code review", &body, None).await
    {
        warn!(error = %e, "review notification failed");
    }

    Ok(RunOutcome {
        run_id,
        status: "done".into(),
        findings,
        report_path: Some(report_path),
    })
}

fn finish_quiet(ctx: &Ctx, run_id: i64, why: &str) -> Result<RunOutcome, String> {
    ctx.store(|s| {
        s.update_run(
            run_id,
            &RunUpdate {
                status: Some("nothing_new".into()),
                error: Some(why.into()),
                finished_at: Some(unix_now()),
                ..Default::default()
            },
        )
    });
    ctx.touch();
    Ok(RunOutcome {
        run_id,
        status: "nothing_new".into(),
        findings: Vec::new(),
        report_path: None,
    })
}

fn counts_of(findings: &[Finding]) -> ReviewCounts {
    let mut c = ReviewCounts::default();
    for f in findings {
        match f.verdict {
            Some(Verdict::Confirmed) => match f.severity {
                codereview::Severity::High => c.high += 1,
                codereview::Severity::Medium => c.medium += 1,
                codereview::Severity::Low => c.low += 1,
            },
            Some(Verdict::Rejected) => {}
            _ => c.unconfirmed += 1,
        }
    }
    c
}

fn chunk_label(c: &Chunk, files: &[SourceFile]) -> String {
    let first = c
        .targets
        .first()
        .map(|&i| files[i].path.clone())
        .unwrap_or_default();
    match c.targets.len() {
        0 | 1 => first,
        n => format!("{first} +{} files", n - 1),
    }
}

struct RunState {
    tasks: Vec<Task>,
    findings: Vec<Finding>,
    dropped: u32,
    worker_tasks: BTreeMap<String, u32>,
    next_id: u64,
}

/// Find the file a model meant by `name` ("dashboard/src/a.ts" or "src/a.ts").
fn find_file(name: &str, candidates: &[usize], files: &[SourceFile]) -> Option<usize> {
    let name = name.trim().trim_start_matches("./");
    candidates
        .iter()
        .copied()
        .find(|&i| files[i].display_name() == name || files[i].path == name)
        .or_else(|| {
            candidates
                .iter()
                .copied()
                .find(|&i| name.ends_with(&files[i].path) || files[i].path.ends_with(name))
        })
}

#[allow(clippy::too_many_arguments)]
async fn dispatch_all(
    ctx: &Ctx,
    repo: &str,
    scope: &str,
    chunks: &[Chunk],
    files: &[SourceFile],
    aliases: &[ImportAlias],
    st: &mut RunState,
) {
    let (rtx, mut rrx) = mpsc::channel::<(u64, String, WorkInferenceDone)>(64);
    let mut running: HashMap<u64, String> = HashMap::new();
    let mut last_progress = Instant::now();
    let all_idx: Vec<usize> = (0..files.len()).collect();
    let exists_set: HashSet<(String, String)> = files
        .iter()
        .map(|f| (f.repo.clone(), f.path.clone()))
        .collect();
    let exists = |r: &str, p: &str| exists_set.contains(&(r.to_string(), p.to_string()));

    loop {
        let pending: Vec<TaskNeed> = st
            .tasks
            .iter()
            .filter(|t| matches!(t.state, TState::Pending))
            .map(|t| TaskNeed {
                id: t.id,
                kind: t.kind,
                tokens: t.tokens,
                avoid_model: t.avoid_model.clone(),
            })
            .collect();
        if pending.is_empty() && running.is_empty() {
            break;
        }
        ctx.set_progress(|p| {
            p.total = st.tasks.len() as u32;
            p.done = st
                .tasks
                .iter()
                .filter(|t| matches!(t.state, TState::Done | TState::GaveUp))
                .count() as u32;
        });

        // A question is waiting: hand out nothing new so a machine frees up
        // for it. Tasks already running carry on.
        let yielding = ctx
            .questions_waiting
            .load(std::sync::atomic::Ordering::SeqCst)
            > 0;
        if yielding {
            last_progress = Instant::now();
        }
        if !pending.is_empty() && !yielding {
            let workers = ctx.workers().await;
            let occupied: HashSet<String> = running.values().cloned().collect();
            let picks = assign::assign(&pending, &workers, &occupied);
            if !picks.is_empty() {
                last_progress = Instant::now();
            }
            for (task_id, node_id) in picks {
                let Some(w) = workers.iter().find(|w| w.node_id == node_id).cloned() else {
                    continue;
                };
                let Some(task) = st.tasks.iter_mut().find(|t| t.id == task_id) else {
                    continue;
                };
                let prompt = match task.kind {
                    TaskKind::Review => {
                        let c = &chunks[task.chunk.unwrap_or(0)];
                        codereview::prompt::review_prompt(repo, scope, c, files)
                    }
                    TaskKind::Verify => {
                        let f = &st.findings[task.finding.unwrap_or(0)];
                        let Some(fi) = files
                            .iter()
                            .position(|s| s.repo == f.repo && s.path == f.path)
                        else {
                            task.state = TState::GaveUp;
                            continue;
                        };
                        let imports: Vec<&SourceFile> =
                            resolve_imports(&files[fi], aliases, &exists)
                                .into_iter()
                                .filter_map(|(r, p)| {
                                    files.iter().find(|s| s.repo == r && s.path == p)
                                })
                                .collect();
                        let budget = assign::capacity(&w, TaskKind::Verify).min(VERIFY_BUDGET);
                        codereview::prompt::verify_prompt(f, &files[fi], &imports, budget)
                    }
                };
                task.state = TState::Running;
                running.insert(task.id, node_id.clone());
                let view = ReviewTaskView {
                    kind: match task.kind {
                        TaskKind::Review => "review".into(),
                        TaskKind::Verify => "check".into(),
                    },
                    node_id: node_id.clone(),
                    worker: w.label(),
                    label: task.label.clone(),
                };
                ctx.set_progress(|p| {
                    p.running.insert(task_id, view);
                });
                let req = WorkInferenceRequest {
                    request_id: format!("review-{}", uuid::Uuid::new_v4()),
                    node_id: Some(node_id.clone()),
                    model_name: Some(w.model.clone()),
                    messages: vec![ChatTurn::system(prompt.system), ChatTurn::user(prompt.user)],
                    max_tokens: match task.kind {
                        TaskKind::Review => REVIEW_MAX_TOKENS_OUT,
                        TaskKind::Verify => VERIFY_MAX_TOKENS_OUT,
                    },
                    temperature: Some(match task.kind {
                        TaskKind::Review => 0.2,
                        TaskKind::Verify => 0.0,
                    }),
                };
                let mesh = ctx.mesh.clone();
                let rtx = rtx.clone();
                let label = w.label();
                tokio::spawn(async move {
                    let done = mesh.infer(req).await;
                    let _ = rtx.send((task_id, label, done)).await;
                });
            }
        }

        // Wait for a result, or poll the snapshot again.
        let got = tokio::time::timeout(ctx.limits.poll, rrx.recv()).await;
        let Ok(Some((task_id, worker_label, done))) = got else {
            if running.is_empty()
                && !pending.is_empty()
                && last_progress.elapsed() >= ctx.limits.stall
            {
                // Nothing could be handed out for a long time: give up on what
                // is left. Unchecked findings stay in the report as "not confirmed".
                warn!(
                    left = pending.len(),
                    "no machine took the remaining tasks; giving up on them"
                );
                for t in st
                    .tasks
                    .iter_mut()
                    .filter(|t| matches!(t.state, TState::Pending))
                {
                    t.state = TState::GaveUp;
                }
            }
            continue;
        };
        last_progress = Instant::now();
        let node = running.remove(&task_id).unwrap_or_default();
        ctx.set_progress(|p| {
            p.running.remove(&task_id);
        });
        let Some(ti) = st.tasks.iter().position(|t| t.id == task_id) else {
            continue;
        };
        match done.outcome {
            WorkOutcome::Finished => {
                ctx.record_speed(&node, &done);
                *st.worker_tasks.entry(worker_label.clone()).or_insert(0) += 1;
                st.tasks[ti].state = TState::Done;
                match st.tasks[ti].kind {
                    TaskKind::Review => {
                        let c = &chunks[st.tasks[ti].chunk.unwrap_or(0)];
                        let in_chunk: Vec<usize> =
                            c.targets.iter().chain(c.context.iter()).copied().collect();
                        for raw in codereview::parse::parse_findings(&done.output) {
                            let Some(fi) = find_file(&raw.file, &in_chunk, files)
                                .or_else(|| find_file(&raw.file, &all_idx, files))
                            else {
                                st.dropped += 1;
                                continue;
                            };
                            let file = &files[fi];
                            let Some(line) = codereview::check::locate_quote(
                                &file.content,
                                &raw.quote,
                                raw.line,
                            ) else {
                                st.dropped += 1;
                                continue;
                            };
                            let id =
                                codereview::dedupe::finding_key(&file.repo, &file.path, &raw.quote);
                            if let Some(existing) = st.findings.iter_mut().find(|f| f.id == id) {
                                if !existing.found_by.contains(&worker_label) {
                                    existing.found_by.push(worker_label.clone());
                                }
                                continue;
                            }
                            st.findings.push(Finding {
                                id,
                                repo: file.repo.clone(),
                                path: file.path.clone(),
                                line,
                                severity: raw.severity,
                                title: raw.title,
                                quote: raw.quote,
                                scenario: raw.scenario,
                                fix: raw.fix,
                                found_by: vec![worker_label.clone()],
                                verdict: None,
                                verdict_reason: None,
                                checked_by: None,
                            });
                            let fidx = st.findings.len() - 1;
                            let id = st.next_id;
                            st.next_id += 1;
                            let label = st.findings[fidx].location();
                            st.tasks.push(Task {
                                id,
                                kind: TaskKind::Verify,
                                chunk: None,
                                finding: Some(fidx),
                                // Sized at hand-out; this is the ceiling.
                                tokens: VERIFY_BUDGET.min(
                                    codereview::chunk::file_cost(file)
                                        + codereview::estimate_tokens(
                                            codereview::prompt::VERIFY_INSTRUCTIONS,
                                        )
                                        + 600,
                                ),
                                attempts: 0,
                                state: TState::Pending,
                                avoid_model: Some(done.model_name.clone()),
                                label,
                            });
                        }
                    }
                    TaskKind::Verify => {
                        let (verdict, reason) = codereview::parse::parse_verdict(&done.output);
                        if let Some(f) = st.tasks[ti].finding.and_then(|i| st.findings.get_mut(i)) {
                            f.verdict = Some(verdict);
                            f.verdict_reason = Some(reason).filter(|r| !r.is_empty());
                            f.checked_by = Some(worker_label);
                        }
                    }
                }
            }
            WorkOutcome::Preempted => {
                let t = &mut st.tasks[ti];
                t.attempts += 1;
                t.state = if t.attempts >= MAX_ATTEMPTS_PAUSED {
                    TState::GaveUp
                } else {
                    TState::Pending
                };
            }
            WorkOutcome::Failed { reason } => {
                warn!(task = task_id, %node, %reason, "review task failed");
                let t = &mut st.tasks[ti];
                t.attempts += 1;
                t.state = if t.attempts >= MAX_ATTEMPTS_FAILED {
                    TState::GaveUp
                } else {
                    TState::Pending
                };
            }
            WorkOutcome::NoWorker { .. } => st.tasks[ti].state = TState::Pending,
        }
    }
    ctx.set_progress(|p| {
        p.done = p.total;
        p.running.clear();
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use shared::{ReviewAlias, ReviewRepoSpec};
    use std::path::Path;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A fake mesh: mac1 with a big context and beelink1 with a small one.
    /// Reviews report the invoice bug plus one invented finding; checks
    /// confirm anything about `pricing?.amount` and reject the rest.
    struct FakeMesh {
        reviews: AtomicU32,
        checks: AtomicU32,
        preempt_first: AtomicU32,
        expect_companion: bool,
        seen_nodes: Mutex<Vec<(String, String)>>,
    }

    #[async_trait]
    impl Mesh for FakeMesh {
        async fn workers(&self) -> Result<Vec<WorkerInfo>, String> {
            Ok(vec![
                WorkerInfo {
                    node_id: "mac1".into(),
                    hostname: "mac1".into(),
                    model_name: "qwen3-coder-30b".into(),
                    ctx_size: Some(262_144),
                    control: true,
                    work: true,
                    busy: false,
                    resting: false,
                },
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
            ])
        }

        async fn infer(&self, req: WorkInferenceRequest) -> WorkInferenceDone {
            let node = req.node_id.clone().unwrap_or_default();
            let system = &req.messages[0].content;
            let user = &req.messages[1].content;
            let is_review = system.contains("JSON array");
            self.seen_nodes.lock().unwrap().push((
                if is_review { "review" } else { "check" }.into(),
                node.clone(),
            ));
            let mut done = WorkInferenceDone {
                request_id: req.request_id,
                node_id: node.clone(),
                model_name: req.model_name.unwrap_or_default(),
                outcome: WorkOutcome::Finished,
                output: String::new(),
                prompt_tokens: 0,
                tokens_generated: 0,
                duration_ms: 10,
            };
            if is_review {
                if self
                    .preempt_first
                    .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| v.checked_sub(1))
                    .is_ok()
                {
                    done.outcome = WorkOutcome::Preempted;
                    return done;
                }
                self.reviews.fetch_add(1, Ordering::SeqCst);
                assert!(user.contains("(UNDER REVIEW)"));
                if self.expect_companion {
                    assert!(
                        user.contains("guv/src/lib/money.ts (CONTEXT)"),
                        "companion repo context"
                    );
                }
                done.output = r#"[
                  {"file":"dashboard/src/pages/JobDetailPage.tsx","line":99,"severity":"high",
                   "title":"Invoice bills the cheapest option","quote":"const agreed = pricing?.amount;",
                   "scenario":"accepted £1,900, billed £1,200","fix":"use the accepted amount"},
                  {"file":"dashboard/src/pages/JobDetailPage.tsx","line":5,"severity":"medium",
                   "title":"Invented","quote":"const nothing = invented();","scenario":"x","fix":"y"},
                  {"file":"dashboard/src/pages/JobDetailPage.tsx","line":2,"severity":"low",
                   "title":"Rounding","quote":"import { parsePrice } from '@app/lib/money';","scenario":"x","fix":"y"}
                ]"#
                .into();
            } else {
                self.checks.fetch_add(1, Ordering::SeqCst);
                done.output = if user.contains("pricing?.amount") && user.contains("cheapest") {
                    r#"{"verdict":"confirmed","reason":"line 3 takes the headline price"}"#.into()
                } else {
                    r#"{"verdict":"rejected","reason":"fine"}"#.into()
                };
            }
            done
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

    fn make_repo(root: &Path, name: &str, files: &[(&str, &str)]) -> PathBuf {
        let dir = root.join("origins").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        sh(&dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("README.md"), "x").unwrap();
        sh(&dir, &["add", "."]);
        sh(&dir, &["commit", "-qm", "init"]);
        for (p, c) in files {
            let path = dir.join(p);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, c).unwrap();
        }
        sh(&dir, &["add", "."]);
        sh(&dir, &["commit", "-qm", "work"]);
        dir
    }

    fn ctx(home: &Path, mesh: Arc<FakeMesh>, store: Store) -> Ctx {
        Ctx {
            store: Arc::new(Mutex::new(store)),
            home: home.to_path_buf(),
            mesh,
            progress: Arc::new(Mutex::new(Progress::default())),
            changed: Arc::new(Notify::new()),
            speeds: Arc::new(Mutex::new(HashMap::new())),
            questions_waiting: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            limits: Limits {
                poll: Duration::from_millis(20),
                stall: Duration::from_millis(500),
                check_urls: false,
                ..Limits::default()
            },
            now: || {
                chrono::NaiveDate::from_ymd_opt(2026, 10, 11)
                    .unwrap()
                    .and_hms_opt(2, 15, 0)
                    .unwrap()
            },
        }
    }

    /// Repos are added with local paths here (`check_urls` is off in tests;
    /// the URL rule itself is tested in `files`).
    fn add_repo(store: &Store, name: &str, url: &Path, aliases: Vec<ReviewAlias>) {
        store.upsert_repo(&ReviewRepoSpec {
            name: name.into(),
            url: url.to_string_lossy().into_owned(),
            branch: "main".into(),
            timeslots: vec![135],
            sweep_day: None,
            sweep_slot: None,
            enabled: true,
            aliases,
        });
    }

    #[tokio::test]
    async fn a_run_reviews_checks_and_reports_across_two_machines() {
        let tmp = tempfile::tempdir().unwrap();
        let guv = make_repo(
            tmp.path(),
            "guv",
            &[(
                "src/lib/money.ts",
                "export function parsePrice(s) { return Number(s); }\n",
            )],
        );
        let dash = make_repo(
            tmp.path(),
            "dashboard",
            &[
                (
                    "src/pages/JobDetailPage.tsx",
                    "import { parsePrice } from '@app/lib/money';\nfunction raise(pricing) {\n  const agreed = pricing?.amount;\n  return agreed;\n}\n",
                ),
                ("src/pages/JobDetailPage.test.tsx", "test('x', () => {});\n"),
            ],
        );
        let store = Store::open_in_memory().unwrap();
        add_repo(&store, "guv", &guv, vec![]);
        add_repo(
            &store,
            "dashboard",
            &dash,
            vec![ReviewAlias {
                prefix: "@app/".into(),
                repo: "guv".into(),
                dir: "src".into(),
            }],
        );
        let mesh = Arc::new(FakeMesh {
            reviews: AtomicU32::new(0),
            checks: AtomicU32::new(0),
            preempt_first: AtomicU32::new(1),
            expect_companion: true,
            seen_nodes: Mutex::new(Vec::new()),
        });
        let home = tmp.path().join("home");
        let c = ctx(&home, mesh.clone(), store);
        let out = run_local(&c, "dashboard", RunKind::Nightly).await;

        assert_eq!(out.status, "done");
        // One review (after one pause), two findings past the quote check, two checks.
        assert_eq!(mesh.reviews.load(Ordering::SeqCst), 1);
        assert_eq!(mesh.checks.load(Ordering::SeqCst), 2);
        let seen = mesh.seen_nodes.lock().unwrap().clone();
        assert!(
            seen.iter()
                .filter(|(k, _)| k == "review")
                .all(|(_, n)| n == "mac1")
        );
        assert!(
            seen.iter()
                .filter(|(k, _)| k == "check")
                .all(|(_, n)| n == "beelink1")
        );

        let confirmed: Vec<&Finding> = out
            .findings
            .iter()
            .filter(|f| f.verdict == Some(Verdict::Confirmed))
            .collect();
        assert_eq!(confirmed.len(), 1);
        assert_eq!(
            confirmed[0].line, 3,
            "line corrected from 99 to where the quote is"
        );
        assert_eq!(
            confirmed[0].checked_by.as_deref(),
            Some("qwen2.5:7b@beelink1")
        );

        let md = std::fs::read_to_string(out.report_path.unwrap()).unwrap();
        assert!(md.contains("Invoice bills the cheapest option"));
        assert!(md.contains("1 quoting code that is not in the file"));
        assert!(md.contains("1 rejected by the checker"));
        assert!(home.join("reports/dashboard/latest.md").exists());

        {
            let s = c.store.lock().unwrap();
            let open = s.open_findings(10);
            assert_eq!(open.len(), 1, "rejected findings are not listed");
            let row = s.repo("dashboard").unwrap();
            assert!(row.last_reviewed_commit.is_some());
            let run = &s.runs(1)[0];
            assert_eq!(run.status, "done");
            assert_eq!(run.counts.high, 1);
        }

        // Nothing new the second time.
        let again = run_local(&c, "dashboard", RunKind::Manual).await;
        assert_eq!(again.status, "nothing_new");
    }

    #[tokio::test]
    async fn a_sweep_walks_one_folder_and_moves_the_cursor() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = make_repo(
            tmp.path(),
            "app",
            &[
                ("functions/a.ts", "export const a = 1;\n"),
                ("src/b.ts", "export const b = 2;\n"),
            ],
        );
        let store = Store::open_in_memory().unwrap();
        add_repo(&store, "app", &repo, vec![]);
        let mesh = Arc::new(FakeMesh {
            reviews: AtomicU32::new(0),
            checks: AtomicU32::new(0),
            preempt_first: AtomicU32::new(0),
            expect_companion: false,
            seen_nodes: Mutex::new(Vec::new()),
        });
        let c = ctx(&tmp.path().join("home"), mesh.clone(), store);
        let first = run_local(&c, "app", RunKind::Sweep).await;
        assert_eq!(first.status, "done");
        assert_eq!(mesh.reviews.load(Ordering::SeqCst), 1);
        {
            let s = c.store.lock().unwrap();
            assert!(s.runs(1)[0].scope.starts_with("Weekly sweep: functions"));
            assert_eq!(
                s.repo("app").unwrap().sweep_cursor.as_deref(),
                Some("functions")
            );
        }
        let second = run_local(&c, "app", RunKind::Sweep).await;
        assert_eq!(second.status, "done");
        let s = c.store.lock().unwrap();
        assert!(s.runs(1)[0].scope.starts_with("Weekly sweep: src"));
        assert_eq!(s.repo("app").unwrap().sweep_cursor.as_deref(), Some("src"));
    }

    fn plain_mesh() -> Arc<FakeMesh> {
        Arc::new(FakeMesh {
            reviews: AtomicU32::new(0),
            checks: AtomicU32::new(0),
            preempt_first: AtomicU32::new(0),
            expect_companion: false,
            seen_nodes: Mutex::new(Vec::new()),
        })
    }

    #[tokio::test]
    async fn an_on_demand_path_review_covers_that_folder_only_and_moves_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = make_repo(
            tmp.path(),
            "app",
            &[
                ("src/services/a.ts", "export const a = 1;\n"),
                ("src/pages/b.ts", "export const b = 2;\n"),
            ],
        );
        let store = Store::open_in_memory().unwrap();
        add_repo(&store, "app", &repo, vec![]);
        let mesh = plain_mesh();
        let c = ctx(&tmp.path().join("home"), mesh.clone(), store);
        let out = run_local(&c, "app", RunKind::Path("src/services".into())).await;
        assert_eq!(out.status, "done");
        {
            let s = c.store.lock().unwrap();
            assert!(s.runs(1)[0].scope.starts_with("On demand: src/services"));
            let row = s.repo("app").unwrap();
            assert!(
                row.last_reviewed_commit.is_none(),
                "nightly bookkeeping untouched"
            );
            assert!(row.sweep_cursor.is_none());
        }
        let err = execute(
            &c,
            &RunRequest {
                repo: "app".into(),
                kind: RunKind::Path("docs".into()),
            },
        )
        .await
        .unwrap_err();
        assert!(err.contains("nothing reviewable"));
    }

    #[tokio::test]
    async fn a_branch_review_covers_what_the_branch_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = make_repo(tmp.path(), "app", &[("src/a.ts", "export const a = 1;\n")]);
        sh(&repo, &["checkout", "-q", "-b", "feature/x"]);
        std::fs::write(repo.join("src/new.ts"), "export const n = 3;\n").unwrap();
        sh(&repo, &["add", "."]);
        sh(&repo, &["commit", "-qm", "feature"]);
        sh(&repo, &["checkout", "-q", "main"]);
        let store = Store::open_in_memory().unwrap();
        add_repo(&store, "app", &repo, vec![]);
        let mesh = plain_mesh();
        let c = ctx(&tmp.path().join("home"), mesh.clone(), store);
        let out = run_local(&c, "app", RunKind::Branch("feature/x".into())).await;
        assert_eq!(out.status, "done");
        assert_eq!(mesh.reviews.load(Ordering::SeqCst), 1);
        let s = c.store.lock().unwrap();
        let scope = &s.runs(1)[0].scope;
        assert!(
            scope.starts_with("Branch feature/x: 1 commit not on main"),
            "{scope}"
        );
        assert!(s.repo("app").unwrap().last_reviewed_commit.is_none());
    }

    #[tokio::test]
    async fn a_waiting_question_holds_back_new_review_tasks() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = make_repo(tmp.path(), "app", &[("src/a.ts", "export const a = 1;\n")]);
        let store = Store::open_in_memory().unwrap();
        add_repo(&store, "app", &repo, vec![]);
        let mesh = plain_mesh();
        let c = ctx(&tmp.path().join("home"), mesh.clone(), store);
        c.questions_waiting.store(1, Ordering::SeqCst);
        let waiting = c.questions_waiting.clone();
        let run = tokio::spawn(async move { run_local(&c, "app", RunKind::Manual).await });
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            mesh.reviews.load(Ordering::SeqCst),
            0,
            "nothing handed out yet"
        );
        waiting.store(0, Ordering::SeqCst);
        let out = run.await.unwrap();
        assert_eq!(out.status, "done");
        assert_eq!(mesh.reviews.load(Ordering::SeqCst), 1);
    }

    async fn run_local(c: &Ctx, repo: &str, kind: RunKind) -> RunOutcome {
        execute(
            c,
            &RunRequest {
                repo: repo.into(),
                kind,
            },
        )
        .await
        .expect("run failed")
    }
}
