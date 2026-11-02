//! Reading each worker and building what its tile shows.

use agentwatch::config::{Agent, Kind};
use agentwatch::hub::{self, Row};
use agentwatch::state::{self, CloudFacts, LocalFacts, State};
use agentwatch::{log, status};
use chrono::{DateTime, Utc};
use chrono_tz::Europe::London;
use serde::Serialize;

use super::App;
use super::repo::Repo;
use super::run::{self, SSH_LIMIT};
use super::store::HistoryRow;

/// Everything one tile and its detail page show.
#[derive(Debug, Clone, Serialize)]
pub struct View {
    pub name: String,
    pub purpose: String,
    pub kind: Kind,
    pub state: State,
    /// How often it runs, as in `agents.toml`: `7d`, `20m`.
    pub every: String,
    pub last_run: Option<DateTime<Utc>>,
    pub next_due: Option<DateTime<Utc>>,
    /// The newest log line or status step.
    pub line: String,
    /// The latest report's path in the repo.
    pub report: Option<String>,
    /// The hub rows under its "needs" heading, answered ones included.
    pub rows: Vec<Row>,
    /// What it is waiting on Jon for, for a local worker (it has no hub).
    pub waiting_lines: Vec<String>,
    pub waiting: usize,
    /// Its own log, newest first.
    pub entries: Vec<EntryView>,
    pub history: Vec<HistoryRow>,
    pub error: Option<String>,
    pub repo_url: Option<String>,
    pub hub_url: Option<String>,
    pub can_reply: bool,
    pub can_run: bool,
    pub can_pause: bool,
    pub paused: bool,
    pub checked_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EntryView {
    pub when: String,
    pub text: String,
    pub report: Option<String>,
}

const ENTRIES: usize = 20;
pub const HISTORY: usize = 30;

pub async fn view(app: &App, agent: &Agent) -> View {
    let now = Utc::now();
    let run_started = app.store().run_started(&agent.name).ok().flatten();
    let mut v = View {
        name: agent.name.clone(),
        purpose: agent.purpose.clone(),
        kind: agent.kind,
        state: State::New,
        every: agentwatch::config::format_duration(agent.every),
        last_run: None,
        next_due: None,
        line: String::new(),
        report: None,
        rows: Vec::new(),
        waiting_lines: Vec::new(),
        waiting: 0,
        entries: Vec::new(),
        history: Vec::new(),
        error: None,
        repo_url: agent.web_url(),
        hub_url: agent
            .web_url()
            .map(|u| format!("{u}/blob/main/{}", agent.hub)),
        can_reply: agent.kind == Kind::Cloud,
        can_run: match agent.kind {
            Kind::Cloud => agent.routine.is_some(),
            Kind::Local => agent.kickstart.is_some(),
        },
        can_pause: agent.kind == Kind::Local && agent.pause.is_some(),
        paused: false,
        checked_at: now,
    };
    match agent.kind {
        Kind::Cloud => cloud(app, agent, &mut v, run_started, now).await,
        Kind::Local => local(agent, &mut v, run_started, now).await,
    }
    v.next_due = state::next_due(v.last_run, agent.every);
    v
}

async fn cloud(
    app: &App,
    agent: &Agent,
    v: &mut View,
    run_started: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) {
    let repo = app.repo(agent);
    let mut facts = CloudFacts {
        reachable: true,
        last_run: None,
        commits_since: Vec::new(),
        run_started,
    };
    match read_cloud(&repo, agent, v).await {
        Ok((last, since)) => {
            facts.last_run = last;
            facts.commits_since = since;
        }
        Err(e) => {
            facts.reachable = false;
            v.error = Some(e);
        }
    }
    v.last_run = facts.last_run;
    v.state = state::cloud(
        &facts,
        &agent.commit_prefix(),
        agent.every,
        agent.grace,
        now,
    );
}

type CloudRead = (Option<DateTime<Utc>>, Vec<state::Commit>);

async fn read_cloud(repo: &Repo, agent: &Agent, v: &mut View) -> run::Result<CloudRead> {
    repo.sync().await?;
    let entries = log::parse(&repo.read("agent-log.md").unwrap_or_default());
    if let Some(top) = entries.first() {
        v.line = top.summary.clone();
        v.report = top.report.clone();
    }
    v.entries = entries
        .into_iter()
        .take(ENTRIES)
        .map(|e| EntryView {
            when: e.date.format("%-d %b %Y").to_string(),
            text: e.summary,
            report: e.report,
        })
        .collect();
    let hub = repo.read(&agent.hub).unwrap_or_default();
    v.rows = hub::rows(&hub, &agent.needs_heading);
    v.waiting = v.rows.iter().filter(|r| r.waiting()).count();
    let Some((hash, at)) = repo.last_change("agent-log.md").await? else {
        return Ok((None, Vec::new()));
    };
    Ok((Some(at), repo.commits_since(&hash).await?))
}

const PAUSED: &str = "@@AGENTWATCH-PAUSED@@";
const TICK: &str = "@@AGENTWATCH-TICK@@";

async fn local(
    agent: &Agent,
    v: &mut View,
    run_started: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) {
    let host = agent.host.as_deref().expect("checked by Config::parse");
    let status_path = agent.status.as_deref().expect("checked by Config::parse");
    // The paths were checked by Config::parse to hold no shell characters, and
    // stay unquoted so `~` expands on the host.
    let mut script = format!("head -c 300000 {status_path} 2>/dev/null; printf '\\n{PAUSED}\\n'");
    if let Some(p) = &agent.pause {
        script.push_str(&format!("; [ -e {p} ] && echo yes || echo no"));
    }
    if let Some(t) = &agent.ticks {
        script.push_str(&format!(
            "; printf '{TICK}\\n'; f=$(ls -t {t} 2>/dev/null | head -1); [ -n \"$f\" ] && tail -c 3000 {t}/\"$f\""
        ));
    }
    let mut facts = LocalFacts {
        reachable: true,
        paused: false,
        last_run: None,
        last_tick_failed: false,
        run_started,
    };
    match run::output(run::ssh(host, &script), SSH_LIMIT).await {
        Ok(out) => {
            let (status_text, rest) = out.split_once(PAUSED).unwrap_or((&out, ""));
            let (paused, tick) = match rest.split_once(TICK) {
                Some((p, t)) => (p, Some(t)),
                None => (rest, None),
            };
            facts.paused = paused.trim() == "yes";
            facts.last_tick_failed =
                tick.is_some_and(|t| !t.trim().is_empty() && status::tick_failed(t));
            let steps = status::parse(status_text, now);
            if let Some(top) = steps.first() {
                facts.last_run = Some(top.at);
                v.line = top.summary.clone();
                v.waiting_lines = status::waiting_on_jon(top);
            }
            v.entries = steps
                .iter()
                .take(10)
                .map(|s| EntryView {
                    when: s
                        .at
                        .with_timezone(&London)
                        .format("%-d %b %H:%M")
                        .to_string(),
                    text: format!("{}: {}", s.kind, s.summary),
                    report: None,
                })
                .collect();
        }
        Err(e) => {
            facts.reachable = false;
            v.error = Some(e);
        }
    }
    v.paused = facts.paused;
    v.waiting = v.waiting_lines.len();
    v.last_run = facts.last_run;
    v.state = state::local(&facts, agent.every, agent.grace, now);
}
