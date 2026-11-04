//! `agentterm`: what ttyd runs to open an agent's terminal, and what that
//! terminal's Claude hooks and status line call (plans/agent-terminals.md).
//!
//! - `agentterm attach <agent>`: open or rejoin the agent's tmux session (ttyd's
//!   `?arg=<agent>`).
//! - `agentterm hook prompt|stop|notify|tool`: Claude hooks; stdin is the hook's JSON.
//! - `agentterm statusline`: Claude's status line; stdin is its JSON.

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
        ["statusline"] => {
            statusline();
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("usage: agentterm attach <agent> | hook <event> | statusline");
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
    if let Some(w) = agentterm::host::load(&dir, &slug).filter(|w| w.on_host()) {
        return attach_host(&slug, &w, &dir);
    }
    let work = dir.join("chats").join(&slug);
    if !work.is_dir() {
        eprintln!("no terminal for {name}: {} is missing", work.display());
        return ExitCode::from(2);
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
}
