//! A worker's live chat: one ongoing `claude` conversation per worker.
//!
//! - A cloud worker's chat runs on lenovo1, in a clone of its repo kept for
//!   chats alone (so a poll never resets it under a conversation).
//! - A local worker's chat runs on its own host over ssh (`claude` on mac1), in
//!   its own working directory, so it is the same worker with its own files.
//!   The orchestrator's tick is paused while Jon talks to it.
//!
//! Every chat starts with the shared memory's index and `jon/` folder only,
//! and opens other memory files when the subject comes up.

use std::process::Stdio;
use std::time::{Duration, Instant};

use agentwatch::chat::{self, Event, Memory};
use agentwatch::config::{Agent, Kind};
use chrono::Utc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

use super::App;
use super::poll::View;
use super::repo::Repo;
use super::run;

/// The longest one answer may take before it is stopped.
const ANSWER_LIMIT: Duration = Duration::from_secs(15 * 60);

/// How long after its last message a chat keeps the orchestrator's tick paused.
pub const CHAT_IDLE: Duration = Duration::from_secs(30 * 60);

/// The tools a cloud worker's chat may use without asking: its own repo, the
/// memory, and git.
const TOOLS: &str =
    "Read,Glob,Grep,Edit,MultiEdit,Write,Bash(git:*),Bash(ls:*),Bash(mkdir:*),Bash(date:*)";

/// The tools a read-only local chat (the reviews) may use: read anything,
/// write only in the memory.
const READONLY_TOOLS: &str = "Read,Glob,Grep,Bash(ls:*),Bash(git:*),Bash(date:*),\
Edit(~/.ai-mesh/memory/**),Write(~/.ai-mesh/memory/**)";

/// A line the local script prints when it paused the tick for this chat.
const PAUSED_BY_CHAT: &str = "AGENTWATCH_PAUSED_BY_CHAT";

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
    if let Err(text) = run_chat(app, agent, message, &tx).await {
        let _ = app.store().add_message(&agent.name, "error", &text);
        let _ = tx.send(Event::Error { text }).await;
    }
}

async fn run_chat(
    app: &App,
    agent: &Agent,
    message: &str,
    tx: &mpsc::Sender<Event>,
) -> Result<(), String> {
    let tile = app
        .views()
        .into_iter()
        .find(|v| v.name == agent.name)
        .map(|v| tile_text(&v))
        .unwrap_or_default();
    let today = Utc::now().format("%Y-%m-%d").to_string();
    let memory_text = memory_text(app).await;
    let session = app
        .store()
        .chat_session(&agent.name)
        .map_err(|e| e.to_string())?;
    if session
        .as_deref()
        .is_some_and(|s| !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
    {
        let _ = app.store().clear_chat(&agent.name);
        return Err("the saved conversation id looked wrong, so it was cleared; send again".into());
    }

    let child = match agent.kind {
        Kind::Queue => return Err(format!("{} is talked to in its terminal", agent.name)),
        Kind::Meter => return Err(format!("{} only counts spend", agent.name)),
        Kind::Cloud => {
            let repo = chat_repo(app, agent);
            repo.sync_keep().await?;
            let memory = memory_text.map(|(index, jon)| Memory {
                path: app.dir.join("memory").display().to_string(),
                index,
                jon,
            });
            let context = chat::context(
                &agent.name,
                repo.read("agent-brief.md").as_deref(),
                &tile,
                &today,
                memory.as_ref(),
            );
            app.store()
                .add_message(&agent.name, "jon", message)
                .map_err(|e| e.to_string())?;
            let mut cmd = Command::new("claude");
            cmd.current_dir(&repo.dir)
                .args(["-p", message, "--output-format", "stream-json", "--verbose"])
                .args(["--append-system-prompt", &context])
                .args([
                    "--permission-mode",
                    "acceptEdits",
                    "--allowedTools",
                    TOOLS,
                    "--max-turns",
                    "40",
                ]);
            if memory.is_some() {
                cmd.arg("--add-dir").arg(app.dir.join("memory"));
            }
            if let Some(s) = &session {
                cmd.args(["--resume", s]);
            }
            spawn(cmd, false)?
        }
        Kind::Local => {
            let host = agent.host.as_deref().expect("checked by Config::parse");
            let memory = memory_text.map(|(index, jon)| Memory {
                path: agent.memory_dir.clone(),
                index,
                jon,
            });
            let brief_note = agent
                .brief
                .as_ref()
                .map(|b| format!("It is the file {b}, appended below."));
            let context = chat::context(
                &agent.name,
                brief_note.as_deref(),
                &tile,
                &today,
                memory.as_ref(),
            );
            let script = local_script(
                agent,
                &context,
                message,
                session.as_deref(),
                memory.is_some(),
            );
            app.store()
                .add_message(&agent.name, "jon", message)
                .map_err(|e| e.to_string())?;
            let mut child = spawn(run::ssh(host, "/bin/bash -s"), true)?;
            let mut stdin = child.stdin.take().expect("piped");
            stdin
                .write_all(script.as_bytes())
                .await
                .map_err(|e| e.to_string())?;
            drop(stdin);
            child
        }
    };
    stream(app, agent, child, tx).await?;
    // It may have changed its hub or status; show that on the tile now.
    let _work = app.work.lock().await;
    app.refresh(agent).await;
    Ok(())
}

fn spawn(mut cmd: Command, piped_stdin: bool) -> Result<Child, String> {
    cmd.stdin(if piped_stdin {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    cmd.stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "claude is not installed here".to_string()
        } else {
            e.to_string()
        }
    })
}

