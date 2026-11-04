//! Terminals for workers on another machine (the orchestrator and Reviews on
//! mac1). mac1 has no tmux or Rust, so the tmux session lives on lenovo1 and
//! holds an `ssh -t` to the host, where `claude` runs. On the host two shell
//! scripts do the rest (`agentterm/mac/`): `agentterm-hook` forwards hooks and
//! the status line to `agentterm` on lenovo1, and gates the orchestrator's
//! prompts on its tick.

use std::fs;
use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

/// The `[[agent]]` fields a terminal needs, read from agentwatch's `agents.toml`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Worker {
    pub name: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub reports: Option<String>,
    #[serde(default)]
    pub pause: Option<String>,
    #[serde(default)]
    pub kickstart: Option<String>,
    #[serde(default)]
    pub chat_dir: Option<String>,
    #[serde(default)]
    pub chat_env: Option<String>,
    #[serde(default)]
    pub brief: Option<String>,
    #[serde(default)]
    pub chat_readonly: bool,
    /// Its deploy key under `keys/`, for pulling its clone.
    #[serde(default)]
    pub key: Option<String>,
}

#[derive(Deserialize)]
struct Agents {
    #[serde(default, rename = "agent")]
    agents: Vec<Worker>,
}

impl Worker {
    /// A worker on another machine with somewhere to work there.
    pub fn on_host(&self) -> bool {
        self.kind.as_deref() == Some("local") && self.host.is_some() && self.chat_dir.is_some()
    }
}

/// The worker whose name slugs to `slug`.
pub fn load(agents_dir: &Path, slug: &str) -> Option<Worker> {
    let text = fs::read_to_string(agents_dir.join("agents.toml")).ok()?;
    let agents: Agents = toml::from_str(&text).ok()?;
    agents
        .agents
        .into_iter()
        .find(|w| crate::slug(&w.name).as_deref() == Some(slug))
}

/// Where the scripts live on the host.
pub const HOST_BIN: &str = "~/.ai-mesh/bin";

/// Where the start script for a session is written on the host.
pub fn script_path(slug: &str) -> String {
    format!("~/.ai-mesh/term/{slug}.sh")
}

/// The hooks and status line for a host session: each calls `agentterm-hook`.
pub fn settings(slug: &str, w: &Worker) -> Value {
    let gate = match (&w.kickstart, &w.pause) {
        (Some(label), Some(pause)) => format!(" {label} {pause}"),
        _ => String::new(),
    };
    let cmd = |event: &str| {
        let extra = if event == "prompt" { gate.as_str() } else { "" };
        format!("{HOST_BIN}/agentterm-hook {slug} {event}{extra}")
    };
    crate::settings_with(cmd)
}

/// The file whose lines open a new session, for free.
pub fn status_file(w: &Worker) -> String {
    match (&w.status, &w.reports) {
        (Some(s), _) => s.clone(),
        (None, Some(r)) => format!("$(ls -t {r}/*/latest.md 2>/dev/null | head -n 1)"),
        _ => "STATUS.md".into(),
    }
}

/// What a host session is told, on top of the shared terminal prompt.
pub fn system_prompt(w: &Worker) -> String {
    let mut p = crate::system_prompt(&w.name);
    if w.chat_readonly {
        p.push_str(" You only read: change nothing but the shared memory.");
    }
    if w.kickstart.is_some() {
        p.push_str(
            " Your scheduled ticks are paused while Jon talks to you here, and run again \
after 30 minutes with nothing to do. Never start a gated run or touch a phone unless Jon \
says so in this session.",
        );
    }
    p
}

/// A heredoc end marker that appears in none of `texts`.
fn tag(texts: &[&str]) -> String {
    (0..)
        .map(|n| format!("AGENTTERM_{n}"))
        .find(|t| texts.iter().all(|x| !x.contains(t.as_str())))
        .unwrap_or_default()
}

/// The bash script that starts the session on the host. Paths come from
/// `agents.toml`, which agentwatch checks; the texts go in through quoted
/// heredocs, so the shell reads nothing in them.
pub fn script(slug: &str, w: &Worker) -> String {
    let settings = settings(slug, w).to_string();
    let prompt = system_prompt(w);
    let t = tag(&[&settings, &prompt]);
    let dir = w.chat_dir.as_deref().unwrap_or("~");
    let mut s = String::from(
        "#!/bin/bash\n# Written by agentterm on lenovo1 (plans/agent-terminals.md).\n",
    );
    s.push_str(&format!("cd {dir} || exit 1\n"));
    if let Some(env) = &w.chat_env {
        s.push_str(&format!(". {env}\n"));
    }
    s.push_str("export PATH=\"$HOME/.local/bin:$PATH\" TERM=xterm-256color\n");
    if let (Some(label), Some(pause)) = (&w.kickstart, &w.pause) {
        // Never two workers in one checkout: wait for a running tick, then pause.
        s.push_str(&format!(
            "while launchctl list {label} 2>/dev/null | grep -q '\"PID\"'; do\n  \
             echo 'Waiting for its tick to finish...'; sleep 15\ndone\n\
             if [ ! -e {pause} ]; then touch {pause} ~/.ai-mesh/term/{slug}.paused; fi\n"
        ));
    }
    s.push_str(&format!("settings=$(cat <<'{t}'\n{settings}\n{t}\n)\n"));
    s.push_str(&format!("prompt=$(cat <<'{t}'\n{prompt}\n{t}\n)\n"));
    if let Some(brief) = &w.brief {
        s.push_str(&format!(
            "prompt=\"$prompt\"$'\\n\\n'\"$(cat {brief} 2>/dev/null)\"\n"
        ));
    }
    // Where it stands, for free; Claude costs nothing until Jon types.
    s.push_str(&format!("{}\necho\n", crate::summary_sh(&status_file(w))));
    let mode = if w.chat_readonly {
        "default"
    } else {
        "acceptEdits"
    };
    s.push_str(&format!(
        "claude --model haiku --permission-mode {mode} --settings \"$settings\" \
         --append-system-prompt \"$prompt\"\n"
    ));
    if let Some(pause) = &w.pause {
        // Give the ticks back if this session paused them.
        s.push_str(&format!(
            "if [ -e ~/.ai-mesh/term/{slug}.paused ]; then rm -f {pause} ~/.ai-mesh/term/{slug}.paused; fi\n"
        ));
    }
    s
}

