//! Agent terminals (plans/agent-terminals.md): each worker's tile opens a real
//! `claude` terminal, kept in tmux so the work carries on after Jon leaves.
//!
//! Claude's hooks and status line call the `agentterm` binary, which writes
//! what the tile shows under `<agents dir>/term/<agent>/`:
//! - `state.json`: working, done or needs you, with one line;
//! - `costs/<YYYY-MM-DD>`: what the agent spent that day, in dollars;
//! - `sessions/<session id>`: a session's running cost, to count only the
//!   increase.
//!
//! agentwatch reads it back with [`read`].

pub mod host;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Local, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// What the terminal is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Working,
    Done,
    NeedsYou,
}

/// `state.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TermState {
    pub phase: Phase,
    pub at: DateTime<Utc>,
    /// The last thing it said, or what it is waiting for.
    pub line: String,
}

/// `term.toml` in the agents dir. Every field has a default.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Limits {
    /// Pounds a session may spend before the tile and a push say so.
    pub session_gbp: f64,
    /// Pounds an agent may spend in a day.
    pub day_gbp: f64,
    /// Pounds per dollar: Claude reports cost in dollars.
    pub gbp_per_usd: f64,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            session_gbp: 2.0,
            day_gbp: 10.0,
            gbp_per_usd: 0.79,
        }
    }
}

impl Limits {
    pub fn load(agents_dir: &Path) -> Limits {
        fs::read_to_string(agents_dir.join("term.toml"))
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default()
    }
}

/// What a tile shows about its terminal.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TermView {
    pub state: Option<TermState>,
    /// The tmux session is running.
    pub open: bool,
    pub today_gbp: f64,
    pub month_gbp: f64,
    /// The costliest session today, for the session limit.
    pub session_gbp: f64,
    pub over_session: bool,
    pub over_day: bool,
}

/// An agent's name as a directory and tmux session name: `Desk` is `desk`.
/// Only letters, digits and `-`, so a URL argument can never name a path.
pub fn slug(name: &str) -> Option<String> {
    let s = name.trim().to_ascii_lowercase();
    let ok =
        !s.is_empty() && s.len() <= 32 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    ok.then_some(s)
}

pub fn term_dir(agents_dir: &Path, slug: &str) -> PathBuf {
    agents_dir.join("term").join(slug)
}

pub fn tmux_session(slug: &str) -> String {
    format!("agent-{slug}")
}

pub fn write_state(dir: &Path, state: &TermState) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let tmp = dir.join("state.json.tmp");
    fs::write(&tmp, serde_json::to_vec(state)?)?;
    fs::rename(tmp, dir.join("state.json"))
}

pub fn read_state(dir: &Path) -> Option<TermState> {
    serde_json::from_slice(&fs::read(dir.join("state.json")).ok()?).ok()
}

/// What a hook says, from its event name and the JSON Claude sends on stdin.
/// `None` when the tile should not change: an idle reminder is not "needs you".
pub fn hook_state(event: &str, input: &Value, now: DateTime<Utc>) -> Option<TermState> {
    let s = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or("");
    let (phase, line) = match event {
        "prompt" => (Phase::Working, first_line(s("prompt"))),
        "stop" => {
            let said = match s("last_assistant_message") {
                "" => last_assistant_text(Path::new(s("transcript_path"))).unwrap_or_default(),
                m => m.to_string(),
            };
            (Phase::Done, first_line(&said))
        }
        "notify" => {
            let kind = s("notification_type");
            let msg = s("message");
            let permission = kind == "permission_prompt"
                || (kind.is_empty() && msg.to_ascii_lowercase().contains("permission"));
            if !permission {
                return None;
            }
            (Phase::NeedsYou, first_line(msg))
        }
        _ => return None,
    };
    Some(TermState {
        phase,
        at: now,
        line,
    })
}

/// The first non-empty line, at most 160 characters.
fn first_line(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let short: String = line.chars().take(160).collect();
    if short.len() < line.len() {
        format!("{short}…")
    } else {
        short
    }
}

