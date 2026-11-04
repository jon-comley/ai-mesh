//! What the page's buttons do: reply in the hub, Run now, Pause and Resume.

use std::time::Duration;

use agentwatch::config::{Agent, Kind};
use agentwatch::hub::{self, ReplyError};
use chrono::Utc;
use tokio::process::Command;

use super::App;
use super::run::{self, Result, SSH_LIMIT};

/// Writes Jon's reply into row `number` of the agent's hub and pushes it.
/// If the agent pushed in between, it starts again from GitHub's copy once.
pub async fn reply(app: &App, agent: &Agent, number: &str, text: &str) -> Result<()> {
    if agent.kind != Kind::Cloud {
        return Err(format!("{} has no hub to reply in", agent.name));
    }
    let repo = app.repo(agent);
    let mut last_err = String::new();
    for _ in 0..2 {
        repo.sync().await?;
        let current = repo
            .read(&agent.hub)
            .ok_or_else(|| format!("{} is not in the repo", agent.hub))?;
        let updated =
            hub::set_reply(&current, &agent.needs_heading, number, text).map_err(|e| match e {
                ReplyError::NoSuchRow => {
                    format!("there is no row {number} under \"{}\"", agent.needs_heading)
                }
                ReplyError::BadText => {
                    "a reply cannot be empty or hold a | or a line break".to_string()
                }
            })?;
        match repo
            .commit_and_push(&agent.hub, &updated, "Hub: reply from Jon")
            .await
        {
            Ok(()) => return Ok(()),
            Err(e) => last_err = e,
        }
    }
    Err(last_err)
}

/// Starts the worker now. A cloud routine is started through `claude` on this
/// machine (its RemoteTrigger tool); a local worker by kicking its launchd job.
pub async fn run_now(app: &App, agent: &Agent) -> Result<()> {
    match agent.kind {
        Kind::Cloud => {
            let id = agent
                .routine
                .as_deref()
                .ok_or_else(|| format!("{} has no routine id", agent.name))?;
            start_routine(id).await?;
        }
        Kind::Queue => {
            return Err(format!(
                "{} runs what is queued, not on a schedule",
                agent.name
            ));
        }
        Kind::Local => {
            let host = agent.host.as_deref().expect("checked by Config::parse");
            let label = agent
                .kickstart
                .as_deref()
                .ok_or_else(|| format!("{} has no kickstart label", agent.name))?;
            run::output(
                run::ssh(host, &format!("launchctl kickstart gui/$(id -u)/{label}")),
                SSH_LIMIT,
            )
            .await?;
        }
    }
    app.store()
        .set_run_started(&agent.name, Utc::now())
        .map_err(|e| e.to_string())
}

async fn start_routine(id: &str) -> Result<()> {
    let prompt = format!(
        "Start the Claude Code routine {id} now: call the RemoteTrigger tool with action \"run\" and \
         trigger_id \"{id}\". Do nothing else. Reply with only OK if it started, or the error it gave."
    );
    let mut cmd = Command::new("claude");
    cmd.args(["-p", &prompt, "--allowedTools", "RemoteTrigger,ToolSearch"]);
    let out = run::output(cmd, Duration::from_secs(180))
        .await
        .map_err(|e| {
            if e.contains("No such file") {
                "claude is not installed on this machine yet (plans/agent-watch.md, Run now)"
                    .to_string()
            } else {
                e
            }
        })?;
    if out.trim().starts_with("OK") {
        Ok(())
    } else {
        Err(format!("the routine did not start: {}", out.trim()))
    }
}

/// Creates (pause) or removes (resume) a local worker's pause file.
pub async fn set_paused(agent: &Agent, paused: bool) -> Result<()> {
    let host = agent
        .host
        .as_deref()
        .ok_or_else(|| format!("{} runs in the cloud", agent.name))?;
    let path = agent
        .pause
        .as_deref()
        .ok_or_else(|| format!("{} has no pause file", agent.name))?;
    let script = if paused {
        format!("touch {path}")
    } else {
        format!("rm -f {path}")
    };
    run::output(run::ssh(host, &script), SSH_LIMIT)
        .await
        .map(|_| ())
}
