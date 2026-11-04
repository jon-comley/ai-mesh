//! Reading each worker and building what its tile shows.

use agentwatch::config::{Agent, Kind};
use agentwatch::hub::{self, Row};
use agentwatch::state::{self, CloudFacts, LocalFacts, State};
use agentwatch::{log, reports, status};
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
    pub can_chat: bool,
    pub can_run: bool,
    pub can_pause: bool,
    pub paused: bool,
    /// Its terminal on lenovo1 (plans/agent-terminals.md), if it has one.
    pub term: Option<agentterm::TermView>,
    /// A queue worker's machine: on the network at the last check.
    pub machine_on: Option<bool>,
    /// What the tile calls that machine: `wl` for the work laptop.
    pub machine: Option<String>,
    /// A queue worker's tasks, newest first.
    pub tasks: Vec<agentterm::queue::Task>,
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

/// The tile before its first read: shown at once, as "checking".
pub fn blank(agent: &Agent, now: DateTime<Utc>) -> View {
    View {
        name: agent.name.clone(),
        purpose: agent.purpose.clone(),
        kind: agent.kind,
        state: State::Checking,
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
        can_chat: agent.kind == Kind::Cloud || agent.chat_dir.is_some(),
        can_run: match agent.kind {
            Kind::Cloud => agent.routine.is_some(),
            Kind::Local => agent.kickstart.is_some(),
            Kind::Queue => false,
        },
        can_pause: agent.kind == Kind::Local && agent.pause.is_some(),
        paused: false,
        term: None,
        machine_on: None,
        machine: None,
        tasks: Vec::new(),
        checked_at: now,
    }
}

pub async fn view(app: &App, agent: &Agent) -> View {
    let now = Utc::now();
    let run_started = app.store().run_started(&agent.name).ok().flatten();
    let mut v = blank(agent, now);
    v.state = State::New;
    match agent.kind {
        Kind::Cloud => cloud(app, agent, &mut v, run_started, now).await,
        Kind::Local => local(agent, &mut v, run_started, now).await,
        Kind::Queue => queue_view(app, agent, &mut v),
    }
    v.term = term(app, agent).await;
    if agent.kind == Kind::Queue {
        return v;
    }
    v.next_due = state::next_due(
        v.last_run,
        agent.every,
        agent.due.as_deref().and_then(state::Due::parse),
        now,
    );
    v
}

/// The agent's terminal: a clone to work in here, or a worker on its host.
pub async fn term(app: &App, agent: &Agent) -> Option<agentterm::TermView> {
    let slug = agentterm::slug(&agent.name)?;
    let on_host = agent.kind == Kind::Local && agent.host.is_some() && agent.chat_dir.is_some();
    if !on_host && !app.dir.join("chats").join(&slug).is_dir() {
        return None;
    }
    let open = tokio::process::Command::new("tmux")
        .args(["has-session", "-t", &agentterm::tmux_session(&slug)])
        .output()
        .await
        .is_ok_and(|o| o.status.success());
    let today = chrono::Local::now().date_naive();
    Some(agentterm::read(&app.dir, &slug, open, today))
}

/// A queue worker's tile: its tasks, what waits on Jon, and its machine's light.
pub fn queue_view(app: &App, agent: &Agent, v: &mut View) {
    use agentterm::queue::TaskState;
    let Some(slug) = agentterm::slug(&agent.name) else {
        return;
    };
    let mut tasks = agentterm::queue::list(&agentterm::term_dir(&app.dir, &slug));
    tasks.reverse();
    v.machine_on = app
        .machine_on
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&agent.name)
        .copied();
    v.machine = agent.host.as_deref().map(|h| {
        if h == "work-laptop" {
            "wl".to_string()
        } else {
            h.to_string()
        }
    });
    let running = tasks.iter().any(|t| t.state == TaskState::Running);
    v.state = if running { State::Running } else { State::Ok };
    v.last_run = tasks.iter().filter_map(|t| t.finished).max();
    v.waiting_lines = tasks
        .iter()
        .filter(|t| matches!(t.state, TaskState::NeedsYou | TaskState::Interrupted))
        .map(|t| format!("{}: {}", first(&t.task), first(&t.result)))
        .collect();
    v.waiting = v.waiting_lines.len();
    v.line = match tasks.first() {
        Some(t) => format!("{:?}: {}", t.state, first(&t.task)),
        None => "Nothing queued".into(),
    };
    tasks.truncate(20);
    v.tasks = tasks;
}

