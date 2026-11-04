//! Runs queued work on a machine that is not always on (plans/agent-terminals.md,
//! "Work laptop"). No AI here: every minute it checks the machine is reachable,
//! collects finished tasks and starts the next one, detached in tmux there so
//! a dropped link does not stop it. lenovo1 always starts the connection.

use std::process::Stdio;
use std::time::Duration;

use agentterm::queue::{self, Task, TaskState};
use agentwatch::config::Kind;
use chrono::Utc;
use tokio::io::AsyncWriteExt;

use super::App;
use super::run;

const LIMIT: Duration = Duration::from_secs(20);
const RUNNING: &str = "AGENTTERM_RUNNING";
const GONE: &str = "AGENTTERM_GONE";

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
        if !on {
            continue;
        }
        let dir = agentterm::term_dir(&app.dir, &slug);
        for mut t in queue::list(&dir)
            .into_iter()
            .filter(|t| t.state == TaskState::Running)
        {
            if let Err(e) = collect(&host, &dir, &mut t).await {
                tracing::warn!("{}: could not check task {}: {e}", agent.name, t.id);
            }
        }
        let tasks = queue::list(&dir);
        if let Some(t) = queue::next(&tasks).cloned() {
            if let Err(e) = start(&host, &dir, t.clone()).await {
                tracing::warn!("{}: could not start task {}: {e}", agent.name, t.id);
            }
        }
    }
}

/// A running task: still going, finished (its result lands in the queue), or
/// gone without a result because the machine stopped (interrupted).
async fn collect(host: &str, dir: &std::path::Path, t: &mut Task) -> run::Result<()> {
    let out = format!("{}/results/{}.json", queue::MACHINE_DIR, t.id);
    let script = format!(
        "cat {out} 2>/dev/null || {{ tmux has-session -t {} 2>/dev/null && echo {RUNNING} || echo {GONE}; }}",
        queue::task_session(t)
    );
    let said = run::output(run::ssh(host, &script), LIMIT).await?;
    if said.contains(RUNNING) {
        return Ok(());
    }
    if said.trim() == GONE {
        t.state = TaskState::Interrupted;
        t.finished = Some(Utc::now());
        t.result = "The machine stopped before it finished. Retry runs it again.".into();
    } else {
        queue::finish(t, &said, Utc::now());
        let today = chrono::Local::now().date_naive();
        let _ = agentterm::record_cost(dir, &t.id, t.cost_usd, today);
    }
    queue::save(dir, t).map_err(|e| e.to_string())
}

async fn start(host: &str, dir: &std::path::Path, mut t: Task) -> run::Result<()> {
    let path = format!("{}/tasks/{}.sh", queue::MACHINE_DIR, t.id);
    let mut cmd = run::ssh(
        host,
        &format!("mkdir -p {}/tasks && cat > {path}", queue::MACHINE_DIR),
    );
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("ssh: {e}"))?;
    let mut stdin = child.stdin.take().expect("piped");
    stdin
        .write_all(queue::task_script(&t).as_bytes())
        .await
        .map_err(|e| format!("ssh: {e}"))?;
    drop(stdin);
    let done = tokio::time::timeout(LIMIT, child.wait_with_output())
        .await
        .map_err(|_| "ssh took too long writing the task".to_string())?
        .map_err(|e| format!("ssh: {e}"))?;
    if !done.status.success() {
        return Err(String::from_utf8_lossy(&done.stderr).trim().to_string());
    }
    let session = queue::task_session(&t);
    run::output(
        run::ssh(
            host,
            &format!("tmux new-session -d -s {session} 'bash -l {path}'"),
        ),
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
