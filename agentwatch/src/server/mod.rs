//! The agent watch page's server on lenovo1 (plans/agent-watch.md). It needs
//! nothing from pi1 or the coordinator.

pub mod actions;
pub mod chat;
pub mod dispatch;
pub mod poll;
pub mod repo;
pub mod run;
pub mod store;
pub mod web;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, RwLock};

use agentwatch::config::{Agent, Config};
use agentwatch::state::State;
use poll::View;
use repo::Repo;
use store::{HistoryRow, Store};

pub struct App {
    /// `~/.ai-mesh/agents`: `agents.toml`, `agents.db`, `keys/`, `repos/`.
    pub dir: PathBuf,
    pub config: RwLock<Config>,
    views: RwLock<HashMap<String, View>>,
    store: Mutex<Store>,
    /// One read or write at a time, so a poll never resets a clone mid-reply.
    pub work: tokio::sync::Mutex<()>,
    /// Workers with an answer on its way: one message at a time each.
    pub chatting: Mutex<std::collections::HashSet<String>>,
    /// Local workers whose tick a chat paused, and when that chat last spoke.
    pub chat_paused: Mutex<HashMap<String, std::time::Instant>>,
    /// The last `/api/live` answer, so many open pages cost one read.
    pub live: tokio::sync::Mutex<Option<(std::time::Instant, serde_json::Value)>>,
    /// Queue workers' machines: reachable at the last dispatch tick.
    pub machine_on: Mutex<HashMap<String, bool>>,
    http: reqwest::Client,
}

impl App {
    pub fn new(dir: PathBuf, config: Config, store: Store) -> App {
        App {
            dir,
            config: RwLock::new(config),
            views: RwLock::new(HashMap::new()),
            store: Mutex::new(store),
            work: tokio::sync::Mutex::new(()),
            chatting: Mutex::new(std::collections::HashSet::new()),
            chat_paused: Mutex::new(HashMap::new()),
            live: tokio::sync::Mutex::new(None),
            machine_on: Mutex::new(HashMap::new()),
            http: reqwest::Client::new(),
        }
    }

    pub fn store(&self) -> MutexGuard<'_, Store> {
        self.store.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn config_path(&self) -> PathBuf {
        self.dir.join("agents.toml")
    }

    pub fn agent(&self, name: &str) -> Option<Agent> {
        self.config
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .agent(name)
            .cloned()
    }

    /// The shared memory's clone on lenovo1, if a memory repo is set.
    pub fn memory_repo(&self) -> Option<Repo> {
        let config = self.config.read().unwrap_or_else(|p| p.into_inner());
        Some(Repo {
            dir: self.dir.join("memory"),
            key: self.dir.join("keys").join(&config.memory_key),
            url: config.memory.clone()?,
        })
    }

    pub fn repo(&self, agent: &Agent) -> Repo {
        Repo {
            dir: repo::clone_dir(&self.dir, &agent.name),
            key: self
                .dir
                .join("keys")
                .join(agent.key.as_deref().unwrap_or_default()),
            url: agent.repo.clone().unwrap_or_default(),
        }
    }

    /// Every tile, in `agents.toml` order; one not read yet shows as checking.
    pub fn views(&self) -> Vec<View> {
        let config = self
            .config
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let views = self.views.read().unwrap_or_else(|p| p.into_inner());
        config
            .agents
            .iter()
            .map(|a| {
                views
                    .get(&a.name)
                    .cloned()
                    .unwrap_or_else(|| poll::blank(a, chrono::Utc::now()))
            })
            .collect()
    }

    /// Reads one worker again, records the change, and pushes when it went
    /// wrong or started waiting on Jon for more. The caller holds `work`.
    pub async fn refresh(&self, agent: &Agent) -> View {
        let mut view = poll::view(self, agent).await;
        let row = HistoryRow {
            at: view.checked_at,
            state: format!("{:?}", view.state).to_lowercase(),
            line: view.line.clone(),
            waiting: view.waiting,
        };
        if let Err(e) = self.store().record(&agent.name, &row) {
            tracing::warn!("{}: could not record history: {e}", agent.name);
        }
        view.history = self
            .store()
            .recent(&agent.name, poll::HISTORY)
            .unwrap_or_default();
        let before = self
            .views
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .get(&agent.name)
            .cloned();
        if let Some(text) = push_text(before.as_ref(), &view) {
            self.push(&agent.name, &text).await;
        }
        self.views
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .insert(agent.name.clone(), view.clone());
        view
    }

    pub async fn refresh_all(&self) {
        let _work = self.work.lock().await;
        let agents = self
            .config
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .agents
            .clone();
        for a in &agents {
            self.refresh(a).await;
        }
        let names: Vec<_> = agents.iter().map(|a| a.name.clone()).collect();
        self.views
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .retain(|k, _| names.contains(k));
    }

    async fn push(&self, title: &str, text: &str) {
        let Some(url) = self
            .config
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .ntfy
            .clone()
        else {
            return;
        };
        let sent = self
            .http
            .post(&url)
            .header("Title", title)
            .body(text.to_string())
            .send()
            .await;
        if let Err(e) = sent.and_then(|r| r.error_for_status()) {
            tracing::warn!("ntfy push failed: {e}");
        }
    }
}

/// What to push about a tile's change, if anything: once when it goes
/// wrong, and when it starts waiting on Jon for more. Not on the first read
/// after a restart, which has nothing to compare with.
fn push_text(before: Option<&View>, after: &View) -> Option<String> {
    let before = before?;
    if let (Some(b), Some(a)) = (&before.term, &after.term) {
        if a.over_session && !b.over_session {
            return Some(format!(
                "terminal session over its limit: £{:.2}",
                a.session_gbp
            ));
        }
        if a.over_day && !b.over_day {
            return Some(format!(
                "terminal over its day limit: £{:.2} today",
                a.today_gbp
            ));
        }
    }
    let bad = |s: State| matches!(s, State::Late | State::Broken | State::Unreachable);
    if bad(after.state) && after.state != before.state {
        let why = after.error.as_deref().unwrap_or(&after.line);
        return Some(format!(
            "{}: {}",
            format!("{:?}", after.state).to_lowercase(),
            why
        ));
    }
    if after.waiting > before.waiting {
        return Some(format!(
            "{} waiting on you (was {})",
            after.waiting, before.waiting
        ));
    }
    None
}