/// The last assistant text in a transcript (JSON lines).
fn last_assistant_text(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    text.lines().rev().find_map(|l| {
        let v: Value = serde_json::from_str(l).ok()?;
        if v.get("type")?.as_str()? != "assistant" {
            return None;
        }
        let said: Vec<&str> = v
            .pointer("/message/content")?
            .as_array()?
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect();
        (!said.is_empty()).then(|| said.join("\n"))
    })
}

/// Records a status-line update: adds the session's increase since the last
/// one to today's total. Returns the session's cost and today's, in dollars.
pub fn record_cost(
    dir: &Path,
    session: &str,
    session_usd: f64,
    today: NaiveDate,
) -> io::Result<(f64, f64)> {
    let sessions = dir.join("sessions");
    let costs = dir.join("costs");
    fs::create_dir_all(&sessions)?;
    fs::create_dir_all(&costs)?;
    let session_file = sessions.join(slug(session).unwrap_or_else(|| "unknown".into()));
    let before = read_num(&session_file);
    let day_file = costs.join(today.format("%Y-%m-%d").to_string());
    let mut day = read_num(&day_file);
    // A session that restarts its count (a resume) adds nothing until it passes
    // what it had.
    let increase = (session_usd - before).max(0.0);
    if increase > 0.0 {
        day += increase;
        fs::write(&day_file, format!("{day:.6}"))?;
        fs::write(&session_file, format!("{session_usd:.6}"))?;
    }
    Ok((session_usd.max(before), day))
}

fn read_num(path: &Path) -> f64 {
    fs::read_to_string(path)
        .ok()
        .and_then(|t| t.trim().parse().ok())
        .unwrap_or(0.0)
}

/// The status line Claude shows under the prompt.
pub fn status_line(model: &str, session_usd: f64, today_usd: f64, limits: &Limits) -> String {
    let gbp = |usd: f64| usd * limits.gbp_per_usd;
    let mut line = format!(
        "{model} · session £{:.2} · today £{:.2}",
        gbp(session_usd),
        gbp(today_usd)
    );
    if gbp(session_usd) >= limits.session_gbp {
        line.push_str(&format!(
            " · over the £{:.0} session limit",
            limits.session_gbp
        ));
    } else if gbp(today_usd) >= limits.day_gbp {
        line.push_str(&format!(" · over the £{:.0} day limit", limits.day_gbp));
    }
    line
}

