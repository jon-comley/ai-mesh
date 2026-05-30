use crate::registry::RoomRecord;
use serde::Serialize;
use shared::MeshMessage;
use shared::hardware::NodeRole;
use shared::messages::{LightAction, LightStateReport, NodeRecordLite};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{Notify, broadcast, mpsc};
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
    ModelUpdate {
        nodes: Vec<NodeModelInfo>,
    },
    LightingUpdate {
        devices: Vec<LightStateReport>,
        #[serde(default)]
        groups: Vec<String>,
    },
    RoomsUpdate {
        rooms: Vec<RoomInfo>,
        #[serde(default)]
        device_names: HashMap<String, String>,
    },
    ScenesUpdate {
        scenes: Vec<SceneInfo>,
    },
    SolarUpdate {
        azimuth: f64,
        elevation: f64,
    },
    EffectUpdate {
        room_id: String,
        effect_id: Option<String>,
        params: serde_json::Value,
        overrides: Vec<String>,
    },
}

/// Snapshot of the currently-active effect (if any) for one room. Pushed to a
/// new WS client on connect alongside the other panel snapshots.
#[derive(Clone, Debug, Serialize)]
pub struct RoomEffectInfo {
    pub room_id: String,
    pub effect_id: Option<String>,
    pub params: serde_json::Value,
    pub overrides: Vec<String>,
}

#[derive(Clone, Serialize)]
pub struct RoomInfo {
    pub id: String,
    pub name: String,
    pub position: i64,
    pub device_ids: Vec<String>,
    pub orientation_degrees: f32,
    pub has_window: bool,
    pub window_facing: Option<f32>,
    pub width_m: f64,
    pub depth_m: f64,
    pub height_m: f64,
    pub origin_x: f64,
    pub origin_y: f64,
}

impl From<RoomRecord> for RoomInfo {
    fn from(r: RoomRecord) -> Self {
        Self {
            id: r.id,
            name: r.name,
            position: r.position,
            device_ids: r.device_ids,
            orientation_degrees: r.orientation_degrees,
            has_window: r.has_window,
            window_facing: r.window_facing,
            width_m: r.width_m,
            depth_m: r.depth_m,
            height_m: r.height_m,
            origin_x: r.origin_x,
            origin_y: r.origin_y,
        }
    }
}

#[derive(Clone, Serialize)]
pub struct SceneInfo {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub room_id: Option<String>,
    pub created_at: i64,
    pub position: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview_color: Option<[f32; 2]>,
}

