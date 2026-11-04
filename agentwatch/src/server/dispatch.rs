//! Runs queued work for a machine that is not always on (plans/agent-terminals.md,
//! "Work laptop"). No AI here: every minute it checks the laptop is reachable and
//! its repos are mounted, collects finished tasks and starts the next one. Each
//! task runs on lenovo1, detached in tmux: Claude edits through the sshfs mount
//! and runs commands on the laptop with `wl`. Nothing of Claude is on the laptop.

use std::time::Duration;

use agentterm::queue::{self, Task, TaskState};
use agentwatch::config::Kind;
use chrono::Utc;

use super::App;
use super::run;

const LIMIT: Duration = Duration::from_secs(20);

pub async fn tick(app: &App) {
    let agents = app
        .config
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .agents
        .clone();
    for agent in agents.into_iter().filter(|a| a.kind == Kind::Queue) {
        let (Some(host), Some(slug)) = (agent.host.clone(), agentterm::slug(&agent.name)) else {
            continue;
        };
        let on = run::output(run::ssh(&host, "exit"), LIMIT).await.is_ok();
        app.machine_on
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(agent.name.clone(), on);
        // Tasks run here, so they are collected even with the laptop off.
        let dir = agentterm::term_dir(&app.dir, &slug);
        for mut t in queue::list(&dir)
            .into_iter()
            .filter(|t| t.state == TaskState::Running)
        {
            if let Err(e) = collect(&dir, &mut t).await {
                tracing::warn!("{}: could not check task {}: {e}", agent.name, t.id);
            }
        }
        if !on {
            continue;
        }
        let home = std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        if !mounted(&home) {
            continue;
        }
        let tasks = queue::list(&dir);
        if let Some(t) = queue::next(&tasks).cloned() {
            if let Err(e) = start(&dir, t.clone()).await {
                tracing::warn!("{}: could not start task {}: {e}", agent.name, t.id);
            }
        }
    }
}

/// A running task: still going, finished (its result lands in the queue), or
/// gone without a result because lenovo1 restarted (interrupted).
async fn collect(dir: &std::path::Path, t: &mut Task) -> run::Result<()> {
    let out = dir.join("results").join(format!("{}.json", t.id));
    if let Ok(said) = std::fs::read_to_string(&out) {
        queue::finish(t, &said, Utc::now());
        let today = chrono::Local::now().date_naive();
        let _ = agentterm::record_cost(dir, &t.id, t.cost_usd, today);
    } else if tmux_has(&queue::task_session(t)).await {
        return Ok(());
    } else {
        t.state = TaskState::Interrupted;
        t.finished = Some(Utc::now());
        t.result = "It stopped before it finished. Retry runs it again.".into();
    }
    queue::save(dir, t).map_err(|e| e.to_string())
}

async fn tmux_has(session: &str) -> bool {
    tokio::process::Command::new("tmux")
        .args(["has-session", "-t", session])
        .output()
        .await
        .is_ok_and(|o| o.status.success())
}

/// The laptop's repos are mounted here (systemd/agentterm-wl-repos.service).
fn mounted(home: &std::path::Path) -> bool {
    let m = home.join(queue::MOUNT.trim_start_matches("~/"));
    std::fs::read_to_string("/proc/mounts").is_ok_and(|t| {
        t.lines()
            .any(|l| l.split(' ').nth(1) == Some(&*m.to_string_lossy()))
    })
}

async fn start(dir: &std::path::Path, mut t: Task) -> run::Result<()> {
    let tasks = dir.join("tasks");
    std::fs::create_dir_all(&tasks).map_err(|e| e.to_string())?;
    let path = tasks.join(format!("{}.sh", t.id));
    std::fs::write(&path, queue::task_script(&t, &dir.join("results")))
        .map_err(|e| e.to_string())?;
    let session = queue::task_session(&t);
    run::output(
        {
            let mut c = tokio::process::Command::new("tmux");
            c.args(["new-session", "-d", "-s", &session, "bash"])
                .arg(&path);
            c
        },
        LIMIT,
    )
    .await?;
    t.state = TaskState::Running;
    t.started = Some(Utc::now());
    queue::save(dir, &t).map_err(|e| e.to_string())
}

/// Jon's Retry: an interrupted or failed task goes back in the queue.
pub fn retry(dir: &std::path::Path, id: &str) -> run::Result<Task> {
    let mut t = queue::list(dir)
        .into_iter()
        .find(|t| t.id == id)
        .ok_or_else(|| format!("no task {id}"))?;
    if !matches!(
        t.state,
        TaskState::Interrupted | TaskState::Failed | TaskState::NeedsYou
    ) {
        return Err("only a stopped task can be retried".into());
    }
    t.state = TaskState::Queued;
    t.started = None;
    t.finished = None;
    t.result.clear();
    queue::save(dir, &t).map_err(|e| e.to_string())?;
    Ok(t)
}