/// Reads the answer as it comes, keeps it, and passes it on to the page.
async fn stream(
    app: &App,
    agent: &Agent,
    mut child: Child,
    tx: &mpsc::Sender<Event>,
) -> Result<(), String> {
    let stdout = child.stdout.take().expect("piped");
    let mut lines = BufReader::new(stdout).lines();
    let mut finished = false;
    let read = async {
        while let Ok(Some(line)) = lines.next_line().await {
            if line.trim() == PAUSED_BY_CHAT {
                app.chat_paused
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(agent.name.clone(), Instant::now());
                let _ = tx
                    .send(Event::Tool {
                        text: "Paused its 20-minute tick while you talk".into(),
                    })
                    .await;
                continue;
            }
            for event in chat::parse_line(&line) {
                finished |= keep(app, &agent.name, &event);
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
    if let Some(at) = app
        .chat_paused
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get_mut(&agent.name)
    {
        *at = Instant::now();
    }
    let out = child.wait_with_output().await.map_err(|e| e.to_string())?;
    if finished {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&out.stderr).trim().to_string();
    // A resumed conversation that no longer exists: start a fresh one next time.
    if said.contains("No conversation found") {
        let _ = app.store().clear_chat(&agent.name);
        return Err(
            "That conversation had expired, so a new one starts with your next message.".into(),
        );
    }
    Err(if said.is_empty() {
        "claude stopped without an answer".into()
    } else {
        said
    })
}

/// Keeps one event for the page; true when it ends the answer.
fn keep(app: &App, agent: &str, event: &Event) -> bool {
    let store = app.store();
    match event {
        Event::Text { text } => {
            let _ = store.add_message(agent, "worker", text);
            false
        }
        Event::Tool { text } => {
            let _ = store.add_message(agent, "tool", text);
            false
        }
        Event::Done { session, .. } => {
            if !session.is_empty() {
                let _ = store.set_chat_session(agent, session);
            }
            true
        }
        Event::Error { text } => {
            let _ = store.add_message(agent, "error", text);
            true
        }
    }
}

/// The memory's index and `jon/` files from lenovo1's clone, brought up to
/// date first. None when no memory repo is set or it cannot be read.
async fn memory_text(app: &App) -> Option<(String, Vec<(String, String)>)> {
    let repo = app.memory_repo()?;
    if let Err(e) = repo.sync_keep().await {
        tracing::warn!("memory: {e}");
    }
    let index = repo.read("INDEX.md")?;
    let mut jon: Vec<(String, String)> = std::fs::read_dir(repo.dir.join("jon"))
        .map(|d| {
            d.filter_map(|e| e.ok())
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    let text = std::fs::read_to_string(e.path()).ok()?;
                    name.ends_with(".md").then(|| (format!("jon/{name}"), text))
                })
                .collect()
        })
        .unwrap_or_default();
    jon.sort();
    Some((index, jon))
}

/// The bash script that runs a local worker's chat on its host. Paths were
/// checked by Config::parse; the context and message go in through quoted
/// heredocs, so nothing in them is read by the shell.
fn local_script(
    agent: &Agent,
    context: &str,
    message: &str,
    session: Option<&str>,
    memory: bool,
) -> String {
    let tag = heredoc_tag(&[context, message]);
    let dir = agent.chat_dir.as_deref().unwrap_or("~");
    let mut s = format!("cd {dir} || exit 1\n");
    if let Some(env) = &agent.chat_env {
        s.push_str(&format!(". {env}\n"));
    }
    if memory {
        s.push_str(&format!(
            "git -C {} pull -q --ff-only >/dev/null 2>&1\n",
            agent.memory_dir
        ));
    }
    if let (Some(pause), Some(label)) = (&agent.pause, &agent.kickstart) {
        // Never two workers in one checkout: wait for a running tick, then pause.
        s.push_str(&format!(
            "if launchctl list {label} 2>/dev/null | grep -q '\"PID\"'; then\n  \
             echo '{{\"type\":\"result\",\"subtype\":\"busy\",\"is_error\":true,\"result\":\"It is in the middle of a step. Ask again in a minute or two.\"}}'\n  \
             exit 0\nfi\n\
             if [ ! -e {pause} ]; then touch {pause}; echo {PAUSED_BY_CHAT}; fi\n"
        ));
    }
    s.push_str(&format!(
        "ctx=$(mktemp); msg=$(mktemp)\ncat > \"$ctx\" <<'{tag}'\n{context}\n{tag}\n"
    ));
    if let Some(brief) = &agent.brief {
        s.push_str(&format!(
            "printf '\\n' >> \"$ctx\"; cat {brief} >> \"$ctx\" 2>/dev/null\n"
        ));
    }
    s.push_str(&format!("cat > \"$msg\" <<'{tag}'\n{message}\n{tag}\n"));
    let (mode, tools) = if agent.chat_readonly {
        ("acceptEdits", READONLY_TOOLS)
    } else {
        ("auto", "")
    };
    s.push_str(&format!(
        "~/.local/bin/claude -p \"$(cat \"$msg\")\" --output-format stream-json --verbose \
         --append-system-prompt \"$(cat \"$ctx\")\" --permission-mode {mode} --max-turns 40"
    ));
    if !tools.is_empty() {
        s.push_str(&format!(" --allowedTools '{tools}'"));
    }
    if memory {
        s.push_str(&format!(" --add-dir {}", agent.memory_dir));
    }
    if let Some(id) = session {
        s.push_str(&format!(" --resume {id}"));
    }
    s.push_str("\nrc=$?; rm -f \"$ctx\" \"$msg\"; exit $rc\n");
    s
}

/// A heredoc end marker that appears in none of `texts`.
fn heredoc_tag(texts: &[&str]) -> String {
    let mut n = Utc::now().timestamp_nanos_opt().unwrap_or(0).unsigned_abs();
    loop {
        let tag = format!("AGENTWATCH_END_{n:x}");
        if !texts.iter().any(|t| t.contains(&tag)) {
            return tag;
        }
        n = n.wrapping_mul(6364136223846793005).wrapping_add(1);
    }
}

/// Resumes the tick of each local worker whose chat paused it and has been
/// quiet for [`CHAT_IDLE`]. Called from the poll loop.
pub async fn resume_idle(app: &App) {
    let due: Vec<String> = app
        .chat_paused
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .iter()
        .filter(|(name, at)| {
            at.elapsed() >= CHAT_IDLE
                && !app
                    .chatting
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .contains(*name)
        })
        .map(|(name, _)| name.clone())
        .collect();
    for name in due {
        let Some(agent) = app.agent(&name) else {
            continue;
        };
        match super::actions::set_paused(&agent, false).await {
            Ok(()) => {
                app.chat_paused
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .remove(&name);
                let _ = app.store().add_message(
                    &name,
                    "tool",
                    "Chat idle for 30 minutes: its tick is running again",
                );
            }
            Err(e) => tracing::warn!("{name}: could not resume after chat: {e}"),
        }
    }
}

/// Gives a local worker its ticks back when its terminal paused them and has
/// had nothing to do for [`CHAT_IDLE`], or has closed. The terminal's next
/// prompt pauses them again (agentterm/mac/agentterm-hook).
pub async fn resume_idle_terms(app: &App) {
    let agents = app
        .config
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .agents
        .clone();
    for agent in agents {
        let (Some(host), Some(pause), Some(slug)) =
            (&agent.host, &agent.pause, agentterm::slug(&agent.name))
        else {
            continue;
        };
        // A terminal never opened has nothing to give back.
        let Some(term) = super::poll::term(app, &agent)
            .await
            .filter(|t| t.state.is_some())
        else {
            continue;
        };
        let idle = |s: &agentterm::TermState| {
            s.phase != agentterm::Phase::Working
                && (chrono::Utc::now() - s.at).to_std().unwrap_or_default() >= CHAT_IDLE
        };
        if term.open && !term.state.as_ref().is_some_and(idle) {
            continue;
        }
        let marker = format!("~/.ai-mesh/term/{slug}.paused");
        let script = format!("if [ -e {marker} ]; then rm -f {pause} {marker}; echo resumed; fi");
        match super::run::ssh(host, &script).output().await {
            Ok(o) if String::from_utf8_lossy(&o.stdout).contains("resumed") => {
                tracing::info!("{}: terminal idle, its tick is running again", agent.name);
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("{}: could not resume after terminal: {e}", agent.name),
        }
    }
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
    for e in v.entries.iter().take(5) {
        out.push_str(&format!("- {}: {}\n", e.when, e.text));
    }
    if let Some(e) = &v.error {
        out.push_str(&format!("Problem reading it: {e}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentwatch::config::Config;

    const CONFIG: &str = r#"
[[agent]]
name = "Orchestrator"
kind = "local"
purpose = "p"
host = "mac1"
status = "~/orchestrator-status.md"
pause = "~/orchestrator-paused"
kickstart = "com.jonno.orchestrator-tick"
chat_dir = "~/guv"
chat_env = "~/orchestrator-env.sh"
brief = "~/guv-orchestrator.md"
every = "20m"

[[agent]]
name = "Reviews"
kind = "local"
purpose = "p"
host = "mac1"
reports = "~/.ai-mesh/reviews/reports"
chat_dir = "~/.ai-mesh/reviews"
chat_readonly = true
every = "1d"
"#;

    #[test]
    fn the_orchestrator_script_pauses_its_tick_and_keeps_its_brief() {
        let c = Config::parse(CONFIG).unwrap();
        let s = local_script(
            c.agent("Orchestrator").unwrap(),
            "CONTEXT",
            "hello $(rm -rf ~)",
            Some("abc-123"),
            true,
        );
        assert!(s.starts_with("cd ~/guv || exit 1\n. ~/orchestrator-env.sh\n"));
        assert!(s.contains("launchctl list com.jonno.orchestrator-tick"));
        assert!(s.contains("touch ~/orchestrator-paused; echo AGENTWATCH_PAUSED_BY_CHAT"));
        assert!(s.contains("cat ~/guv-orchestrator.md >> \"$ctx\""));
        assert!(s.contains("--permission-mode auto"));
        assert!(!s.contains("--allowedTools"));
        assert!(s.contains("--add-dir ~/.ai-mesh/memory --resume abc-123"));
        // The message is inside a quoted heredoc, so the shell never runs it.
        let tag = s
            .lines()
            .find_map(|l| l.strip_prefix("cat > \"$msg\" <<'"))
            .unwrap()
            .trim_end_matches('\'');
        assert!(s.contains(&format!("<<'{tag}'\nhello $(rm -rf ~)\n{tag}\n")));
    }

    #[test]
    fn the_reviews_script_is_read_only_and_never_pauses() {
        let c = Config::parse(CONFIG).unwrap();
        let s = local_script(c.agent("Reviews").unwrap(), "C", "m", None, false);
        assert!(s.starts_with("cd ~/.ai-mesh/reviews || exit 1\n"));
        assert!(!s.contains("launchctl") && !s.contains("touch"));
        assert!(s.contains("--permission-mode acceptEdits"));
        assert!(s.contains("Edit(~/.ai-mesh/memory/**)"));
        assert!(!s.contains("--resume") && !s.contains("--add-dir"));
    }

    #[test]
    fn the_heredoc_tag_is_never_in_the_text() {
        let a = heredoc_tag(&["x"]);
        assert!(a.starts_with("AGENTWATCH_END_"));
        let b = heredoc_tag(&[&a]);
        assert_ne!(a, b);
    }
}
