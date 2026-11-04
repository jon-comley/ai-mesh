//! `agentterm`: what ttyd runs to open an agent's terminal, and what that
//! terminal's Claude hooks and status line call (plans/agent-terminals.md).
//!
//! - `agentterm attach <agent>`: open or rejoin the agent's tmux session (ttyd's
//!   `?arg=<agent>`).
//! - `agentterm hook prompt|stop|notify|tool`: Claude hooks; stdin is the hook's JSON.
//! - `agentterm statusline`: Claude's status line; stdin is its JSON.
//! - `agentterm queue add <agent> <dir> <task>`: queue work for a machine that is
//!   not always on; `queue list <agent>` shows the queue.
//! - `agentterm summary <file>`: a status file's State and NEXT lines.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

use agentterm::{Limits, Phase, TermState};
use chrono::{Local, Utc};
use serde_json::Value;

fn agents_dir() -> PathBuf {
    std::env::var_os("AGENTTERM_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".ai-mesh/agents")
        })
}

fn stdin_json() -> Value {
    let mut s = String::new();
    let _ = std::io::stdin().read_to_string(&mut s);
    serde_json::from_str(&s).unwrap_or(Value::Null)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["attach", name] => attach(name),
        ["hook", event] => {
            hook(event);
            ExitCode::SUCCESS
        }
        ["queue", "add", agent, dir, task] => queue_add(agent, dir, task),
        ["queue", "list", agent] => {
            let Some(slug) = agentterm::slug(agent) else {
                return ExitCode::from(2);
            };
            for t in agentterm::queue::list(&agentterm::term_dir(&agents_dir(), &slug)) {
                println!(
                    "{} {:?} {}: {}",
                    t.id,
                    t.state,
                    t.dir,
                    t.task.lines().next().unwrap_or("")
                );
                if !t.result.is_empty() {
                    println!("    {}", t.result.lines().next().unwrap_or(""));
                }
            }
            ExitCode::SUCCESS
        }
        ["summary", file] => {
            // Where the worker stands, before Claude starts: no model call.
            let text = std::fs::read_to_string(file).unwrap_or_default();
            println!("{}", agentterm::summary(&text));
            ExitCode::SUCCESS
        }
        ["statusline"] => {
            statusline();
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!(
                "usage: agentterm attach <agent> | hook <event> | statusline | summary <file>"
            );
            ExitCode::from(2)
        }
    }
}

fn attach(name: &str) -> ExitCode {
    let Some(slug) = agentterm::slug(name) else {
        eprintln!("no agent called {name}");
        return ExitCode::from(2);
    };
    let dir = agents_dir();
    let worker = agentterm::host::load(&dir, &slug);
    if let Some(w) = worker.as_ref().filter(|w| w.on_host()) {
        return attach_host(&slug, w, &dir);
    }
    // A session already open keeps its node: tmux -A rejoins it.
    if !session_open(&slug) {
        if let Some(w) = &worker {
            match place(&slug, w, &dir) {
                Ok(node) => {
                    let _ = std::fs::create_dir_all(agentterm::term_dir(&dir, &slug));
                    let _ =
                        std::fs::write(agentterm::term_dir(&dir, &slug).join("node"), &node.name);
                    if !node.local() {
                        return attach_pooled(&slug, w, &node, &dir);
                    }
                }
                Err(why) => return wait_and_fail(&format!("Cannot open {}: {why}.", w.name)),
            }
        }
    }
    let work = dir.join("chats").join(&slug);
    if !work.is_dir() {
        eprintln!("no terminal for {name}: {} is missing", work.display());
        return ExitCode::from(2);
    }
    // A new session starts from what is on main: the clone may be days old.
    if !session_open(&slug) && work.join(".git").exists() {
        let key = agentterm::host::load(&dir, &slug)
            .and_then(|w| w.key)
            .unwrap_or_else(|| slug.clone());
        let pulled = Command::new("git")
            .arg("-C")
            .arg(&work)
            .args(["pull", "-q", "--ff-only"])
            .env(
                "GIT_SSH_COMMAND",
                format!(
                    "ssh -i {} -o IdentitiesOnly=yes -o BatchMode=yes",
                    dir.join("keys").join(key).display()
                ),
            )
            .status();
        if !pulled.is_ok_and(|s| s.success()) {
            eprintln!(
                "Could not pull {}: starting from the clone as it is.",
                work.display()
            );
        }
    }
    let term = dir.join("term");
    let settings = term.join("settings.json");
    let bin = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("agentterm"));
    let written = std::fs::create_dir_all(&term)
        .and_then(|_| std::fs::write(&settings, agentterm::settings(&bin).to_string()));
    if let Err(e) = written {
        eprintln!("cannot write {}: {e}", settings.display());
        return ExitCode::FAILURE;
    }
    let display = {
        let mut c = slug.chars();
        c.next()
            .map(|f| f.to_ascii_uppercase().to_string() + c.as_str())
            .unwrap_or_default()
    };
    let err = Command::new("tmux")
        .args(agentterm::attach_command(
            &slug, &display, &work, &dir, &settings,
        ))
        .exec();
    eprintln!("cannot start tmux: {err}");
    ExitCode::FAILURE
}