/// Everything a tile shows about an agent's terminal.
pub fn read(agents_dir: &Path, slug: &str, open: bool, today: NaiveDate) -> TermView {
    let dir = term_dir(agents_dir, slug);
    let limits = Limits::load(agents_dir);
    let month = today.format("%Y-%m-").to_string();
    let day_name = today.format("%Y-%m-%d").to_string();
    let (mut today_usd, mut month_usd) = (0.0, 0.0);
    for entry in fs::read_dir(dir.join("costs"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with(&month) {
            let n = read_num(&entry.path());
            month_usd += n;
            if name == day_name {
                today_usd = n;
            }
        }
    }
    let mut session_usd: f64 = 0.0;
    for entry in fs::read_dir(dir.join("sessions"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let touched_today = entry
            .metadata()
            .and_then(|m| m.modified())
            .map(|t| DateTime::<Local>::from(t).date_naive() == today)
            .unwrap_or(false);
        if touched_today {
            session_usd = session_usd.max(read_num(&entry.path()));
        }
    }
    let g = limits.gbp_per_usd;
    TermView {
        state: read_state(&dir),
        open,
        today_gbp: today_usd * g,
        month_gbp: month_usd * g,
        session_gbp: session_usd * g,
        over_session: session_usd * g >= limits.session_gbp,
        over_day: today_usd * g >= limits.day_gbp,
    }
}

/// What the terminal session is told about itself, on top of Claude Code's own.
pub fn system_prompt(name: &str) -> String {
    format!(
        "You are the {name} agent's terminal, talking to Jon. You run on Haiku to keep costs \
down. Do checks, summaries and small edits yourself. When a task needs more (writing a \
document, code, careful multi-step work), say so in one line first, \"Sonnet: <reason>\", \
then hand it to one subagent with the Task tool, model sonnet, and wait for it. Never use \
Opus unless Jon asks for it. Follow agent-brief.md in this repository for writing, git and \
memory, and its rules on what you may change. Jon may leave while you work: carry on with \
what was agreed, and when you need his yes, ask it as one question and stop. Replies: one \
line per point, no rationale, then the question if there is one. Never use an em dash."
    )
}

/// What a new terminal shows before Claude starts, with no model call: the
/// `State:` and `NEXT:` lines of a status file, else its first line, cut short.
pub fn summary(status: &str) -> String {
    let picked: Vec<&str> = status
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("State:") || l.starts_with("NEXT:"))
        .collect();
    if !picked.is_empty() {
        return picked.join("\n");
    }
    let first = status
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let short: String = first.chars().take(400).collect();
    if short.len() < first.len() {
        format!("{short}…")
    } else {
        short
    }
}

/// The shell snippet a host runs for [`summary`]: it has no agentterm.
pub fn summary_sh(file: &str) -> String {
    format!(
        "grep -E '^(State|NEXT):' {file} 2>/dev/null || head -n 1 {file} 2>/dev/null | cut -c1-400"
    )
}

/// Claude's settings for a terminal session: hooks and the status line, all
/// calling this binary.
pub fn settings(bin: &Path) -> Value {
    settings_with(|event| match event {
        "statusline" => format!("{} statusline", bin.display()),
        e => format!("{} hook {e}", bin.display()),
    })
}

/// Hooks and status line, each event's command from `cmd`: `prompt`, `stop`,
/// `notify`, `tool` and `statusline`.
pub fn settings_with(cmd: impl Fn(&str) -> String) -> Value {
    let hook = |event: &str| serde_json::json!([{ "hooks": [{ "type": "command", "command": cmd(event) }] }]);
    serde_json::json!({
        "hooks": {
            "UserPromptSubmit": hook("prompt"),
            "Stop": hook("stop"),
            "Notification": hook("notify"),
            "PostToolUse": hook("tool"),
        },
        "statusLine": { "type": "command", "command": cmd("statusline") },
    })
}

/// The tmux command that opens (or rejoins) an agent's terminal: `tmux` and
/// its arguments.
pub fn attach_command(
    slug: &str,
    name: &str,
    work_dir: &Path,
    agents_dir: &Path,
    settings_file: &Path,
) -> Vec<String> {
    let s = |x: &str| x.to_string();
    vec![
        s("new-session"),
        s("-A"),
        s("-s"),
        tmux_session(slug),
        s("-c"),
        work_dir.display().to_string(),
        s("-e"),
        format!("AGENTTERM_AGENT={slug}"),
        s("-e"),
        format!("AGENTTERM_DIR={}", agents_dir.display()),
        // Show where it stands for free, then start Claude, which costs nothing until Jon types.
        s("sh"),
        s("-c"),
        s("agentterm summary STATUS.md; echo; exec \"$@\""),
        s("sh"),
        s("claude"),
        s("--model"),
        s("haiku"),
        s("--permission-mode"),
        s("acceptEdits"),
        s("--settings"),
        settings_file.display().to_string(),
        s("--append-system-prompt"),
        system_prompt(name),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn now() -> DateTime<Utc> {
        "2026-11-04T12:00:00Z".parse().unwrap()
    }

    #[test]
    fn slug_refuses_paths() {
        assert_eq!(slug("Desk").as_deref(), Some("desk"));
        assert_eq!(slug("../etc"), None);
        assert_eq!(slug("a b"), None);
        assert_eq!(slug(""), None);
    }

    #[test]
    fn prompt_is_working() {
        let s = hook_state("prompt", &json!({"prompt": "\n do the hub\nmore"}), now()).unwrap();
        assert_eq!(s.phase, Phase::Working);
        assert_eq!(s.line, "do the hub");
    }

    #[test]
    fn stop_uses_last_message() {
        let s = hook_state(
            "stop",
            &json!({"last_assistant_message": "Done: hub rewritten."}),
            now(),
        )
        .unwrap();
        assert_eq!(s.phase, Phase::Done);
        assert_eq!(s.line, "Done: hub rewritten.");
    }

    #[test]
    fn stop_reads_transcript() {
        let dir = tempfile::tempdir().unwrap();
        let t = dir.path().join("t.jsonl");
        fs::write(
            &t,
            [
                json!({"type":"assistant","message":{"content":[{"type":"text","text":"old"}]}}),
                json!({"type":"assistant","message":{"content":[{"type":"text","text":"Pushed a5bba0b."}]}}),
                json!({"type":"user","message":{"content":"tool result"}}),
            ]
            .map(|v| v.to_string())
            .join("\n"),
        )
        .unwrap();
        let s = hook_state("stop", &json!({"transcript_path": t}), now()).unwrap();
        assert_eq!(s.line, "Pushed a5bba0b.");
    }

    #[test]
    fn only_permission_notices_need_jon() {
        let idle = json!({"message": "Claude is waiting for your input", "notification_type": "idle_prompt"});
        assert_eq!(hook_state("notify", &idle, now()), None);
        let ask = json!({"message": "Claude needs your permission to use Bash"});
        assert_eq!(
            hook_state("notify", &ask, now()).unwrap().phase,
            Phase::NeedsYou
        );
    }

    #[test]
    fn costs_count_increases_once() {
        let dir = tempfile::tempdir().unwrap();
        let day = NaiveDate::from_ymd_opt(2026, 11, 4).unwrap();
        record_cost(dir.path(), "s1", 0.5, day).unwrap();
        record_cost(dir.path(), "s1", 0.5, day).unwrap();
        let (_, today) = record_cost(dir.path(), "s1", 1.25, day).unwrap();
        assert!((today - 1.25).abs() < 1e-9);
        let (_, today) = record_cost(dir.path(), "s2", 0.75, day).unwrap();
        assert!((today - 2.0).abs() < 1e-9);
        // A resumed session that starts counting again adds nothing until it passes.
        let (_, today) = record_cost(dir.path(), "s1", 0.1, day).unwrap();
        assert!((today - 2.0).abs() < 1e-9);
    }

    #[test]
    fn read_sums_the_month_and_flags_limits() {
        let agents = tempfile::tempdir().unwrap();
        let dir = term_dir(agents.path(), "desk");
        let today = NaiveDate::from_ymd_opt(2026, 11, 4).unwrap();
        fs::create_dir_all(dir.join("costs")).unwrap();
        fs::write(dir.join("costs/2026-11-03"), "4").unwrap();
        fs::write(dir.join("costs/2026-10-31"), "100").unwrap();
        record_cost(&dir, "s1", 13.0, today).unwrap();
        let v = read(agents.path(), "desk", true, today);
        assert!((v.today_gbp - 13.0 * 0.79).abs() < 1e-9);
        assert!((v.month_gbp - 17.0 * 0.79).abs() < 1e-9);
        assert!(v.over_day && v.over_session);
    }

    #[test]
    fn summary_needs_no_model() {
        assert_eq!(
            summary("# Desk\n\nChecked: x\nState: 5 waiting\nNEXT: Hiscox\n"),
            "State: 5 waiting\nNEXT: Hiscox"
        );
        assert_eq!(
            summary("\n# STEP 4 Nov: NO CHANGE.\nmore"),
            "# STEP 4 Nov: NO CHANGE."
        );
    }

    #[test]
    fn status_line_says_over() {
        let l = Limits::default();
        assert_eq!(
            status_line("Haiku", 0.1, 0.2, &l),
            "Haiku · session £0.08 · today £0.16"
        );
        assert!(status_line("Haiku", 3.0, 3.0, &l).ends_with("over the £2 session limit"));
    }

    #[test]
    fn attach_starts_haiku_in_tmux() {
        let c = attach_command(
            "desk",
            "Desk",
            Path::new("/w"),
            Path::new("/a"),
            Path::new("/a/term/settings.json"),
        );
        assert_eq!(&c[..4], ["new-session", "-A", "-s", "agent-desk"]);
        assert!(c.windows(2).any(|w| w == ["--model", "haiku"]));
        assert!(c.contains(&"agentterm summary STATUS.md; echo; exec \"$@\"".to_string()));
    }
}
