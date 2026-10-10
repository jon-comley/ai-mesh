//! The review capability — runs on mac1 (`NODE_FEATURES=llm,review`).
//!
//! mac1 is in charge of the code reviews: it keeps the repo list, runs the
//! schedule, fetches the repos, plans the work and hands review and check
//! tasks to whichever machines are free, through the coordinator (which
//! keeps home commands first). Findings, runs and reports live here in
//! `~/.ai-mesh/reviews/`; the coordinator only shows snapshots of them on the
//! dashboard's Reviews tab. See docs/code-review.md.

pub mod files;
pub mod git;
pub mod run;
pub mod schedule;
pub mod store;

use async_trait::async_trait;
use capability_core::Capability;
use run::{Ctx, Limits, Mesh, Progress, RunKind, RunRequest};
use shared::{
    MeshMessage, ReviewCommand, ReviewReply, ReviewRepoSpec, ReviewRepoView, ReviewSettingsView,
    ReviewSnapshot, WorkInferenceDone, WorkInferenceRequest, WorkOutcome, WorkerInfo,
    WorkerSnapshot,
};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use store::Store;
use tokio::sync::mpsc::Sender;
use tokio::sync::{Notify, oneshot};
use tracing::{info, warn};

/// How long one task may take before mac1 stops waiting for its result.
const TASK_TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);
/// Open findings sent to the dashboard.
const SNAPSHOT_FINDINGS: u32 = 200;
const SNAPSHOT_RUNS: u32 = 20;

struct Inner {
    node_id: String,
    hostname: String,
    home: PathBuf,
    store: Arc<Mutex<Store>>,
    /// The current connection to the coordinator; replaced on every reconnect.
    tx: Mutex<Option<Sender<MeshMessage>>>,
    pending_work: Mutex<HashMap<String, oneshot::Sender<WorkInferenceDone>>>,
    worker_waiters: Mutex<Vec<oneshot::Sender<WorkerSnapshot>>>,
    queue: Mutex<VecDeque<RunRequest>>,
    queue_wake: Notify,
    progress: Arc<Mutex<Progress>>,
    changed: Arc<Notify>,
    speeds: Arc<Mutex<HashMap<String, f32>>>,
    started: AtomicBool,
    notice: Mutex<Option<String>>,
}

pub struct ReviewCapability {
    inner: Arc<Inner>,
}

