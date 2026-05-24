use serde::Serialize;
use shared::MeshMessage;
use shared::hardware::NodeRole;
use shared::messages::NodeRecordLite;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{broadcast, mpsc};
use tracing::warn;

/// Live TCP sender channels keyed by node ID — shared between the TCP server and the HTTP API.
pub type NodeConnections = Arc<Mutex<HashMap<String, mpsc::Sender<MeshMessage>>>>;

const HEALTH_WINDOW: usize = 60;

/// Events broadcast to all connected dashboard WebSocket clients.
#[derive(Clone, Serialize)]
#[serde(tag = "type")]
pub enum DashboardEvent {
    TopologyUpdate {
        nodes: Vec<NodeDashInfo>,
    },
    HealthUpdate {
        node_id: String,
        samples: Vec<HealthSample>,
    },
}

/// One health data point, coordinator-stamped.
#[derive(Clone, Serialize)]
pub struct HealthSample {
    /// Unix timestamp in milliseconds, set by the coordinator on receipt.
    pub ts_ms: u64,
    pub cpu_pct: f32,
    pub ram_used_gb: f32,
    pub ram_total_gb: f32,
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
    health_store: Mutex<HashMap<String, VecDeque<HealthSample>>>,
    /// Live TCP sender channels — used by the HTTP API to push commands to agents.
    pub connections: NodeConnections,
}

impl DashboardState {
    pub fn new(auth_tokens: Arc<Vec<String>>, connections: NodeConnections) -> Arc<Self> {
        let (tx, _) = broadcast::channel(128);
        Arc::new(Self {
            tx,
            auth_tokens,
            health_store: Mutex::new(HashMap::new()),
            connections,
        })
    }

    /// Send `msg` to the named node's open TCP channel.
    /// Returns `true` if the message was queued, `false` if the node is not connected.
    pub fn send_to_node(&self, node_id: &str, msg: MeshMessage) -> bool {
        let guard = self.connections.lock().unwrap();
        match guard.get(node_id) {
            Some(tx) => match tx.try_send(msg) {
                Ok(()) => true,
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                    warn!(node_id, "send_to_node: channel full, message dropped");
                    false
                }
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => false,
            },
            None => false,
        }
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

    /// Record a health sample for `node_id`, capped at HEALTH_WINDOW entries,
    /// then broadcast a HealthUpdate with the full window to connected clients.
    pub fn push_health(&self, node_id: &str, cpu_pct: f32, ram_used_gb: f32, ram_total_gb: f32) {
        let ts_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let sample = HealthSample {
            ts_ms,
            cpu_pct,
            ram_used_gb,
            ram_total_gb,
        };
        let samples = {
            let mut store = self.health_store.lock().unwrap();
            let window = store.entry(node_id.to_owned()).or_default();
            window.push_back(sample);
            if window.len() > HEALTH_WINDOW {
                window.pop_front();
            }
            window.iter().cloned().collect::<Vec<_>>()
        };
        if self.tx.receiver_count() == 0 {
            return;
        }
        let _ = self.tx.send(DashboardEvent::HealthUpdate {
            node_id: node_id.to_owned(),
            samples,
        });
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
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        assert!(state.auth_ok(""));
        assert!(state.auth_ok("any-token"));
        assert!(state.auth_ok("completely-random"));
    }

