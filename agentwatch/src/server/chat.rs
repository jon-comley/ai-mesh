//! A worker's live chat: `claude` on lenovo1, in a clone of the worker's repo
//! kept for chats alone (so a poll never resets it under a conversation),
//! resuming one conversation per worker.

use std::process::Stdio;
use std::time::Duration;

use agentwatch::chat::{self, Event};
use agentwatch::config::{Agent, Kind};
use chrono::Utc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use super::App;
use super::poll::View;
use super::repo::Repo;

/// The longest one answer may take before it is stopped.
const ANSWER_LIMIT: Duration = Duration::from_secs(15 * 60);

/// The tools a chat may use without asking: its own repo's files and git.
const TOOLS: &str =
    "Read,Glob,Grep,Edit,MultiEdit,Write,Bash(git:*),Bash(ls:*),Bash(mkdir:*),Bash(date:*)";

pub fn chat_repo(app: &App, agent: &Agent) -> Repo {
    let mut repo = app.repo(agent);
    repo.dir = app
        .dir
        .join("chats")
        .join(agent.name.to_lowercase().replace(' ', "-"));
    repo
}

/// Sends Jon's message and streams the worker's answer into `tx` as it
/// comes. The answer, and what it did, are kept for the page.
pub async fn talk(app: &App, agent: &Agent, message: &str, tx: mpsc::Sender<Event>) {
    if let Err(text) = run(app, agent, message, &tx).await {
        let _ = app.store().add_message(&agent.name, "error", &text);
        let _ = tx.send(Event::Error { text }).await;
    }
}

async fn run(
    app: &App,
    agent: &Agent,
    message: &str,
    tx: &mpsc::Sender<Event>,
) -> Result<(), String> {
    if agent.kind != Kind::Cloud {
        return Err(format!(
            "Chat with {} is not built yet: it runs on {}.",
            agent.name,
            agent.host.as_deref().unwrap_or("its host")
        ));
    }
    let repo = chat_repo(app, agent);
    repo.sync().await?;
    let brief = repo.read("agent-brief.md");
    let tile = app
        .views()
        .into_iter()
        .find(|v| v.name == agent.name)
        .map(|v| tile_text(&v))
        .unwrap_or_default();
    let today = Utc::now().format("%Y-%m-%d").to_string();
    let context = chat::context(&agent.name, brief.as_deref(), &tile, &today);
    let session = app
        .store()
        .chat_session(&agent.name)
        .map_err(|e| e.to_string())?;
    app.store()
        .add_message(&agent.name, "jon", message)
        .map_err(|e| e.to_string())?;

    let mut cmd = Command::new("claude");
    cmd.current_dir(&repo.dir)
        .args(["-p", message, "--output-format", "stream-json", "--verbose"])
        .args(["--append-system-prompt", &context])
        .args(["--permission-mode", "acceptEdits", "--allowedTools", TOOLS, "--max-turns", "40"])
        .env(
            "GIT_SSH_COMMAND",
            format!("ssh -i {} -o IdentitiesOnly=yes -o BatchMode=yes -o StrictHostKeyChecking=accept-new", repo.key.display()),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(s) = &session {
        cmd.args(["--resume", s]);
    }
    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "claude is not installed on lenovo1".to_string()
        } else {
            e.to_string()
        }
    })?;
    let stdout = child.stdout.take().expect("piped");
    let mut lines = BufReader::new(stdout).lines();
    let mut finished = false;
    let read = async {
        while let Ok(Some(line)) = lines.next_line().await {
            for event in chat::parse_line(&line) {
                match &event {
                    Event::Text { text } => {
                        let _ = app.store().add_message(&agent.name, "worker", text);
                    }
                    Event::Tool { text } => {
                        let _ = app.store().add_message(&agent.name, "tool", text);
                    }
                    Event::Done { session, .. } => {
                        if !session.is_empty() {
                            let _ = app.store().set_chat_session(&agent.name, session);
                        }
                        finished = true;
                    }
                    Event::Error { text } => {
                        let _ = app.store().add_message(&agent.name, "error", text);
                        finished = true;
                    }
                }
                let _ = tx.send(event).await;
            }
        }
    };
    if tokio::time::timeout(ANSWER_LIMIT, read).await.is_err() {
        let _ = child.kill().await;
        return Err(format!(
            "No answer after {} minutes, so it was stopped.",
            ANSWER_LIMIT.as_secs() / 60
        ));
    }
    let status = child.wait_with_output().await.map_err(|e| e.to_string())?;
    if !finished {
        let said = String::from_utf8_lossy(&status.stderr).trim().to_string();
        // A resumed session that no longer exists: start a fresh one next time.
        if said.contains("No conversation found") {
            let _ = app.store().clear_chat(&agent.name);
        }
        return Err(if said.is_empty() {
            "claude stopped without an answer".into()
        } else {
            said
        });
    }
    // It may have changed its hub; show that on the tile now.
    let _work = app.work.lock().await;
    app.refresh(agent).await;
    Ok(())
}

/// What the tile shows, as plain text for the chat's context.
pub fn tile_text(v: &View) -> String {
    let mut out = format!(
        "State: {:?}. Runs every {}. Last run: {}. Last log line: {}\n",
        v.state,
        v.every,
        v.last_run
            .map(|t| t.to_rfc3339())
            .unwrap_or_else(|| "never".into()),
        if v.line.is_empty() { "none" } else { &v.line },
    );
    if !v.rows.is_empty() {
        out.push_str("Waiting on Jon (hub rows, number: what | his reply so far):\n");
        for r in &v.rows {
            let what = r
                .cells
                .get(2)
                .or(r.cells.get(1))
                .cloned()
                .unwrap_or_default();
            let reply = if r.reply.is_empty() {
                "no reply yet"
            } else {
                &r.reply
            };
            out.push_str(&format!("- {}: {what} | {reply}\n", r.number));
        }
    }
    for l in &v.waiting_lines {
        out.push_str(&format!("- {l}\n"));
    }
    if let Some(r) = &v.report {
        out.push_str(&format!("Latest report: {r}\n"));
    }
    if let Some(e) = &v.error {
        out.push_str(&format!("Problem reading it: {e}\n"));
    }
    out
}