/// The tmux arguments that open (or rejoin) a host worker's terminal.
pub fn attach_command(slug: &str, host: &str, agents_dir: &Path) -> Vec<String> {
    let s = |x: &str| x.to_string();
    vec![
        s("new-session"),
        s("-A"),
        s("-s"),
        crate::tmux_session(slug),
        s("-e"),
        format!("AGENTTERM_AGENT={slug}"),
        s("-e"),
        format!("AGENTTERM_DIR={}", agents_dir.display()),
        s("ssh"),
        s("-t"),
        s(host),
        format!("bash {}", script_path(slug)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn orchestrator() -> Worker {
        toml::from_str::<Agents>(
            r#"
[[agent]]
name = "Orchestrator"
kind = "local"
host = "mac1"
status = "~/orchestrator-status.md"
pause = "~/orchestrator-paused"
kickstart = "com.jonno.orchestrator-tick"
chat_dir = "~/guv"
chat_env = "~/orchestrator-env.sh"
brief = "~/guv-orchestrator.md"

[[agent]]
name = "Reviews"
kind = "local"
host = "mac1"
reports = "~/.ai-mesh/reviews/reports"
chat_dir = "~/.ai-mesh/reviews"
chat_readonly = true
"#,
        )
        .unwrap()
        .agents
        .remove(0)
    }

    #[test]
    fn orchestrator_script_waits_for_its_tick_and_gives_it_back() {
        let s = script("orchestrator", &orchestrator());
        assert!(s.contains("cd ~/guv || exit 1\n. ~/orchestrator-env.sh\n"));
        assert!(s.contains("while launchctl list com.jonno.orchestrator-tick"));
        assert!(s.contains("touch ~/orchestrator-paused ~/.ai-mesh/term/orchestrator.paused"));
        assert!(s.contains("cat ~/guv-orchestrator.md"));
        assert!(s.contains("claude --model haiku --permission-mode acceptEdits"));
        assert!(
            s.trim_end()
                .ends_with("rm -f ~/orchestrator-paused ~/.ai-mesh/term/orchestrator.paused; fi")
        );
    }

    #[test]
    fn only_the_prompt_hook_gates_on_the_tick() {
        let v = settings("orchestrator", &orchestrator());
        let cmd = |k: &str| {
            v.pointer(&format!("/hooks/{k}/0/hooks/0/command"))
                .unwrap()
                .as_str()
                .unwrap()
                .to_string()
        };
        assert_eq!(
            cmd("UserPromptSubmit"),
            "~/.ai-mesh/bin/agentterm-hook orchestrator prompt com.jonno.orchestrator-tick ~/orchestrator-paused"
        );
        assert_eq!(
            cmd("Stop"),
            "~/.ai-mesh/bin/agentterm-hook orchestrator stop"
        );
        assert_eq!(
            v.pointer("/statusLine/command").unwrap(),
            "~/.ai-mesh/bin/agentterm-hook orchestrator statusline"
        );
    }

    #[test]
    fn reviews_reads_only_and_never_pauses() {
        let mut w = orchestrator();
        w.name = "Reviews".into();
        w.kickstart = None;
        w.pause = None;
        w.status = None;
        w.reports = Some("~/.ai-mesh/reviews/reports".into());
        w.chat_readonly = true;
        let s = script("reviews", &w);
        assert!(!s.contains("launchctl"));
        assert!(s.contains("--permission-mode default"));
        assert!(s.contains("ls -t ~/.ai-mesh/reviews/reports/*/latest.md"));
    }

    #[test]
    fn heredoc_tag_never_in_the_text() {
        assert_eq!(tag(&["plain"]), "AGENTTERM_0");
        assert_eq!(tag(&["has AGENTTERM_0 in it"]), "AGENTTERM_1");
    }

    #[test]
    fn attach_holds_an_ssh_to_the_host() {
        let c = attach_command("reviews", "mac1", Path::new("/a"));
        assert_eq!(
            &c[c.len() - 4..],
            ["ssh", "-t", "mac1", "bash ~/.ai-mesh/term/reviews.sh"]
        );
    }
}
