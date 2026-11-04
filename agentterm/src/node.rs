//! The machine pool (plans/agent-terminals.md, "Where a session runs"). Each
//! `[[node]]` in `agents.toml` says what it can do; a new terminal session goes
//! to the least busy reachable node that can do its worker's job. A worker's
//! `pin` overrides, and a worker with its own `host` (the orchestrator) always
//! runs there.
//!
//! ```toml
//! [[node]]
//! name = "lenovo1"                 # no host: this machine
//! can = ["claude", "desk", "standards"]
//!
//! [[node]]
//! name = "beelink1"
//! host = "beelink1"
//! os = "windows"
//! can = []                         # nothing until Jon adds Claude there
//! ```

use std::collections::HashMap;

use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Os {
    #[default]
    Unix,
    Windows,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Node {
    pub name: String,
    /// ssh name; none for the machine agentterm runs on.
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub os: Os,
    /// What it can do: `claude` (logged in), a worker's slug (it has that
    /// worker's clone), `phones`, `work`.
    #[serde(default)]
    pub can: Vec<String>,
    /// Where the workers' clones are, `chats/<slug>` under it.
    #[serde(default = "default_chats")]
    pub chats: String,
}

fn default_chats() -> String {
    "~/.ai-mesh/agents/chats".into()
}

impl Node {
    pub fn local(&self) -> bool {
        self.host.is_none()
    }

    pub fn can_do(&self, needs: &[String]) -> bool {
        needs.iter().all(|n| self.can.iter().any(|c| c == n))
    }
}

/// With no `[[node]]` list, this machine alone, as before the pool.
pub fn or_this_machine(nodes: Vec<Node>, slugs: &[String]) -> Vec<Node> {
    if !nodes.is_empty() {
        return nodes;
    }
    let mut can = vec!["claude".to_string()];
    can.extend(slugs.iter().cloned());
    vec![Node {
        name: "this machine".into(),
        host: None,
        os: Os::Unix,
        can,
        chats: default_chats(),
    }]
}

/// What a worker needs from a node: Claude and its own clone, unless it says.
pub fn needs(slug: &str, declared: &[String]) -> Vec<String> {
    if declared.is_empty() {
        vec!["claude".into(), slug.into()]
    } else {
        declared.to_vec()
    }
}

/// The node a new session goes to. `reachable` is asked only of nodes that
/// can do the job; `load` is each node's open terminal sessions. Ties go to
/// the node listed first.
pub fn place<'a>(
    needs: &[String],
    pin: Option<&str>,
    nodes: &'a [Node],
    mut reachable: impl FnMut(&Node) -> bool,
    load: &HashMap<String, usize>,
) -> Result<&'a Node, String> {
    let able: Vec<&Node> = nodes
        .iter()
        .filter(|n| pin.is_none_or(|p| n.name == p))
        .filter(|n| n.can_do(needs))
        .collect();
    if able.is_empty() {
        return Err(match pin {
            Some(p) => format!("{p} cannot do this job (needs {})", needs.join(", ")),
            None => format!("no machine can do this job (needs {})", needs.join(", ")),
        });
    }
    let mut up: Vec<&Node> = able.into_iter().filter(|n| reachable(n)).collect();
    if up.is_empty() {
        return Err(match pin {
            Some(p) => format!("{p} is off or out of reach"),
            None => "every machine that can do this job is off or out of reach".into(),
        });
    }
    // Stable: equal loads keep the listed order.
    up.sort_by_key(|n| load.get(&n.name).copied().unwrap_or(0));
    Ok(up[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nodes() -> Vec<Node> {
        let n = |name: &str, host: Option<&str>, can: &[&str]| Node {
            name: name.into(),
            host: host.map(Into::into),
            os: Os::Unix,
            can: can.iter().map(|s| s.to_string()).collect(),
            chats: default_chats(),
        };
        vec![
            n("lenovo1", None, &["claude", "desk", "standards"]),
            n("mac1", Some("mac1"), &["claude", "desk", "phones"]),
            n("beelink1", Some("beelink1"), &[]),
        ]
    }

    fn needs_of(slug: &str) -> Vec<String> {
        needs(slug, &[])
    }

    #[test]
    fn goes_to_the_least_busy_able_node() {
        let ns = nodes();
        let mut load = HashMap::new();
        assert_eq!(
            place(&needs_of("desk"), None, &ns, |_| true, &load)
                .unwrap()
                .name,
            "lenovo1"
        );
        load.insert("lenovo1".to_string(), 2);
        assert_eq!(
            place(&needs_of("desk"), None, &ns, |_| true, &load)
                .unwrap()
                .name,
            "mac1"
        );
    }

    #[test]
    fn skips_nodes_that_cannot_or_are_off() {
        let ns = nodes();
        let load = HashMap::new();
        // Only lenovo1 has the standards clone.
        assert_eq!(
            place(&needs_of("standards"), None, &ns, |_| true, &load)
                .unwrap()
                .name,
            "lenovo1"
        );
        // lenovo1 off: mac1 takes Desk.
        let off = |n: &Node| n.name != "lenovo1";
        assert_eq!(
            place(&needs_of("desk"), None, &ns, off, &load)
                .unwrap()
                .name,
            "mac1"
        );
        // beelink1 has no Claude yet: never chosen.
        let e = place(&needs_of("desk"), Some("beelink1"), &ns, |_| true, &load).unwrap_err();
        assert!(e.contains("beelink1 cannot do this job"));
    }

    #[test]
    fn a_pin_waits_for_its_node() {
        let ns = nodes();
        let e = place(
            &needs_of("desk"),
            Some("mac1"),
            &ns,
            |_| false,
            &HashMap::new(),
        )
        .unwrap_err();
        assert_eq!(e, "mac1 is off or out of reach");
    }

    #[test]
    fn declared_needs_replace_the_default() {
        assert_eq!(needs("desk", &[]), ["claude", "desk"]);
        assert_eq!(
            needs("wl", &["claude".into(), "work".into()]),
            ["claude", "work"]
        );
    }

    #[test]
    fn no_node_list_means_this_machine() {
        let ns = or_this_machine(Vec::new(), &["desk".into()]);
        assert!(ns[0].local() && ns[0].can_do(&needs_of("desk")));
    }
}