fn queue_add(agent: &str, dir: &str, task: &str) -> ExitCode {
    let Some(slug) = agentterm::slug(agent) else {
        eprintln!("no agent called {agent}");
        return ExitCode::from(2);
    };
    match agentterm::queue::add(
        &agentterm::term_dir(&agents_dir(), &slug),
        task,
        dir,
        Utc::now(),
    ) {
        Ok(t) => {
            println!(
                "Queued {}: it runs on the machine as soon as it is on.",
                t.id
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("Not queued: {e}");
            ExitCode::FAILURE
        }
    }
}

fn session_open(slug: &str) -> bool {
    Command::new("tmux")
        .args(["has-session", "-t", &agentterm::tmux_session(slug)])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Shows why in the terminal and waits for Enter, so ttyd does not close
/// before Jon has read it.
fn wait_and_fail(why: &str) -> ExitCode {
    println!("{why}\nPress Enter to close.");
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    ExitCode::FAILURE
}

fn reachable(node: &agentterm::node::Node) -> bool {
    let Some(host) = &node.host else {
        return true;
    };
    Command::new("ssh")
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=3",
            host,
            "exit",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// The pool's choice for a new session of this worker.
fn place(
    slug: &str,
    w: &agentterm::host::Worker,
    dir: &std::path::Path,
) -> Result<agentterm::node::Node, String> {
    let nodes = agentterm::host::nodes(dir);
    let first = nodes.first().map(|n| n.name.clone()).unwrap_or_default();
    // Each node's open sessions, from the node each one was placed on.
    let mut load = std::collections::HashMap::new();
    for entry in std::fs::read_dir(dir.join("term"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let other = entry.file_name().to_string_lossy().to_string();
        if other != slug && entry.path().is_dir() && session_open(&other) {
            let on = std::fs::read_to_string(entry.path().join("node"))
                .unwrap_or_else(|_| first.clone());
            *load.entry(on.trim().to_string()).or_insert(0) += 1;
        }
    }
    let needs = agentterm::node::needs(slug, &w.needs);
    agentterm::node::place(&needs, w.pin.as_deref(), &nodes, reachable, &load).cloned()
}

/// A pooled worker on another node.
fn attach_pooled(
    slug: &str,
    w: &agentterm::host::Worker,
    node: &agentterm::node::Node,
    dir: &std::path::Path,
) -> ExitCode {
    let w = agentterm::host::on_node(w, slug, node);
    if node.os != agentterm::node::Os::Windows {
        return attach_host(slug, &w, dir);
    }
    let host = node.host.as_deref().unwrap_or_default();
    let local = agentterm::term_dir(dir, slug).join("start.ps1");
    let copied = std::fs::write(&local, agentterm::host::script_windows(slug, &w)).is_ok()
        && Command::new("ssh")
            .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10", host])
            .arg("if not exist .ai-mesh\\term mkdir .ai-mesh\\term")
            .status()
            .is_ok_and(|s| s.success())
        && Command::new("scp")
            .args(["-q", "-o", "BatchMode=yes"])
            .arg(&local)
            .arg(format!("{host}:{}", agentterm::host::ps1_path(slug)))
            .status()
            .is_ok_and(|s| s.success());
    if !copied {
        return wait_and_fail(&format!("Cannot reach {host} to start {}.", w.name));
    }
    let err = Command::new("tmux")
        .args(agentterm::host::attach_command_windows(slug, host, dir))
        .exec();
    eprintln!("cannot start tmux: {err}");
    ExitCode::FAILURE
}

/// A worker on another machine: write its start script there, then hold an
/// ssh to it in tmux here.
fn attach_host(slug: &str, w: &agentterm::host::Worker, dir: &std::path::Path) -> ExitCode {
    use std::io::Write;
    let host = w.host.as_deref().unwrap_or_default();
    let path = agentterm::host::script_path(slug);
    let child = Command::new("ssh")
        .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10", host])
        .arg(format!("mkdir -p ~/.ai-mesh/term && cat > {path}"))
        .stdin(std::process::Stdio::piped())
        .spawn();
    let written = child.and_then(|mut c| {
        c.stdin
            .take()
            .expect("piped")
            .write_all(agentterm::host::script(slug, w).as_bytes())?;
        c.wait()
    });
    match written {
        Ok(s) if s.success() => {}
        Ok(s) => {
            eprintln!("cannot reach {host} to start {}: ssh exited {s}", w.name);
            return ExitCode::FAILURE;
        }
        Err(e) => {
            eprintln!("cannot reach {host} to start {}: {e}", w.name);
            return ExitCode::FAILURE;
        }
    }
    let err = Command::new("tmux")
        .args(agentterm::host::attach_command(slug, host, dir))
        .exec();
    eprintln!("cannot start tmux: {err}");
    ExitCode::FAILURE
}

/// Hooks never fail the session: a tile that misses an update is better than
/// a blocked prompt.
fn hook(event: &str) {
    let Some(slug) = std::env::var("AGENTTERM_AGENT")
        .ok()
        .and_then(|a| agentterm::slug(&a))
    else {
        return;
    };
    let dir = agentterm::term_dir(&agents_dir(), &slug);
    let input = stdin_json();
    let state = if event == "tool" {
        // After a permission answer, the tile goes back to working.
        match agentterm::read_state(&dir) {
            Some(s) if s.phase == Phase::NeedsYou => Some(TermState {
                phase: Phase::Working,
                at: Utc::now(),
                line: "carrying on".into(),
            }),
            _ => None,
        }
    } else {
        agentterm::hook_state(event, &input, Utc::now())
    };
    if let Some(s) = state {
        let _ = agentterm::write_state(&dir, &s);
    }
}

fn statusline() {
    let input = stdin_json();
    let s = |p: &str| input.pointer(p).and_then(Value::as_str).unwrap_or("");
    let model = match s("/model/display_name") {
        "" => "Claude",
        m => m,
    };
    let usd = input
        .pointer("/cost/total_cost_usd")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let agents = agents_dir();
    let limits = Limits::load(&agents);
    let (session, today) = match std::env::var("AGENTTERM_AGENT")
        .ok()
        .and_then(|a| agentterm::slug(&a))
    {
        Some(slug) => agentterm::record_cost(
            &agentterm::term_dir(&agents, &slug),
            s("/session_id"),
            usd,
            Local::now().date_naive(),
        )
        .unwrap_or((usd, usd)),
        None => (usd, usd),
    };
    println!("{}", agentterm::status_line(model, session, today, &limits));
    // Where the worker stands, under the prompt for the whole session: Claude
    // clears the screen as it starts, so a summary printed before it is lost.
    let dir = match s("/workspace/current_dir") {
        "" => s("/cwd"),
        d => d,
    };
    if let Ok(text) = std::fs::read_to_string(std::path::Path::new(dir).join("STATUS.md")) {
        for line in agentterm::summary(&text).lines() {
            println!("{}", short(line, 150));
        }
    }
}

fn short(line: &str, max: usize) -> String {
    let cut: String = line.chars().take(max).collect();
    if cut.len() < line.len() {
        format!("{cut}…")
    } else {
        cut
    }
}