/// Per-model entry in a `ModelUpdate` event — one per non-Unloaded allocation.
#[derive(Clone, Serialize)]
pub struct ModelEntry {
    pub name: String,
    pub size_mb: u64,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Per-node model summary — one per Compute node in a `ModelUpdate` event.
#[derive(Clone, Serialize)]
pub struct NodeModelInfo {
    pub node_id: String,
    pub hostname: String,
    pub role: String,
    /// Static RAM ceiling from HardwareSpec; 0.0 if hardware not yet reported.
    pub ram_gb: f32,
    pub models: Vec<ModelEntry>,
}

/// One health data point, coordinator-stamped.
#[derive(Clone, Serialize)]
pub struct HealthSample {
    /// Unix timestamp in milliseconds, set by the coordinator on receipt.
    pub ts_ms: u64,
    pub cpu_pct: f32,
    pub ram_used_gb: f32,
    pub ram_total_gb: f32,
    /// GPU utilisation 0.0–100.0; None on CPU-only nodes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gpu_pct: Option<f32>,
    /// GPU VRAM in use, gibibytes; None on CPU-only nodes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gpu_vram_used_gb: Option<f32>,
    /// Total GPU VRAM, gibibytes; None on CPU-only nodes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gpu_vram_total_gb: Option<f32>,
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
    model_snapshot: Mutex<Vec<NodeModelInfo>>,
    light_snapshot: Mutex<HashMap<String, LightStateReport>>,
    /// Group friendly name → node_id that owns it.
    group_snapshot: Mutex<HashMap<String, String>>,
    room_snapshot: Mutex<Vec<RoomInfo>>,
    scene_snapshot: Mutex<Vec<SceneInfo>>,
    /// Per-room currently-active effect (if any). Mirrors `room_effects` rows
    /// where `enabled = 1`. Pushed to new WS clients on connect.
    effect_snapshot: Mutex<HashMap<String, RoomEffectInfo>>,
    /// Live TCP sender channels — used by the HTTP API to push commands to agents.
    pub connections: NodeConnections,
    /// Wakes the EffectRunner for an immediate tick — used by activation /
    /// deactivation paths and any UX that needs effect output to reflect a
    /// state change without waiting for the next scheduled tick.
    pub solar_sweep_notify: Arc<Notify>,
    /// Location used by the JS solar calculator (served via GET /api/solar/config).
    pub lat: f64,
    pub lon: f64,
}

impl DashboardState {
    pub fn new(auth_tokens: Arc<Vec<String>>, connections: NodeConnections) -> Arc<Self> {
        let lat = std::env::var("MESH_LATITUDE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(51.5074);
        let lon = std::env::var("MESH_LONGITUDE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(-0.1278);
        let (tx, _) = broadcast::channel(128);
        Arc::new(Self {
            tx,
            auth_tokens,
            health_store: Mutex::new(HashMap::new()),
            model_snapshot: Mutex::new(Vec::new()),
            light_snapshot: Mutex::new(HashMap::new()),
            group_snapshot: Mutex::new(HashMap::new()),
            room_snapshot: Mutex::new(Vec::new()),
            scene_snapshot: Mutex::new(Vec::new()),
            effect_snapshot: Mutex::new(HashMap::new()),
            connections,
            solar_sweep_notify: Arc::new(Notify::new()),
            lat,
            lon,
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

    /// Return a snapshot of all stored health windows — used to warm-start new WS clients.
    pub fn get_all_health_snapshots(&self) -> Vec<(String, Vec<HealthSample>)> {
        let store = self.health_store.lock().unwrap();
        store
            .iter()
            .map(|(id, deque)| (id.clone(), deque.iter().cloned().collect()))
            .collect()
    }

    /// Store and broadcast a model snapshot. Always stores (for snapshot-on-connect);
    /// broadcasts only when at least one WS client is connected.
    pub fn push_model_update(&self, nodes: Vec<NodeModelInfo>) {
        *self.model_snapshot.lock().unwrap() = nodes.clone();
        if self.tx.receiver_count() > 0 {
            let _ = self.tx.send(DashboardEvent::ModelUpdate { nodes });
        }
    }

    /// Return a point-in-time copy of the model snapshot for warm-starting new WS clients.
    pub fn get_model_snapshot(&self) -> Vec<NodeModelInfo> {
        self.model_snapshot.lock().unwrap().clone()
    }

    /// Store the latest state for one device and broadcast a LightingUpdate with all devices + groups.
    pub fn push_lighting_update(&self, report: LightStateReport) {
        let devices = {
            let mut snap = self.light_snapshot.lock().unwrap();
            snap.insert(report.device_id.clone(), report);
            snap.values().cloned().collect::<Vec<_>>()
        };
        if self.tx.receiver_count() > 0 {
            let groups = self.get_group_snapshot();
            let _ = self
                .tx
                .send(DashboardEvent::LightingUpdate { devices, groups });
        }
    }

    /// Add placeholder entries for discovered devices that haven't yet reported state.
    /// Broadcasts a LightingUpdate if any new placeholders were added and `emit` is true.
    pub fn push_device_discovery(&self, node_id: &str, devices: Vec<String>, emit: bool) {
        let mut updated = false;
        {
            let mut snap = self.light_snapshot.lock().unwrap();
            for id in devices {
                if !snap.contains_key(&id) {
                    snap.insert(
                        id.clone(),
                        LightStateReport {
                            node_id: node_id.to_owned(),
                            device_id: id,
                            on: false, // Default to off as a fallback visibility state
                            // Placeholder values for UI sliders - not used for commands.
                            // 254 = 100% brightness, 370 = 2700K (neutral warm white).
                            brightness: Some(254),
                            color_xy: None,
                            color_temp: Some(370),
                        },
                    );
                    updated = true;
                }
            }
        }
        if emit && updated && self.tx.receiver_count() > 0 {
            let devices = self.get_light_snapshot();
            let groups = self.get_group_snapshot();
            let _ = self
                .tx
                .send(DashboardEvent::LightingUpdate { devices, groups });
        }
    }

    /// Apply an outbound command's intended effect to the in-memory light snapshot.
    ///
    /// Subsequent broadcasts (triggered by any device's later LightState report)
    /// will then carry the user's intended value rather than the pre-command
    /// value, preventing UI sliders from snapping back when another device's
    /// status report races ahead of the bulb's own confirmation.
    pub fn apply_command_to_snapshot(&self, device_id: &str, action: &LightAction) {
        let mut snap = self.light_snapshot.lock().unwrap();
        let Some(entry) = snap.get_mut(device_id) else {
            return;
        };
        match action {
            LightAction::On => entry.on = true,
            LightAction::Off => entry.on = false,
            LightAction::Toggle => entry.on = !entry.on,
            LightAction::Brightness(b) => {
                entry.brightness = Some(*b);
                entry.on = true;
            }
            LightAction::BrightnessTransition { value, .. } => {
                entry.brightness = Some(*value);
                entry.on = true;
            }
            LightAction::ColorTemp(ct) => entry.color_temp = Some(*ct),
            LightAction::ColorTempTransition { value, .. } => entry.color_temp = Some(*value),
            LightAction::ColorXY { x, y } => entry.color_xy = Some((*x, *y)),
            LightAction::ColorXYTransition { x, y, .. } => entry.color_xy = Some((*x, *y)),
        }
    }

    /// Store groups for a node and broadcast a LightingUpdate with all devices + new groups.
    pub fn push_group_update(&self, node_id: &str, groups: Vec<String>) {
        {
            let mut snap = self.group_snapshot.lock().unwrap();
            snap.retain(|_, v| v != node_id);
            for g in groups {
                snap.insert(g, node_id.to_owned());
            }
        }
        if self.tx.receiver_count() > 0 {
            let devices = self.get_light_snapshot();
            let groups = self.get_group_snapshot();
            let _ = self
                .tx
                .send(DashboardEvent::LightingUpdate { devices, groups });
        }
    }

    /// Return all known light device states — used to warm-start new WS clients.
    pub fn get_light_snapshot(&self) -> Vec<LightStateReport> {
        self.light_snapshot
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect()
    }

    /// Return all known group friendly names — used to warm-start new WS clients.
    pub fn get_group_snapshot(&self) -> Vec<String> {
        self.group_snapshot
            .lock()
            .unwrap()
            .keys()
            .cloned()
            .collect()
    }

    /// Return the node_id that last reported state for a given device — used to route commands.
    pub fn get_node_for_device(&self, device_id: &str) -> Option<String> {
        self.light_snapshot
            .lock()
            .unwrap()
            .get(device_id)
            .map(|r| r.node_id.clone())
    }

    /// Return the node_id responsible for a given group — used to route group commands.
    pub fn get_node_for_group(&self, name: &str) -> Option<String> {
        self.group_snapshot.lock().unwrap().get(name).cloned()
    }

    /// Store and broadcast the current rooms state.
    pub fn push_rooms_update(&self, rooms: Vec<RoomInfo>) {
        self.push_rooms_update_with_names(rooms, HashMap::new());
    }

    pub fn push_rooms_update_with_names(
        &self,
        rooms: Vec<RoomInfo>,
        device_names: HashMap<String, String>,
    ) {
        *self.room_snapshot.lock().unwrap() = rooms.clone();
        if self.tx.receiver_count() > 0 {
            let _ = self.tx.send(DashboardEvent::RoomsUpdate {
                rooms,
                device_names,
            });
        }
    }

    /// Return a point-in-time copy of the rooms snapshot for warm-starting new WS clients.
    pub fn get_room_snapshot(&self) -> Vec<RoomInfo> {
        self.room_snapshot.lock().unwrap().clone()
    }

    /// Store and broadcast the current scenes state.
    pub fn push_scenes_update(&self, scenes: Vec<SceneInfo>) {
        *self.scene_snapshot.lock().unwrap() = scenes.clone();
        if self.tx.receiver_count() > 0 {
            let _ = self.tx.send(DashboardEvent::ScenesUpdate { scenes });
        }
    }

    /// Return a point-in-time copy of the scenes snapshot for warm-starting new WS clients.
    pub fn get_scene_snapshot(&self) -> Vec<SceneInfo> {
        self.scene_snapshot.lock().unwrap().clone()
    }

    /// Update the per-room effect snapshot and broadcast `EffectUpdate`.
    /// Pass `effect_id = None` to clear the room's active effect.
    pub fn push_effect_update(
        &self,
        room_id: String,
        effect_id: Option<String>,
        params: serde_json::Value,
        overrides: Vec<String>,
    ) {
        {
            let mut snap = self.effect_snapshot.lock().unwrap();
            if effect_id.is_none() {
                snap.remove(&room_id);
            } else {
                snap.insert(
                    room_id.clone(),
                    RoomEffectInfo {
                        room_id: room_id.clone(),
                        effect_id: effect_id.clone(),
                        params: params.clone(),
                        overrides: overrides.clone(),
                    },
                );
            }
        }
        if self.tx.receiver_count() > 0 {
            let _ = self.tx.send(DashboardEvent::EffectUpdate {
                room_id,
                effect_id,
                params,
                overrides,
            });
        }
    }

    /// Snapshot of every room with an active effect — used by `ws.rs` to
    /// hydrate a new dashboard client.
    pub fn get_effect_snapshot(&self) -> Vec<RoomEffectInfo> {
        self.effect_snapshot
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect()
    }

    /// Broadcast the current solar position to all connected clients.
    pub fn push_solar_update(&self, azimuth: f64, elevation: f64) {
        if self.tx.receiver_count() > 0 {
            let _ = self
                .tx
                .send(DashboardEvent::SolarUpdate { azimuth, elevation });
        }
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
    #[allow(clippy::too_many_arguments)]
    pub fn push_health(
        &self,
        node_id: &str,
        cpu_pct: f32,
        ram_used_gb: f32,
        ram_total_gb: f32,
        gpu_pct: Option<f32>,
        gpu_vram_used_gb: Option<f32>,
        gpu_vram_total_gb: Option<f32>,
    ) {
        let ts_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let sample = HealthSample {
            ts_ms,
            cpu_pct,
            ram_used_gb,
            ram_total_gb,
            gpu_pct,
            gpu_vram_used_gb,
            gpu_vram_total_gb,
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
        state.push_health("n1", 42.5, 6.1, 15.9, None, None, None);
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
        state.push_health("n1", 10.0, 1.0, 16.0, None, None, None);
        state.push_health("n1", 20.0, 2.0, 16.0, None, None, None);
        state.push_health("n1", 30.0, 3.0, 16.0, None, None, None);
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
            state.push_health("n1", i as f32, 0.0, 16.0, None, None, None);
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
        state.push_health("n1", 10.0, 1.0, 8.0, None, None, None);
        state.push_health("n2", 50.0, 4.0, 8.0, None, None, None);
        state.push_health("n1", 15.0, 1.5, 8.0, None, None, None);
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
        state.push_health("n1", 0.0, 0.0, 0.0, None, None, None);
    }

    #[test]
    fn health_sample_serializes_expected_fields() {
        let s = HealthSample {
            ts_ms: 1_000_000,
            cpu_pct: 33.3,
            ram_used_gb: 4.0,
            ram_total_gb: 16.0,
            gpu_pct: None,
            gpu_vram_used_gb: None,
            gpu_vram_total_gb: None,
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"ts_ms\""));
        assert!(json.contains("\"cpu_pct\""));
        assert!(json.contains("\"ram_used_gb\""));
        assert!(json.contains("\"ram_total_gb\""));
    }

    #[test]
    fn health_sample_gpu_fields_omitted_when_none() {
        let s = HealthSample {
            ts_ms: 1_000,
            cpu_pct: 10.0,
            ram_used_gb: 1.0,
            ram_total_gb: 8.0,
            gpu_pct: None,
            gpu_vram_used_gb: None,
            gpu_vram_total_gb: None,
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(
            !json.contains("gpu_pct"),
            "None gpu_pct should be absent: {json}"
        );
        assert!(
            !json.contains("gpu_vram"),
            "None gpu_vram fields should be absent: {json}"
        );
    }

    #[test]
    fn health_sample_gpu_fields_present_when_some() {
        let s = HealthSample {
            ts_ms: 1_000,
            cpu_pct: 10.0,
            ram_used_gb: 1.0,
            ram_total_gb: 8.0,
            gpu_pct: Some(42.0),
            gpu_vram_used_gb: Some(3.7),
            gpu_vram_total_gb: Some(4.0),
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"gpu_pct\":42.0"), "gpu_pct missing: {json}");
        assert!(
            json.contains("\"gpu_vram_used_gb\""),
            "gpu_vram_used_gb missing: {json}"
        );
        assert!(
            json.contains("\"gpu_vram_total_gb\""),
            "gpu_vram_total_gb missing: {json}"
        );
    }

    #[test]
    fn push_health_with_gpu_data_broadcasts_gpu_fields() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        let mut rx = state.tx.subscribe();
        state.push_health("n1", 55.0, 4.0, 16.0, Some(42.0), Some(3.7), Some(4.0));
        match rx.try_recv().unwrap() {
            DashboardEvent::HealthUpdate { samples, .. } => {
                assert_eq!(samples[0].gpu_pct, Some(42.0));
                assert_eq!(samples[0].gpu_vram_used_gb, Some(3.7));
                assert_eq!(samples[0].gpu_vram_total_gb, Some(4.0));
            }
            _ => panic!("expected HealthUpdate"),
        }
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
                gpu_pct: None,
                gpu_vram_used_gb: None,
                gpu_vram_total_gb: None,
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
    fn get_all_health_snapshots_returns_all_nodes() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_health("n1", 10.0, 1.0, 8.0, None, None, None);
        state.push_health("n2", 20.0, 2.0, 8.0, None, None, None);
        state.push_health("n1", 15.0, 1.5, 8.0, None, None, None);
        let mut snaps = state.get_all_health_snapshots();
        snaps.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(snaps.len(), 2);
        assert_eq!(snaps[0].0, "n1");
        assert_eq!(snaps[0].1.len(), 2);
        assert_eq!(snaps[1].0, "n2");
        assert_eq!(snaps[1].1.len(), 1);
    }

    #[test]
    fn get_all_health_snapshots_empty_when_no_data() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        assert!(state.get_all_health_snapshots().is_empty());
    }

    #[test]
    fn get_all_health_snapshots_is_point_in_time_copy() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_health("n1", 10.0, 1.0, 8.0, None, None, None);
        let snap = state.get_all_health_snapshots();
        // Push more data after snapshot — snapshot must not change.
        state.push_health("n1", 99.0, 7.0, 8.0, None, None, None);
        assert_eq!(
            snap[0].1.len(),
            1,
            "snapshot should not reflect post-snapshot pushes"
        );
        assert_eq!(snap[0].1[0].cpu_pct, 10.0);
    }

    #[test]
    fn get_all_health_snapshots_preserves_sample_values() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_health("n1", 33.3, 4.5, 16.0, None, None, None);
        let snaps = state.get_all_health_snapshots();
        let s = &snaps[0].1[0];
        assert_eq!(s.cpu_pct, 33.3);
        assert_eq!(s.ram_used_gb, 4.5);
        assert_eq!(s.ram_total_gb, 16.0);
        assert!(s.ts_ms > 0);
    }

    #[test]
    fn get_all_health_snapshots_includes_single_sample_node() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_health("solo", 50.0, 2.0, 8.0, None, None, None);
        let snaps = state.get_all_health_snapshots();
        assert_eq!(snaps.len(), 1);
        assert_eq!(snaps[0].0, "solo");
        assert_eq!(snaps[0].1.len(), 1);
    }

    #[test]
    fn get_all_health_snapshots_preserves_sample_order() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_health("n1", 10.0, 1.0, 8.0, None, None, None);
        state.push_health("n1", 20.0, 2.0, 8.0, None, None, None);
        state.push_health("n1", 30.0, 3.0, 8.0, None, None, None);
        let snaps = state.get_all_health_snapshots();
        let samp = &snaps[0].1;
        assert_eq!(samp[0].cpu_pct, 10.0, "oldest first");
        assert_eq!(samp[1].cpu_pct, 20.0);
        assert_eq!(samp[2].cpu_pct, 30.0, "newest last");
    }