fn env_list(key: &str) -> Vec<String> {
    std::env::var(key)
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

fn env_num(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

impl ReviewCapability {
    pub fn new(node_id: &str) -> Self {
        let home = std::env::var("REVIEW_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                dirs::home_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join(".ai-mesh")
                    .join("reviews")
            });
        let (store, notice) = match Store::open(&home.join("reviews.db")) {
            Ok(s) => (s, None),
            Err(e) => {
                warn!(error = %e, "review database unavailable; using a temporary one");
                (
                    Store::open_in_memory().expect("in-memory SQLite"),
                    Some(format!("review database could not be opened: {e}")),
                )
            }
        };
        let hostname = std::env::var("HOSTNAME")
            .ok()
            .filter(|h| !h.is_empty())
            .unwrap_or_else(|| node_id.to_string());
        Self {
            inner: Arc::new(Inner {
                node_id: node_id.to_string(),
                hostname,
                home,
                store: Arc::new(Mutex::new(store)),
                tx: Mutex::new(None),
                pending_work: Mutex::new(HashMap::new()),
                worker_waiters: Mutex::new(Vec::new()),
                queue: Mutex::new(VecDeque::new()),
                queue_wake: Notify::new(),
                progress: Arc::new(Mutex::new(Progress::default())),
                changed: Arc::new(Notify::new()),
                speeds: Arc::new(Mutex::new(HashMap::new())),
                started: AtomicBool::new(false),
                notice: Mutex::new(notice),
            }),
        }
    }
}

impl Inner {
    fn sender(&self) -> Option<Sender<MeshMessage>> {
        self.tx.lock().unwrap().clone()
    }

    async fn send(&self, msg: MeshMessage) -> bool {
        match self.sender() {
            Some(tx) => tx.send(msg).await.is_ok(),
            None => false,
        }
    }

    fn limits(&self) -> Limits {
        let store = self.store.lock().unwrap();
        let num = |key: &str, env: &str, default: usize| {
            store
                .setting(key)
                .and_then(|v| v.parse().ok())
                .unwrap_or_else(|| env_num(env, default))
        };
        Limits {
            max_review_tokens: num("max_review_tokens", "REVIEW_MAX_TOKENS", 100_000),
            evening_max_tokens: num("evening_max_tokens", "REVIEW_EVENING_MAX_TOKENS", 32_000),
            allowed_owners: env_list("REVIEW_ALLOWED_OWNERS"),
            ..Limits::default()
        }
    }

    fn enqueue(&self, req: RunRequest) {
        let mut q = self.queue.lock().unwrap();
        if q.iter().any(|r| r.repo == req.repo && r.kind == req.kind) {
            return;
        }
        info!(repo = %req.repo, kind = req.kind.as_str(), "review queued");
        q.push_back(req);
        drop(q);
        self.queue_wake.notify_one();
        self.changed.notify_one();
    }

    fn set_notice(&self, notice: impl Into<String>) {
        *self.notice.lock().unwrap() = Some(notice.into());
        self.changed.notify_one();
    }

    fn snapshot(&self) -> ReviewSnapshot {
        let store = self.store.lock().unwrap();
        let progress = self.progress.lock().unwrap();
        let queued: Vec<RunRequest> = self.queue.lock().unwrap().iter().cloned().collect();
        let mut runs = store.runs(SNAPSHOT_RUNS);
        for r in &mut runs {
            if Some(r.id) == progress.run_id {
                r.running = progress.running.values().cloned().collect();
                r.tasks_total = progress.total;
                r.tasks_done = progress.done;
            }
        }
        // Queued runs have no row yet; show them first so "Run now" has an answer.
        let mut queued_views: Vec<shared::ReviewRunView> = queued
            .iter()
            .map(|q| shared::ReviewRunView {
                id: 0,
                repo: q.repo.clone(),
                kind: q.kind.as_str().into(),
                scope: "waiting to start".into(),
                status: "queued".into(),
                started_at: 0,
                finished_at: None,
                tasks_total: 0,
                tasks_done: 0,
                running: Vec::new(),
                counts: Default::default(),
                error: None,
            })
            .collect();
        queued_views.extend(runs);
        let topic = store.setting("ntfy_topic_url").filter(|t| !t.is_empty());
        let limits_max = store
            .setting("max_review_tokens")
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| env_num("REVIEW_MAX_TOKENS", 100_000))
            as u32;
        let limits_evening = store
            .setting("evening_max_tokens")
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| env_num("REVIEW_EVENING_MAX_TOKENS", 32_000))
            as u32;
        ReviewSnapshot {
            node_id: self.node_id.clone(),
            hostname: self.hostname.clone(),
            generated_at: run::unix_now() as u64,
            repos: store
                .repos()
                .into_iter()
                .map(|r| ReviewRepoView {
                    spec: r.spec,
                    last_reviewed_commit: r.last_reviewed_commit,
                    last_sweep_folder: r.sweep_cursor,
                })
                .collect(),
            runs: queued_views,
            findings: store.open_findings(SNAPSHOT_FINDINGS),
            settings: ReviewSettingsView {
                ntfy_hint: topic.as_ref().map(|t| {
                    let tail: String = t
                        .chars()
                        .rev()
                        .take(4)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect();
                    format!("…{tail}")
                }),
                ntfy_topic_set: topic.is_some(),
                max_review_tokens: limits_max,
                evening_max_tokens: limits_evening,
                allowed_owners: env_list("REVIEW_ALLOWED_OWNERS"),
            },
            notice: self.notice.lock().unwrap().clone(),
        }
    }

    async fn push_snapshot(&self) {
        let snap = self.snapshot();
        let _ = self.send(MeshMessage::ReviewSnapshot(Box::new(snap))).await;
    }

    async fn apply(&self, cmd: ReviewCommand) {
        match cmd {
            ReviewCommand::RunNow { repo, sweep } => {
                if self.store.lock().unwrap().repo(&repo).is_none() {
                    self.set_notice(format!("'{repo}' is not in the review list"));
                    return;
                }
                let kind = if sweep {
                    RunKind::Sweep
                } else {
                    RunKind::Manual
                };
                self.enqueue(RunRequest { repo, kind });
            }
            ReviewCommand::UpsertRepo { spec } => match validate_spec(&spec) {
                Ok(()) => {
                    self.store.lock().unwrap().upsert_repo(&spec);
                    *self.notice.lock().unwrap() = None;
                }
                Err(e) => self.set_notice(format!("{}: {e}", spec.name)),
            },
            ReviewCommand::RemoveRepo { name } => {
                self.store.lock().unwrap().remove_repo(&name);
            }
            ReviewCommand::SetFindingStatus { id, status } => {
                if !self.store.lock().unwrap().set_finding_status(&id, &status) {
                    self.set_notice(format!("could not set that finding to '{status}'"));
                }
            }
            ReviewCommand::SetSettings {
                ntfy_topic_url,
                max_review_tokens,
                evening_max_tokens,
            } => {
                let store = self.store.lock().unwrap();
                if let Some(url) = ntfy_topic_url {
                    let url = url.trim();
                    if url.is_empty() || url.starts_with("https://") {
                        store.set_setting("ntfy_topic_url", url);
                    } else {
                        drop(store);
                        self.set_notice("the ntfy topic must be an https:// URL");
                        return;
                    }
                }
                if let Some(n) = max_review_tokens {
                    store.set_setting("max_review_tokens", &n.max(4_000).to_string());
                }
                if let Some(n) = evening_max_tokens {
                    store.set_setting("evening_max_tokens", &n.max(4_000).to_string());
                }
            }
            ReviewCommand::RequestSnapshot => {}
            ReviewCommand::FetchReport { request_id, run_id } => {
                let path = self.store.lock().unwrap().report_path(run_id);
                let reply = match path.map(std::fs::read_to_string) {
                    Some(Ok(markdown)) => ReviewReply {
                        request_id,
                        markdown: Some(markdown),
                        error: None,
                    },
                    Some(Err(e)) => ReviewReply {
                        request_id,
                        markdown: None,
                        error: Some(format!("the report file could not be read: {e}")),
                    },
                    None => ReviewReply {
                        request_id,
                        markdown: None,
                        error: Some("that run has no report".into()),
                    },
                };
                let _ = self.send(MeshMessage::ReviewReply(reply)).await;
                return;
            }
        }
        self.changed.notify_one();
    }
}

