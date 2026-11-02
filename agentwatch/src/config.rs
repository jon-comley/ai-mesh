//! `agents.toml`: the list of workers the page shows.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Where a worker runs, which decides how its state is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// A scheduled cloud routine that writes to its own repo.
    #[default]
    Cloud,
    /// A worker on one of our machines, read from its status file over ssh.
    Local,
}

/// One `[[agent]]` entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Agent {
    pub name: String,
    /// One line saying what the agent is, shown on its tile.
    pub purpose: String,
    #[serde(default)]
    pub kind: Kind,

    // Cloud agents.
    /// `git@github.com:<owner>/<repo>.git`.
    #[serde(default)]
    pub repo: Option<String>,
    /// File name of its deploy key under `keys/`.
    #[serde(default)]
    pub key: Option<String>,
    /// The hub file, relative to the repo root.
    #[serde(default = "default_hub")]
    pub hub: String,
    /// The hub heading whose rows are waiting on Jon.
    #[serde(default = "default_needs_heading")]
    pub needs_heading: String,
    /// The cloud routine's id, for Run now.
    #[serde(default)]
    pub routine: Option<String>,

    // Local workers.
    #[serde(default)]
    pub host: Option<String>,
    /// Path of its status file on `host`.
    #[serde(default)]
    pub status: Option<String>,
    /// Path of its pause file on `host`.
    #[serde(default)]
    pub pause: Option<String>,
    /// Directory of its tick logs on `host`; the newest says whether the last
    /// tick failed.
    #[serde(default)]
    pub ticks: Option<String>,
    /// Directory of its reports on `host`, one folder per repo
    /// (`~/.ai-mesh/reviews/reports`), for a worker that leaves reports
    /// instead of a status file: the code reviews on mac1.
    #[serde(default)]
    pub reports: Option<String>,
    /// Its launchd label, for Run now (`launchctl kickstart`).
    #[serde(default)]
    pub kickstart: Option<String>,

    // Chat with a local worker, which runs `claude` on `host`.
    /// Where the chat works on `host` (`~/guv`).
    #[serde(default)]
    pub chat_dir: Option<String>,
    /// A file sourced first (`~/orchestrator-env.sh`).
    #[serde(default)]
    pub chat_env: Option<String>,
    /// Its standing brief on `host` (`~/guv-orchestrator.md`).
    #[serde(default)]
    pub brief: Option<String>,
    /// The chat may read but not change anything.
    #[serde(default)]
    pub chat_readonly: bool,
    /// The memory clone on `host`.
    #[serde(default = "default_memory_dir")]
    pub memory_dir: String,

    /// How often it is meant to run: `7d`, `1d`, `20m`.
    #[serde(with = "duration_text")]
    pub every: Duration,
    /// How long past `every` before it counts as late.
    #[serde(with = "duration_text", default = "default_grace")]
    pub grace: Duration,
}

fn default_hub() -> String {
    "HUB.md".into()
}

fn default_needs_heading() -> String {
    "Needs you".into()
}

fn default_memory_dir() -> String {
    "~/.ai-mesh/memory".into()
}

fn default_grace() -> Duration {
    Duration::from_secs(6 * 3600)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default, rename = "agent")]
    pub agents: Vec<Agent>,
    /// The ntfy topic URL for state-change pushes.
    #[serde(default)]
    pub ntfy: Option<String>,
    /// The shared memory repo every worker reads (`git@github.com:jon-comley/memory.git`).
    #[serde(default)]
    pub memory: Option<String>,
    /// Its deploy key under `keys/`.
    #[serde(default = "default_memory_key")]
    pub memory_key: String,
}

fn default_memory_key() -> String {
    "memory".into()
}