    // ── push_device_discovery ────────────────────────────────────────────────

    #[test]
    fn push_device_discovery_adds_placeholders() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_device_discovery("n1", vec!["new_bulb".into()], true);
        let snap = state.get_light_snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].device_id, "new_bulb");
        assert_eq!(snap[0].node_id, "n1");
        assert!(!snap[0].on);
    }

    #[test]
    fn push_device_discovery_skips_existing_devices() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        // Add a real report first
        state.push_lighting_update(LightStateReport {
            node_id: "n1".into(),
            device_id: "known_bulb".into(),
            on: true,
            brightness: Some(100),
            color_xy: None,
            color_temp: None,
        });
        // Discover it again
        state.push_device_discovery("n1", vec!["known_bulb".into()], true);
        let snap = state.get_light_snapshot();
        assert_eq!(snap.len(), 1);
        assert!(
            snap[0].on,
            "Real state should not be overwritten by placeholder"
        );
    }

    // ── push_model_update / get_model_snapshot ───────────────────────────────

    fn make_node_model_info(node_id: &str) -> NodeModelInfo {
        NodeModelInfo {
            node_id: node_id.into(),
            hostname: "host".into(),
            role: "Compute".into(),
            ram_gb: 16.0,
            models: vec![ModelEntry {
                name: "qwen2.5:7b".into(),
                size_mb: 4000,
                state: "Ready".into(),
                reason: None,
            }],
        }
    }

    #[test]
    fn push_model_update_stores_snapshot() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_model_update(vec![make_node_model_info("n1")]);
        let snap = state.get_model_snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].node_id, "n1");
        assert_eq!(snap[0].models[0].name, "qwen2.5:7b");
    }

    #[test]
    fn push_model_update_broadcasts_event() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        let mut rx = state.tx.subscribe();
        state.push_model_update(vec![make_node_model_info("n1")]);
        match rx.try_recv().unwrap() {
            DashboardEvent::ModelUpdate { nodes } => {
                assert_eq!(nodes.len(), 1);
                assert_eq!(nodes[0].node_id, "n1");
                assert_eq!(nodes[0].ram_gb, 16.0);
            }
            _ => panic!("expected ModelUpdate"),
        }
    }

    #[test]
    fn push_model_update_with_no_receivers_stores_snapshot_anyway() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        // No subscriber — snapshot must still be stored for future connects.
        state.push_model_update(vec![make_node_model_info("n1")]);
        assert_eq!(state.get_model_snapshot().len(), 1);
    }

    #[test]
    fn get_model_snapshot_is_point_in_time_copy() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_model_update(vec![make_node_model_info("n1")]);
        let snap = state.get_model_snapshot();
        state.push_model_update(vec![make_node_model_info("n2")]);
        assert_eq!(
            snap.len(),
            1,
            "snapshot should not reflect post-snapshot updates"
        );
        assert_eq!(snap[0].node_id, "n1");
    }

    #[test]
    fn model_update_event_wire_format() {
        let evt = DashboardEvent::ModelUpdate {
            nodes: vec![NodeModelInfo {
                node_id: "abc".into(),
                hostname: "beelink1".into(),
                role: "Compute".into(),
                ram_gb: 32.0,
                models: vec![ModelEntry {
                    name: "qwen2.5:7b".into(),
                    size_mb: 4000,
                    state: "Ready".into(),
                    reason: None,
                }],
            }],
        };
        let json = serde_json::to_string(&evt).unwrap();
        assert!(
            json.contains("\"type\":\"ModelUpdate\""),
            "missing type tag: {json}"
        );
        assert!(
            json.contains("\"node_id\":\"abc\""),
            "missing node_id: {json}"
        );
        assert!(json.contains("\"ram_gb\":32.0"), "missing ram_gb: {json}");
        assert!(
            json.contains("\"state\":\"Ready\""),
            "missing state: {json}"
        );
        assert!(
            !json.contains("\"reason\""),
            "reason should be absent when None: {json}"
        );
    }

    // ── push_lighting_update / get_light_snapshot ────────────────────────────

    fn make_light_report(device_id: &str, on: bool) -> LightStateReport {
        LightStateReport {
            node_id: "lighting-node".into(),
            device_id: device_id.into(),
            on,
            brightness: Some(200),
            color_xy: None,
            color_temp: Some(370),
        }
    }

    #[test]
    fn push_lighting_update_stores_snapshot() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_lighting_update(make_light_report("bulb1", true));
        let snap = state.get_light_snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].device_id, "bulb1");
        assert!(snap[0].on);
    }

    #[test]
    fn push_lighting_update_broadcasts_event() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        let mut rx = state.tx.subscribe();
        state.push_lighting_update(make_light_report("bulb1", true));
        match rx.try_recv().unwrap() {
            DashboardEvent::LightingUpdate { devices, .. } => {
                assert_eq!(devices.len(), 1);
                assert_eq!(devices[0].device_id, "bulb1");
                assert!(devices[0].on);
            }
            _ => panic!("expected LightingUpdate"),
        }
    }

    #[test]
    fn push_lighting_update_with_no_receivers_stores_snapshot_anyway() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_lighting_update(make_light_report("bulb1", false));
        assert_eq!(state.get_light_snapshot().len(), 1);
    }

    #[test]
    fn push_lighting_update_overwrites_same_device() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_lighting_update(make_light_report("bulb1", true));
        state.push_lighting_update(make_light_report("bulb1", false));
        let snap = state.get_light_snapshot();
        assert_eq!(
            snap.len(),
            1,
            "same device should overwrite, not accumulate"
        );
        assert!(!snap[0].on, "latest state should win");
    }

    #[test]
    fn get_light_snapshot_returns_all_devices() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_lighting_update(make_light_report("bulb1", true));
        state.push_lighting_update(make_light_report("bulb2", false));
        let snap = state.get_light_snapshot();
        assert_eq!(snap.len(), 2);
    }

    #[test]
    fn lighting_update_event_wire_format() {
        let evt = DashboardEvent::LightingUpdate {
            devices: vec![LightStateReport {
                node_id: "n1".into(),
                device_id: "test_bulb".into(),
                on: true,
                brightness: Some(200),
                color_xy: Some((0.3, 0.3)),
                color_temp: Some(370),
            }],
            groups: vec!["all".into()],
        };
        let json = serde_json::to_string(&evt).unwrap();
        assert!(
            json.contains("\"type\":\"LightingUpdate\""),
            "missing type tag: {json}"
        );
        assert!(
            json.contains("\"device_id\":\"test_bulb\""),
            "missing device_id: {json}"
        );
        assert!(json.contains("\"on\":true"), "missing on field: {json}");
        assert!(
            json.contains("\"brightness\":200"),
            "missing brightness: {json}"
        );
        assert!(
            json.contains("\"color_temp\":370"),
            "missing color_temp: {json}"
        );
        assert!(json.contains("\"groups\""), "missing groups field: {json}");
        assert!(json.contains("\"all\""), "missing group name: {json}");
    }

    // ── push_group_update / get_group_snapshot ───────────────────────────────

    #[test]
    fn push_group_update_stores_groups() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_group_update("pi1", vec!["all".into(), "living_room".into()]);
        let snap = state.get_group_snapshot();
        assert_eq!(snap.len(), 2);
        assert!(snap.contains(&"all".to_string()));
        assert!(snap.contains(&"living_room".to_string()));
    }

    #[test]
    fn push_group_update_replaces_groups_for_same_node() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_group_update("pi1", vec!["all".into(), "old_group".into()]);
        state.push_group_update("pi1", vec!["all".into()]);
        let snap = state.get_group_snapshot();
        assert_eq!(
            snap.len(),
            1,
            "old_group should be replaced, not accumulated"
        );
        assert!(snap.contains(&"all".to_string()));
    }

    #[test]
    fn get_node_for_group_returns_node_id() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_group_update("pi1", vec!["all".into()]);
        assert_eq!(state.get_node_for_group("all"), Some("pi1".into()));
    }

    #[test]
    fn get_node_for_group_returns_none_for_unknown() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        assert!(state.get_node_for_group("nonexistent").is_none());
    }

    #[test]
    fn get_node_for_device_returns_node_id() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_lighting_update(make_light_report("bulb1", true));
        assert_eq!(
            state.get_node_for_device("bulb1"),
            Some("lighting-node".into())
        );
    }

    #[test]
    fn get_node_for_device_returns_none_for_unknown() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        assert!(state.get_node_for_device("no-such-bulb").is_none());
    }

    // ── push_rooms_update / get_room_snapshot ─────────────────────────────────

    fn make_room_info(id: &str, name: &str) -> RoomInfo {
        RoomInfo {
            id: id.into(),
            name: name.into(),
            position: 0,
            device_ids: vec!["bulb1".into()],
            orientation_degrees: 0.0,
            has_window: false,
            window_facing: None,
            width_m: 3.0,
            depth_m: 6.0,
            height_m: 2.5,
            origin_x: 0.5,
            origin_y: 0.5,
        }
    }

    #[test]
    fn push_rooms_update_stores_snapshot() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_rooms_update(vec![make_room_info("r1", "Living Room")]);
        let snap = state.get_room_snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].id, "r1");
        assert_eq!(snap[0].name, "Living Room");
    }

    #[test]
    fn push_rooms_update_replaces_snapshot() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_rooms_update(vec![make_room_info("r1", "Old")]);
        state.push_rooms_update(vec![make_room_info("r2", "New")]);
        let snap = state.get_room_snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].id, "r2");
    }

    #[test]
    fn push_rooms_update_broadcasts_event() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        let mut rx = state.tx.subscribe();
        state.push_rooms_update(vec![make_room_info("r1", "Bedroom")]);
        match rx.try_recv().unwrap() {
            DashboardEvent::RoomsUpdate { rooms, .. } => {
                assert_eq!(rooms.len(), 1);
                assert_eq!(rooms[0].name, "Bedroom");
            }
            _ => panic!("expected RoomsUpdate"),
        }
    }

    #[test]
    fn push_rooms_update_with_no_receivers_stores_snapshot_anyway() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_rooms_update(vec![make_room_info("r1", "Hall")]);
        assert_eq!(state.get_room_snapshot().len(), 1);
    }

    #[test]
    fn rooms_update_event_wire_format() {
        let evt = DashboardEvent::RoomsUpdate {
            rooms: vec![RoomInfo {
                id: "abc-123".into(),
                name: "Living Room".into(),
                position: 0,
                device_ids: vec!["test_bulb".into()],
                orientation_degrees: 0.0,
                has_window: false,
                window_facing: None,
                width_m: 3.0,
                depth_m: 6.0,
                height_m: 2.5,
                origin_x: 0.5,
                origin_y: 0.5,
            }],
            device_names: std::collections::HashMap::new(),
        };
        let json = serde_json::to_string(&evt).unwrap();
        assert!(
            json.contains("\"type\":\"RoomsUpdate\""),
            "missing type tag: {json}"
        );
        assert!(json.contains("\"id\":\"abc-123\""), "missing id: {json}");
        assert!(
            json.contains("\"name\":\"Living Room\""),
            "missing name: {json}"
        );
        assert!(
            json.contains("\"device_ids\""),
            "missing device_ids: {json}"
        );
    }

    // ── push_scenes_update / get_scene_snapshot ──────────────────────────────

    fn make_scene_info(id: &str, name: &str, room_id: Option<&str>) -> SceneInfo {
        SceneInfo {
            id: id.into(),
            name: name.into(),
            room_id: room_id.map(|s| s.into()),
            created_at: 1_000_000,
            position: 0,
            preview_color: None,
        }
    }

    #[test]
    fn push_scenes_update_stores_snapshot() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_scenes_update(vec![make_scene_info("s1", "Evening", Some("r1"))]);
        let snap = state.get_scene_snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].id, "s1");
        assert_eq!(snap[0].name, "Evening");
        assert_eq!(snap[0].room_id, Some("r1".into()));
    }

    #[test]
    fn push_scenes_update_replaces_snapshot() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_scenes_update(vec![make_scene_info("s1", "Old", None)]);
        state.push_scenes_update(vec![make_scene_info("s2", "New", None)]);
        let snap = state.get_scene_snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].id, "s2");
    }

    #[test]
    fn push_scenes_update_broadcasts_event() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        let mut rx = state.tx.subscribe();
        state.push_scenes_update(vec![make_scene_info("s1", "Night", Some("r1"))]);
        match rx.try_recv().unwrap() {
            DashboardEvent::ScenesUpdate { scenes } => {
                assert_eq!(scenes.len(), 1);
                assert_eq!(scenes[0].name, "Night");
            }
            _ => panic!("expected ScenesUpdate"),
        }
    }

    #[test]
    fn push_scenes_update_with_no_receivers_stores_snapshot_anyway() {
        let state = DashboardState::new(
            Arc::new(vec![]),
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        );
        state.push_scenes_update(vec![make_scene_info("s1", "Morning", None)]);
        assert_eq!(state.get_scene_snapshot().len(), 1);
    }

    #[test]
    fn scenes_update_event_wire_format() {
        let evt = DashboardEvent::ScenesUpdate {
            scenes: vec![SceneInfo {
                id: "abc-123".into(),
                name: "Evening".into(),
                room_id: Some("room-456".into()),
                created_at: 1_700_000_000,
                position: 0,
                preview_color: None,
            }],
        };
        let json = serde_json::to_string(&evt).unwrap();
        assert!(
            json.contains("\"type\":\"ScenesUpdate\""),
            "missing type tag: {json}"
        );
        assert!(json.contains("\"id\":\"abc-123\""), "missing id: {json}");
        assert!(
            json.contains("\"name\":\"Evening\""),
            "missing name: {json}"
        );
        assert!(
            json.contains("\"room_id\":\"room-456\""),
            "missing room_id: {json}"
        );
    }

    #[test]
    fn scene_info_room_id_omitted_when_none() {
        let evt = DashboardEvent::ScenesUpdate {
            scenes: vec![SceneInfo {
                id: "s1".into(),
                name: "Global".into(),
                room_id: None,
                created_at: 0,
                position: 0,
                preview_color: None,
            }],
        };
        let json = serde_json::to_string(&evt).unwrap();
        assert!(
            !json.contains("\"room_id\""),
            "None room_id should be absent: {json}"
        );
    }
}