/// Check a repo spec from the dashboard before storing it.
pub fn validate_spec(spec: &ReviewRepoSpec) -> Result<(), String> {
    if !files::valid_repo_name(&spec.name) {
        return Err("the name may only use letters, digits, '-', '_' and '.'".into());
    }
    files::validate_repo_url(&spec.url, &env_list("REVIEW_ALLOWED_OWNERS"))?;
    if spec.branch.is_empty()
        || spec.branch.starts_with('-')
        || spec.branch.contains("..")
        || spec.branch.chars().any(|c| c.is_whitespace() || c == ':')
    {
        return Err("that branch name is not valid".into());
    }
    if spec.timeslots.iter().any(|&m| m >= 24 * 60) || spec.sweep_slot.is_some_and(|m| m >= 24 * 60)
    {
        return Err("times must be within the day".into());
    }
    if spec.sweep_day.is_some_and(|d| d > 6) {
        return Err("the sweep day must be Monday to Sunday".into());
    }
    for a in &spec.aliases {
        if a.prefix.is_empty() || !files::valid_repo_name(&a.repo) || a.dir.contains("..") {
            return Err(format!("import alias '{}' is not valid", a.prefix));
        }
    }
    Ok(())
}

/// The mesh as seen from mac1: everything goes through the coordinator.
struct Link(Arc<Inner>);

#[async_trait]
impl Mesh for Link {
    async fn workers(&self) -> Result<Vec<WorkerInfo>, String> {
        let (tx, rx) = oneshot::channel();
        self.0.worker_waiters.lock().unwrap().push(tx);
        if !self.0.send(MeshMessage::RequestWorkers).await {
            return Err("not connected to the coordinator".into());
        }
        match tokio::time::timeout(Duration::from_secs(10), rx).await {
            Ok(Ok(snap)) => Ok(snap.workers),
            _ => Err("the coordinator did not answer".into()),
        }
    }