impl Default for Config {
    fn default() -> Config {
        Config {
            agents: Vec::new(),
            ntfy: None,
            memory: None,
            memory_key: default_memory_key(),
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct ConfigError(pub String);

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

impl Config {
    /// Parses `agents.toml` and checks each agent has what its kind needs.
    pub fn parse(text: &str) -> Result<Config, ConfigError> {
        let config: Config = toml::from_str(text).map_err(|e| ConfigError(e.to_string()))?;
        let mut seen = std::collections::HashSet::new();
        for a in &config.agents {
            if !seen.insert(a.name.to_lowercase()) {
                return Err(ConfigError(format!("two agents are called {}", a.name)));
            }
            let missing = match a.kind {
                Kind::Cloud => [("repo", a.repo.is_none()), ("key", a.key.is_none())],
                Kind::Local => [
                    ("host", a.host.is_none()),
                    ("status", a.status.is_none() && a.reports.is_none()),
                ],
            };
            if let Some((field, _)) = missing.iter().find(|(_, m)| *m) {
                return Err(ConfigError(format!("{} has no {field}", a.name)));
            }
            check(a)?;
        }
        Ok(config)
    }

    pub fn to_text(&self) -> String {
        toml::to_string_pretty(self).expect("config always serialises")
    }

    pub fn agent(&self, name: &str) -> Option<&Agent> {
        self.agents
            .iter()
            .find(|a| a.name.eq_ignore_ascii_case(name))
    }
}

impl Agent {
    /// How the agent's own commits start: `Desk:` for Desk.
    pub fn commit_prefix(&self) -> String {
        format!("{}:", self.name)
    }

    /// `https://github.com/<owner>/<repo>` for a `git@github.com:` repo.
    pub fn web_url(&self) -> Option<String> {
        let path = self.repo.as_deref()?.strip_prefix("git@github.com:")?;
        Some(format!(
            "https://github.com/{}",
            path.strip_suffix(".git").unwrap_or(path)
        ))
    }
}

/// The values that end up in a command line (ssh, git, launchctl) are checked
/// here, so a typo in `agents.toml` cannot run anything unexpected.
fn check(a: &Agent) -> Result<(), ConfigError> {
    let bad = |field: &str| {
        Err(ConfigError(format!(
            "{}: {field} has characters it cannot have",
            a.name
        )))
    };
    let name_ok = !a.name.is_empty()
        && a.name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == ' ');
    if !name_ok {
        return bad("name");
    }
    fn path_ok(p: &str) -> bool {
        !p.is_empty()
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || "~._/-".contains(c))
    }
    fn word_ok(w: &str) -> bool {
        !w.is_empty()
            && w.chars()
                .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
    }
    type Check<'a> = (&'a str, &'a Option<String>, fn(&str) -> bool);
    let checks: [Check; 11] = [
        ("host", &a.host, word_ok),
        ("key", &a.key, word_ok),
        ("kickstart", &a.kickstart, word_ok),
        ("routine", &a.routine, word_ok),
        ("status", &a.status, path_ok),
        ("pause", &a.pause, path_ok),
        ("ticks", &a.ticks, path_ok),
        ("reports", &a.reports, path_ok),
        ("chat_dir", &a.chat_dir, path_ok),
        ("chat_env", &a.chat_env, path_ok),
        ("brief", &a.brief, path_ok),
    ];
    for (field, value, ok) in checks {
        if value.as_deref().is_some_and(|v| !ok(v)) {
            return bad(field);
        }
    }
    if !path_ok(&a.memory_dir) {
        return bad("memory_dir");
    }
    if !path_ok(&a.hub) || a.hub.contains("..") {
        return bad("hub");
    }
    if let Some(repo) = &a.repo {
        let rest = repo.strip_prefix("git@github.com:").unwrap_or("");
        let ok = rest.split('/').count() == 2 && path_ok(rest) && !rest.contains("..");
        if !ok {
            return Err(ConfigError(format!(
                "{}: repo must be git@github.com:<owner>/<repo>.git",
                a.name
            )));
        }
    }
    Ok(())
}

/// Reads `30m`, `6h`, `7d` (a whole number and one unit).
pub fn parse_duration(text: &str) -> Option<Duration> {
    let text = text.trim();
    let unit = text.chars().last()?;
    let n: u64 = text[..text.len() - unit.len_utf8()].parse().ok()?;
    let secs = match unit {
        'm' => 60,
        'h' => 3600,
        'd' => 86_400,
        _ => return None,
    };
    Some(Duration::from_secs(n * secs))
}

/// The reverse of [`parse_duration`], in the largest whole unit.
pub fn format_duration(d: Duration) -> String {
    let s = d.as_secs();
    if s.is_multiple_of(86_400) {
        format!("{}d", s / 86_400)
    } else if s.is_multiple_of(3600) {
        format!("{}h", s / 3600)
    } else {
        format!("{}m", s / 60)
    }
}

mod duration_text {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&super::format_duration(*d))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        let text = String::deserialize(d)?;
        super::parse_duration(&text).ok_or_else(|| {
            serde::de::Error::custom(format!("not a duration: {text:?} (use 30m, 6h or 7d)"))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"
ntfy = "https://ntfy.sh/example"

[[agent]]
name = "Standards"
purpose = "Writes the Cyber Essentials, UK GDPR, ISO 27001 and ISO 9001 documents"
repo = "git@github.com:jon-comley/standards.git"
key = "standards"
every = "7d"

[[agent]]
name = "Desk"
purpose = "What only Jon can do today"
repo = "git@github.com:jon-comley/desk.git"
key = "desk"
needs_heading = "Only you"
every = "1d"
grace = "3h"

[[agent]]
name = "Orchestrator"
kind = "local"
purpose = "Runs the gated Maestro tests on guv"
host = "mac1"
status = "~/orchestrator-status.md"
pause = "~/orchestrator-paused"
every = "20m"
grace = "10m"
"#;

    #[test]
    fn reads_all_three_kinds_with_defaults() {
        let c = Config::parse(EXAMPLE).unwrap();
        assert_eq!(c.agents.len(), 3);
        let s = c.agent("standards").unwrap();
        assert_eq!(s.kind, Kind::Cloud);
        assert_eq!(s.hub, "HUB.md");
        assert_eq!(s.needs_heading, "Needs you");
        assert_eq!(s.every, Duration::from_secs(7 * 86_400));
        assert_eq!(s.grace, Duration::from_secs(6 * 3600));
        assert_eq!(c.agent("Desk").unwrap().needs_heading, "Only you");
        let o = c.agent("Orchestrator").unwrap();
        assert_eq!(o.kind, Kind::Local);
        assert_eq!(o.every, Duration::from_secs(20 * 60));
    }

    #[test]
    fn round_trips_through_text() {
        let c = Config::parse(EXAMPLE).unwrap();
        assert_eq!(Config::parse(&c.to_text()).unwrap(), c);
    }

    #[test]
    fn refuses_an_agent_missing_what_its_kind_needs() {
        let e = Config::parse(
            "[[agent]]\nname = \"X\"\npurpose = \"p\"\nkey = \"x\"\nevery = \"1d\"\n",
        )
        .unwrap_err();
        assert_eq!(e.0, "X has no repo");
        let e = Config::parse("[[agent]]\nname = \"Y\"\npurpose = \"p\"\nkind = \"local\"\nhost = \"mac1\"\nevery = \"1d\"\n")
            .unwrap_err();
        assert_eq!(e.0, "Y has no status");
    }

    #[test]
    fn refuses_two_agents_with_one_name() {
        let one = "[[agent]]\nname = \"Desk\"\npurpose = \"p\"\nrepo = \"git@github.com:a/b.git\"\nkey = \"k\"\nevery = \"1d\"\n";
        let e = Config::parse(&format!("{one}{}", one.replace("Desk", "desk"))).unwrap_err();
        assert_eq!(e.0, "two agents are called desk");
    }

    #[test]
    fn refuses_values_that_would_reach_a_command_line_oddly() {
        let base = "[[agent]]\nname = \"O\"\npurpose = \"p\"\nkind = \"local\"\nhost = \"mac1\"\nevery = \"20m\"\n";
        assert!(Config::parse(&format!("{base}status = \"~/s.md\"\n")).is_ok());
        let e = Config::parse(&format!("{base}status = \"~/s.md; rm -rf ~\"\n")).unwrap_err();
        assert_eq!(e.0, "O: status has characters it cannot have");
        let e = Config::parse(&format!("{base}status = \"~/s.md\"\nkickstart = \"a b\"\n"))
            .unwrap_err();
        assert_eq!(e.0, "O: kickstart has characters it cannot have");
        let cloud = "[[agent]]\nname = \"C\"\npurpose = \"p\"\nkey = \"c\"\nevery = \"1d\"\n";
        let e = Config::parse(&format!("{cloud}repo = \"https://example.com/x\"\n")).unwrap_err();
        assert_eq!(e.0, "C: repo must be git@github.com:<owner>/<repo>.git");
        let e = Config::parse(&format!(
            "{cloud}repo = \"git@github.com:a/b.git\"\nhub = \"../x.md\"\n"
        ))
        .unwrap_err();
        assert_eq!(e.0, "C: hub has characters it cannot have");
    }

    #[test]
    fn web_url_and_commit_prefix() {
        let c = Config::parse(EXAMPLE).unwrap();
        let d = c.agent("Desk").unwrap();
        assert_eq!(
            d.web_url().as_deref(),
            Some("https://github.com/jon-comley/desk")
        );
        assert_eq!(d.commit_prefix(), "Desk:");
        assert_eq!(c.agent("Orchestrator").unwrap().web_url(), None);
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("30m"), Some(Duration::from_secs(1800)));
        assert_eq!(parse_duration(" 6h "), Some(Duration::from_secs(21_600)));
        assert_eq!(parse_duration("7d"), Some(Duration::from_secs(604_800)));
        assert_eq!(parse_duration("7"), None);
        assert_eq!(parse_duration("d"), None);
        assert_eq!(parse_duration("2w"), None);
        assert_eq!(format_duration(Duration::from_secs(604_800)), "7d");
        assert_eq!(format_duration(Duration::from_secs(21_600)), "6h");
        assert_eq!(format_duration(Duration::from_secs(1800)), "30m");
        assert!(
            Config::parse(
                "[[agent]]\nname=\"a\"\npurpose=\"p\"\nrepo=\"git@github.com:a/b.git\"\nkey=\"k\"\nevery=\"soon\"\n"
            )
            .is_err()
        );
    }
}
