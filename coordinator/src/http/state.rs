use serde::Serialize;
use shared::hardware::NodeRole;
use shared::messages::NodeRecordLite;
use std::sync::Arc;
use tokio::sync::broadcast;

/// Events broadcast to all connected dashboard WebSocket clients.
#[derive(Clone, Serialize)]
#[serde(tag = "type")]
pub enum DashboardEvent {
    TopologyUpdate { nodes: Vec<NodeDashInfo> },
}

#[derive(Clone, Serialize)]
pub struct NodeDashInfo {
    pub id: String,
    pub name: String,
    pub role: String,
    pub ip: String,
    /// Age of last heartbeat in whole seconds.
    pub last_seen_secs: u64,
    /// "green" (<10 s), "amber" (10–30 s), "red" (>30 s).
    pub health: &'static str,
}

pub struct DashboardState {
    pub tx: broadcast::Sender<DashboardEvent>,
    /// Valid auth tokens — mirrors the mesh server's token list. Empty = no auth (dev mode).
    pub auth_tokens: Arc<Vec<String>>,
}

impl DashboardState {
    pub fn new(auth_tokens: Arc<Vec<String>>) -> Arc<Self> {
        let (tx, _) = broadcast::channel(128);
        Arc::new(Self { tx, auth_tokens })
    }

    /// Returns true when the supplied token is acceptable.
    pub fn auth_ok(&self, token: &str) -> bool {
        self.auth_tokens.is_empty() || self.auth_tokens.iter().any(|t| t == token)
    }

    /// Build and broadcast a TopologyUpdate from a fresh node list.
    pub fn push_topology(&self, nodes: &[NodeRecordLite]) {
        // No receivers — nothing to do.
        if self.tx.receiver_count() == 0 {
            return;
        }
        let evt = DashboardEvent::TopologyUpdate {
            nodes: nodes.iter().map(node_dash_info).collect(),
        };
        let _ = self.tx.send(evt);
    }
}

fn node_dash_info(n: &NodeRecordLite) -> NodeDashInfo {
    let age_secs = (n.last_heartbeat_ms / 1000) as u64;
    let health = if age_secs < 10 {
        "green"
    } else if age_secs < 30 {
        "amber"
    } else {
        "red"
    };
    NodeDashInfo {
        id: n.id.clone(),
        name: n.hostname.clone(),
        role: role_label(&n.role),
        ip: n.ip.clone(),
        last_seen_secs: age_secs,
        health,
    }
}

fn role_label(role: &NodeRole) -> String {
    match role {
        NodeRole::Compute => "Compute".into(),
        NodeRole::Controller => "Controller".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::messages::NodeRecordLite;

    fn lite(last_heartbeat_ms: u128) -> NodeRecordLite {
        NodeRecordLite {
            id: "test-id".into(),
            hostname: "testhost".into(),
            ip: "10.0.0.1".into(),
            role: NodeRole::Compute,
            last_heartbeat_ms,
        }
    }

    // ── auth_ok ───────────────────────────────────────────────────────────────

    #[test]
    fn auth_ok_dev_mode_accepts_any_token() {
        let state = DashboardState::new(Arc::new(vec![]));
        assert!(state.auth_ok(""));
        assert!(state.auth_ok("any-token"));
        assert!(state.auth_ok("completely-random"));
    }

    #[test]
    fn auth_ok_accepts_matching_token() {
        let state = DashboardState::new(Arc::new(vec!["secret".into()]));
        assert!(state.auth_ok("secret"));
    }

    #[test]
    fn auth_ok_rejects_wrong_and_empty_token() {
        let state = DashboardState::new(Arc::new(vec!["secret".into()]));
        assert!(!state.auth_ok("wrong"));
        assert!(!state.auth_ok(""));
    }

    #[test]
    fn auth_ok_accepts_any_configured_token() {
        let state = DashboardState::new(Arc::new(vec!["alpha".into(), "beta".into()]));
        assert!(state.auth_ok("alpha"));
        assert!(state.auth_ok("beta"));
        assert!(!state.auth_ok("gamma"));
    }

    // ── health colour thresholds ──────────────────────────────────────────────

    #[test]
    fn health_green_under_10s() {
        let info = node_dash_info(&lite(5_000)); // 5 s
        assert_eq!(info.health, "green");
        assert_eq!(info.last_seen_secs, 5);
    }

    #[test]
    fn health_amber_10_to_29s() {
        let info = node_dash_info(&lite(10_000)); // 10 s — boundary
        assert_eq!(info.health, "amber");
        let info2 = node_dash_info(&lite(29_000)); // 29 s
        assert_eq!(info2.health, "amber");
    }

    #[test]
    fn health_red_at_30s_and_above() {
        let info = node_dash_info(&lite(30_000)); // exactly 30 s
        assert_eq!(info.health, "red");
        let info2 = node_dash_info(&lite(120_000)); // 2 min
        assert_eq!(info2.health, "red");
    }

    // ── push_topology ─────────────────────────────────────────────────────────

    #[test]
    fn push_topology_with_no_receivers_is_noop() {
        let state = DashboardState::new(Arc::new(vec![]));
        // No panic, no side-effects — just verifies the early-return path.
        state.push_topology(&[]);
        state.push_topology(&[lite(1_000)]);
    }
}