    async fn infer(&self, req: WorkInferenceRequest) -> WorkInferenceDone {
        let request_id = req.request_id.clone();
        let failed = |reason: &str| WorkInferenceDone {
            request_id: request_id.clone(),
            node_id: String::new(),
            model_name: String::new(),
            outcome: WorkOutcome::Failed {
                reason: reason.to_string(),
            },
            output: String::new(),
            prompt_tokens: 0,
            tokens_generated: 0,
            duration_ms: 0,
        };
        let (tx, rx) = oneshot::channel();
        self.0
            .pending_work
            .lock()
            .unwrap()
            .insert(request_id.clone(), tx);
        if !self.0.send(MeshMessage::WorkInferenceRequest(req)).await {
            self.0.pending_work.lock().unwrap().remove(&request_id);
            return failed("not connected to the coordinator");
        }
        match tokio::time::timeout(TASK_TIMEOUT, rx).await {
            Ok(Ok(done)) => done,
            Ok(Err(_)) => failed("the connection to the coordinator was lost"),
            Err(_) => {
                self.0.pending_work.lock().unwrap().remove(&request_id);
                failed("no answer within two hours")
            }
        }
    }
}

fn local_from_unix(ts: i64) -> Option<chrono::NaiveDateTime> {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_opt(ts, 0)
        .single()
        .map(|d| d.naive_local())
}

async fn runner(inner: Arc<Inner>) {
    loop {
        let next = inner.queue.lock().unwrap().front().cloned();
        let Some(req) = next else {
            inner.queue_wake.notified().await;
            continue;
        };
        let ctx = Ctx {
            store: inner.store.clone(),
            home: inner.home.clone(),
            mesh: Arc::new(Link(inner.clone())),
            progress: inner.progress.clone(),
            changed: inner.changed.clone(),
            speeds: inner.speeds.clone(),
            limits: inner.limits(),
            now: || chrono::Local::now().naive_local(),
        };
        // Leave it on the queue while it runs, so a second "Run now" for the
        // same repo is not queued behind it.
        let result = run::execute(&ctx, &req).await;
        inner.queue.lock().unwrap().pop_front();
        match result {
            Ok(out) => {
                info!(repo = %req.repo, status = %out.status, findings = out.findings.len(), "review run finished")
            }
            Err(e) => warn!(repo = %req.repo, error = %e, "review run failed"),
        }
        inner.changed.notify_one();
    }
}