fn first(text: &str) -> &str {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
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
        &agent.hub,
        agent.every,
        agent.grace,
        agent.due.as_deref().and_then(state::Due::parse),
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
const REPORTS: &str = "@@AGENTWATCH-REPORTS@@";

/// The ssh script that reads a local worker in one go. The paths were checked
/// by Config::parse to hold no shell characters, and stay unquoted so `~`
/// expands on the host. It runs under the host's login shell (zsh on mac1).
fn local_script(agent: &Agent) -> String {
    let mut script = String::from("true");
    if let Some(s) = &agent.status {
        script.push_str(&format!("; head -c 300000 {s} 2>/dev/null"));
    }
    script.push_str(&format!("; printf '\\n{PAUSED}\\n'"));
    if let Some(p) = &agent.pause {
        script.push_str(&format!("; [ -e {p} ] && echo yes || echo no"));
    }
    if let Some(t) = &agent.ticks {
        script.push_str(&format!(
            "; printf '{TICK}\\n'; f=$(ls -t {t} 2>/dev/null | head -1); printf '%s\\n' \"$f\"; [ -n \"$f\" ] && tail -c 3000 {t}/\"$f\""
        ));
    }
    if let Some(r) = &agent.reports {
        script.push_str(&format!(
            "; printf '{REPORTS}\\n'; for f in $(ls -t {r}/*/*.md 2>/dev/null | grep -v '/latest.md$' | head -10); \
             do printf '%s %s %s\\n' '{marker}' \"$(date -r \"$f\" +%s)\" \"$f\"; head -12 \"$f\"; done",
            marker = reports::FILE_MARKER
        ));
    }
    script
}

/// The script's output cut at its markers: (status, paused, tick, reports).
fn split_local(out: &str) -> (&str, &str, Option<&str>, Option<&str>) {
    let (status_text, rest) = out.split_once(PAUSED).unwrap_or((out, ""));
    let (rest, listing) = match rest.split_once(REPORTS) {
        Some((a, b)) => (a, Some(b)),
        None => (rest, None),
    };
    let (paused, tick) = match rest.split_once(TICK) {
        Some((p, t)) => (p, Some(t)),
        None => (rest, None),
    };
    (status_text, paused, tick, listing)
}

async fn local(
    agent: &Agent,
    v: &mut View,
    run_started: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) {
    let host = agent.host.as_deref().expect("checked by Config::parse");
    let mut facts = LocalFacts {
        reachable: true,
        paused: false,
        last_run: None,
        last_tick_failed: false,
        run_started,
    };
    match run::output(run::ssh(host, &local_script(agent)), SSH_LIMIT).await {
        Ok(out) => {
            let (status_text, paused, tick, listing) = split_local(&out);
            facts.paused = paused.trim() == "yes";
            // The tick section starts with the newest log's name, then its tail.
            let (tick_name, tick_tail) = match tick {
                Some(t) => {
                    let t = t.trim_start_matches('\n');
                    t.split_once('\n').unwrap_or((t, ""))
                }
                None => ("", ""),
            };
            facts.last_tick_failed = !tick_name.trim().is_empty() && status::tick_failed(tick_tail);
            let tick_at = status::tick_time(tick_name);
            let steps = status::parse(status_text, now);
            if let Some(top) = steps.first() {
                facts.last_run = Some(top.at);
                v.line = top.summary.clone();
                v.waiting_lines = status::waiting_on_jon(top);
            }
            // A heading may give only the day; the tick log's name has the time.
            if let Some(t) = tick_at
                && facts.last_run.is_none_or(|l| t > l)
            {
                facts.last_run = Some(t);
            }
            v.entries = steps
                .iter()
                .take(10)
                .map(|s| EntryView {
                    when: s
                        .at
                        .with_timezone(&London)
                        .format(if s.timed { "%-d %b %H:%M" } else { "%-d %b" })
                        .to_string(),
                    text: format!("{}: {}", s.kind, s.summary),
                    report: None,
                })
                .collect();
            if let Some(listing) = listing {
                read_reports(listing, v, &mut facts);
            }
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

/// A worker that leaves reports: its last run is its newest report, and what
/// waits on Jon is each repo's confirmed high and medium findings to check.
fn read_reports(listing: &str, v: &mut View, facts: &mut LocalFacts) {
    let all = reports::parse_listing(listing);
    let Some(newest) = all.first() else { return };
    if facts.last_run.is_none_or(|l| newest.at > l) {
        facts.last_run = Some(newest.at);
        v.line = newest.line();
        v.report = Some(newest.path.clone());
    }
    for r in reports::latest_per_repo(&all) {
        if r.high + r.medium > 0 {
            v.waiting_lines.push(format!(
                "{}: {} high, {} medium to check",
                r.repo, r.high, r.medium
            ));
        }
    }
    v.entries.extend(all.iter().map(|r| {
        EntryView {
            when: r
                .at
                .with_timezone(&London)
                .format("%-d %b %H:%M")
                .to_string(),
            text: r.line(),
            report: Some(r.path.clone()),
        }
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_the_script_output_at_its_markers() {
        let out = format!("# STEP x\n{PAUSED}\nno\n{TICK}\nlog tail\n{REPORTS}\nlisting");
        let (s, p, t, r) = split_local(&out);
        assert_eq!(s, "# STEP x\n");
        assert_eq!(p.trim(), "no");
        assert_eq!(t.map(str::trim), Some("log tail"));
        assert_eq!(r.map(str::trim), Some("listing"));
        let out = format!("\n{PAUSED}\n{REPORTS}\nlisting");
        let (s, p, t, r) = split_local(&out);
        assert_eq!(
            (s.trim(), p.trim(), t, r.map(str::trim)),
            ("", "", None, Some("listing"))
        );
    }
}