    #[test]
    fn auth_ok_accepts_matching_token() {
        let state = DashboardState::new(
            Arc::new(vec!["secret".into()]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        assert!(state.auth_ok("secret"));
    }

    #[test]
    fn auth_ok_rejects_wrong_and_empty_token() {
        let state = DashboardState::new(
            Arc::new(vec!["secret".into()]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        assert!(!state.auth_ok("wrong"));
        assert!(!state.auth_ok(""));
    }

    #[test]
    fn auth_ok_accepts_any_configured_token() {
        let state = DashboardState::new(
            Arc::new(vec!["alpha".into(), "beta".into()]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
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
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        // No panic, no side-effects — just verifies the early-return path.
        state.push_topology(&[]);
        state.push_topology(&[lite(1_000)]);
    }

    // ── push_health ───────────────────────────────────────────────────────────

    #[test]
    fn push_health_broadcasts_health_update() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        let mut rx = state.tx.subscribe();
        state.push_health("n1", 42.5, 6.1, 15.9);
        let evt = rx.try_recv().unwrap();
        match evt {
            DashboardEvent::HealthUpdate { node_id, samples } => {
                assert_eq!(node_id, "n1");
                assert_eq!(samples.len(), 1);
                assert_eq!(samples[0].cpu_pct, 42.5);
                assert_eq!(samples[0].ram_used_gb, 6.1);
                assert_eq!(samples[0].ram_total_gb, 15.9);
                assert!(samples[0].ts_ms > 0);
            }
            _ => panic!("expected HealthUpdate"),
        }
    }

    #[test]
    fn push_health_accumulates_samples() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        let mut rx = state.tx.subscribe();
        state.push_health("n1", 10.0, 1.0, 16.0);
        state.push_health("n1", 20.0, 2.0, 16.0);
        state.push_health("n1", 30.0, 3.0, 16.0);
        // Drain; last event has all 3 samples.
        let mut last = None;
        while let Ok(e) = rx.try_recv() {
            last = Some(e);
        }
        match last.unwrap() {
            DashboardEvent::HealthUpdate { samples, .. } => assert_eq!(samples.len(), 3),
            _ => panic!("expected HealthUpdate"),
        }
    }

    #[test]
    fn push_health_caps_window_at_60() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        let mut rx = state.tx.subscribe();
        for i in 0..=60u32 {
            state.push_health("n1", i as f32, 0.0, 16.0);
        }
        let mut last = None;
        while let Ok(e) = rx.try_recv() {
            last = Some(e);
        }
        match last.unwrap() {
            DashboardEvent::HealthUpdate { samples, .. } => {
                assert_eq!(samples.len(), 60);
                // cpu_pct 0.0 was the first (evicted); 1.0 is now oldest
                assert_eq!(samples[0].cpu_pct, 1.0);
                assert_eq!(samples[59].cpu_pct, 60.0);
            }
            _ => panic!("expected HealthUpdate"),
        }
    }

    #[test]
    fn push_health_independent_per_node() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        let mut rx = state.tx.subscribe();
        state.push_health("n1", 10.0, 1.0, 8.0);
        state.push_health("n2", 50.0, 4.0, 8.0);
        state.push_health("n1", 15.0, 1.5, 8.0);
        let mut events: Vec<DashboardEvent> = vec![];
        while let Ok(e) = rx.try_recv() {
            events.push(e);
        }
        // n1 was pushed twice; its last event has 2 samples
        let n1_last = events
            .iter()
            .filter_map(|e| match e {
                DashboardEvent::HealthUpdate { node_id, samples } if node_id == "n1" => {
                    Some(samples.len())
                }
                _ => None,
            })
            .next_back()
            .unwrap();
        assert_eq!(n1_last, 2);
    }

    #[test]
    fn push_health_with_no_receivers_is_noop() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        // No panic — store still updates, broadcast skipped
        state.push_health("n1", 0.0, 0.0, 0.0);
    }

    #[test]
    fn health_sample_serializes_expected_fields() {
        let s = HealthSample {
            ts_ms: 1_000_000,
            cpu_pct: 33.3,
            ram_used_gb: 4.0,
            ram_total_gb: 16.0,
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"ts_ms\""));
        assert!(json.contains("\"cpu_pct\""));
        assert!(json.contains("\"ram_used_gb\""));
        assert!(json.contains("\"ram_total_gb\""));
    }

    #[test]
    fn health_update_event_wire_format() {
        // Pins the exact JSON shape the dashboard JS expects.
        let evt = DashboardEvent::HealthUpdate {
            node_id: "n1".into(),
            samples: vec![HealthSample {
                ts_ms: 1_000,
                cpu_pct: 10.0,
                ram_used_gb: 2.0,
                ram_total_gb: 8.0,
            }],
        };
        let json = serde_json::to_string(&evt).unwrap();
        assert!(
            json.contains("\"type\":\"HealthUpdate\""),
            "missing type tag: {json}"
        );
        assert!(
            json.contains("\"node_id\":\"n1\""),
            "missing node_id: {json}"
        );
        assert!(json.contains("\"samples\""), "missing samples: {json}");
        assert!(
            json.contains("\"ts_ms\""),
            "missing ts_ms in sample: {json}"
        );
    }

    #[test]
    fn push_health_ts_ms_monotonically_nondecreasing() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        let mut rx = state.tx.subscribe();
        state.push_health("n1", 10.0, 1.0, 8.0);
        state.push_health("n1", 20.0, 2.0, 8.0);
        let mut last = None;
        while let Ok(e) = rx.try_recv() {
            last = Some(e);
        }
        match last.unwrap() {
            DashboardEvent::HealthUpdate { samples, .. } => {
                assert_eq!(samples.len(), 2);
                assert!(samples[1].ts_ms >= samples[0].ts_ms);
            }
            _ => panic!("expected HealthUpdate"),
        }
    }
}