async fn scheduler(inner: Arc<Inner>) {
    loop {
        let now = chrono::Local::now().naive_local();
        let repos = inner.store.lock().unwrap().repos();
        for row in repos.into_iter().filter(|r| r.spec.enabled) {
            let last = row.last_scheduled_at.and_then(local_from_unix);
            let sweep = row.spec.sweep_day.zip(row.spec.sweep_slot);
            if let Some(due) = schedule::due(now, last, &row.spec.timeslots, sweep) {
                inner
                    .store
                    .lock()
                    .unwrap()
                    .set_last_scheduled(&row.spec.name, run::unix_now());
                let kind = match due {
                    schedule::Due::Nightly => RunKind::Nightly,
                    schedule::Due::Sweep => RunKind::Sweep,
                };
                inner.enqueue(RunRequest {
                    repo: row.spec.name.clone(),
                    kind,
                });
            }
        }
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
}

async fn snapshot_pusher(inner: Arc<Inner>) {
    loop {
        // Every change, at most once a second, and once a minute regardless.
        let _ = tokio::time::timeout(Duration::from_secs(60), inner.changed.notified()).await;
        tokio::time::sleep(Duration::from_secs(1)).await;
        inner.push_snapshot().await;
    }
}

#[async_trait]
impl Capability for ReviewCapability {
    fn name(&self) -> &'static str {
        "review"
    }

    fn handles(&self, msg: &MeshMessage) -> bool {
        matches!(
            msg,
            MeshMessage::WorkInferenceDone(_)
                | MeshMessage::WorkerSnapshot(_)
                | MeshMessage::ReviewCommand(_)
        )
    }

    async fn start(&self, tx: Sender<MeshMessage>) -> Result<(), String> {
        let inner = &self.inner;
        *inner.tx.lock().unwrap() = Some(tx);
        // Work sent on the old connection will never be answered: the
        // coordinator replies on the connection the request came in on. Fail
        // it now so those tasks go back on the queue.
        let stale: Vec<_> = inner.pending_work.lock().unwrap().drain().collect();
        for (_, waiter) in stale {
            drop(waiter);
        }
        if !inner.started.swap(true, Ordering::SeqCst) {
            for (repo, kind) in inner
                .store
                .lock()
                .unwrap()
                .take_interrupted(run::unix_now())
            {
                inner.queue.lock().unwrap().push_back(RunRequest {
                    repo,
                    kind: RunKind::parse(&kind),
                });
            }
            tokio::spawn(runner(inner.clone()));
            tokio::spawn(scheduler(inner.clone()));
            tokio::spawn(snapshot_pusher(inner.clone()));
            inner.queue_wake.notify_one();
        }
        inner.push_snapshot().await;
        Ok(())
    }

    async fn handle(&self, msg: MeshMessage, _tx: Sender<MeshMessage>) {
        match msg {
            MeshMessage::WorkInferenceDone(done) => {
                let waiter = self
                    .inner
                    .pending_work
                    .lock()
                    .unwrap()
                    .remove(&done.request_id);
                if let Some(w) = waiter {
                    let _ = w.send(done);
                }
            }
            MeshMessage::WorkerSnapshot(snap) => {
                let waiters: Vec<_> = self
                    .inner
                    .worker_waiters
                    .lock()
                    .unwrap()
                    .drain(..)
                    .collect();
                for w in waiters {
                    let _ = w.send(snap.clone());
                }
            }
            MeshMessage::ReviewCommand(cmd) => self.inner.apply(cmd).await,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::ReviewAlias;
    use tokio::sync::mpsc;

    fn cap() -> (ReviewCapability, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let c = ReviewCapability::new("mac1");
        // Point it at a throwaway store.
        *c.inner.store.lock().unwrap() = Store::open(&tmp.path().join("r.db")).unwrap();
        (c, tmp)
    }

    fn spec(name: &str, url: &str) -> ReviewRepoSpec {
        ReviewRepoSpec {
            name: name.into(),
            url: url.into(),
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
        }
    }

    #[test]
    fn specs_are_checked_before_storing() {
        assert!(
            validate_spec(&spec(
                "dashboard",
                "https://github.com/jon-comley/dashboard"
            ))
            .is_ok()
        );
        assert!(validate_spec(&spec("../x", "https://github.com/a/b")).is_err());
        assert!(validate_spec(&spec("x", "/tmp/repo")).is_err());
        let mut s = spec("x", "https://github.com/a/b");
        s.timeslots = vec![1440];
        assert!(validate_spec(&s).is_err());
        let mut s = spec("x", "https://github.com/a/b");
        s.branch = "--upload-pack=x".into();
        assert!(validate_spec(&s).is_err());
        let mut s = spec("x", "https://github.com/a/b");
        s.aliases[0].dir = "../../etc".into();
        assert!(validate_spec(&s).is_err());
    }

    #[tokio::test]
    async fn commands_update_the_store_and_snapshot() {
        let (c, _tmp) = cap();
        c.inner
            .apply(ReviewCommand::UpsertRepo {
                spec: spec("dashboard", "https://github.com/jon-comley/dashboard"),
            })
            .await;
        c.inner
            .apply(ReviewCommand::UpsertRepo {
                spec: spec("evil", "file:///etc"),
            })
            .await;
        c.inner
            .apply(ReviewCommand::SetSettings {
                ntfy_topic_url: Some("https://ntfy.sh/mesh-reviews-x7q2".into()),
                max_review_tokens: Some(10),
                evening_max_tokens: None,
            })
            .await;
        c.inner
            .apply(ReviewCommand::RunNow {
                repo: "dashboard".into(),
                sweep: false,
            })
            .await;
        c.inner
            .apply(ReviewCommand::RunNow {
                repo: "dashboard".into(),
                sweep: false,
            })
            .await;
        let snap = c.inner.snapshot();
        assert_eq!(snap.repos.len(), 1);
        assert!(snap.notice.as_deref().unwrap().starts_with("evil:"));
        assert!(snap.settings.ntfy_topic_set);
        assert_eq!(snap.settings.ntfy_hint.as_deref(), Some("…x7q2"));
        assert_eq!(snap.settings.max_review_tokens, 4_000, "floored");
        let queued: Vec<_> = snap.runs.iter().filter(|r| r.status == "queued").collect();
        assert_eq!(queued.len(), 1, "a second Run now is not queued twice");
    }

    #[tokio::test]
    async fn work_results_and_worker_snapshots_reach_their_waiters() {
        let (c, _tmp) = cap();
        let (tx, mut rx) = mpsc::channel(8);
        *c.inner.tx.lock().unwrap() = Some(tx);
        let link = Link(c.inner.clone());

        let workers = tokio::spawn(async move { link.workers().await });
        assert!(matches!(rx.recv().await, Some(MeshMessage::RequestWorkers)));
        c.handle(
            MeshMessage::WorkerSnapshot(WorkerSnapshot {
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
            }),
            mpsc::channel(1).0,
        )
        .await;
        assert_eq!(workers.await.unwrap().unwrap().len(), 1);

        let link = Link(c.inner.clone());
        let job = tokio::spawn(async move {
            link.infer(WorkInferenceRequest {
                request_id: "review-1".into(),
                node_id: Some("beelink1".into()),
                model_name: None,
                messages: vec![],
                max_tokens: 10,
                temperature: None,
            })
            .await
        });
        assert!(matches!(
            rx.recv().await,
            Some(MeshMessage::WorkInferenceRequest(_))
        ));
        c.handle(
            MeshMessage::WorkInferenceDone(WorkInferenceDone {
                request_id: "review-1".into(),
                node_id: "beelink1".into(),
                model_name: "qwen2.5:7b".into(),
                outcome: WorkOutcome::Finished,
                output: "[]".into(),
                prompt_tokens: 1,
                tokens_generated: 1,
                duration_ms: 1,
            }),
            mpsc::channel(1).0,
        )
        .await;
        assert_eq!(job.await.unwrap().outcome, WorkOutcome::Finished);
    }

    #[tokio::test]
    async fn a_reconnect_fails_work_sent_on_the_old_connection() {
        let (c, _tmp) = cap();
        let (tx, mut rx) = mpsc::channel(8);
        *c.inner.tx.lock().unwrap() = Some(tx);
        let link = Link(c.inner.clone());
        let job = tokio::spawn(async move {
            link.infer(WorkInferenceRequest {
                request_id: "review-2".into(),
                node_id: None,
                model_name: None,
                messages: vec![],
                max_tokens: 10,
                temperature: None,
            })
            .await
        });
        assert!(rx.recv().await.is_some());
        // Mark as started so start() only swaps the connection.
        c.inner.started.store(true, Ordering::SeqCst);
        let (tx2, _rx2) = mpsc::channel(8);
        c.start(tx2).await.unwrap();
        assert!(matches!(
            job.await.unwrap().outcome,
            WorkOutcome::Failed { .. }
        ));
    }

    #[tokio::test]
    async fn fetch_report_replies_with_the_file_or_an_error() {
        let (c, tmp) = cap();
        let (tx, mut rx) = mpsc::channel(8);
        *c.inner.tx.lock().unwrap() = Some(tx);
        let path = tmp.path().join("r.md");
        std::fs::write(&path, "# Code review: x").unwrap();
        let id = {
            let s = c.inner.store.lock().unwrap();
            let id = s.create_run("x", "manual", "", "done", 1);
            s.update_run(
                id,
                &store::RunUpdate {
                    report_path: Some(path.to_string_lossy().into_owned()),
                    ..Default::default()
                },
            );
            id
        };
        c.inner
            .apply(ReviewCommand::FetchReport {
                request_id: "q1".into(),
                run_id: id,
            })
            .await;
        let Some(MeshMessage::ReviewReply(r)) = rx.recv().await else {
            panic!("expected a reply");
        };
        assert_eq!(r.markdown.as_deref(), Some("# Code review: x"));
        c.inner
            .apply(ReviewCommand::FetchReport {
                request_id: "q2".into(),
                run_id: 999,
            })
            .await;
        let Some(MeshMessage::ReviewReply(r)) = rx.recv().await else {
            panic!("expected a reply");
        };
        assert!(r.error.is_some());
    }
}
