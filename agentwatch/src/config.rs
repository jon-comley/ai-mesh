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

fn default_grace() -> Duration {
    Duration::from_secs(6 * 3600)
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default, rename = "agent")]
    pub agents: Vec<Agent>,
    /// The ntfy topic URL for state-change pushes.
    #[serde(default)]
    pub ntfy: Option<String>,
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
                Kind::Local => [("host", a.host.is_none()), ("status", a.status.is_none())],
            };
            if let Some((field, _)) = missing.iter().find(|(_, m)| *m) {
                return Err(ConfigError(format!("{} has no {field}", a.name)));
            }
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
        let one = "[[agent]]\nname = \"Desk\"\npurpose = \"p\"\nrepo = \"r\"\nkey = \"k\"\nevery = \"1d\"\n";
        let e = Config::parse(&format!("{one}{}", one.replace("Desk", "desk"))).unwrap_err();
        assert_eq!(e.0, "two agents are called desk");
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
                "[[agent]]\nname=\"a\"\npurpose=\"p\"\nrepo=\"r\"\nkey=\"k\"\nevery=\"soon\"\n"
            )
            .is_err()
        );
    }
}
