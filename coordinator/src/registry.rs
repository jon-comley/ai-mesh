use rusqlite::{Connection, params};
use shared::{
    HardwareSpec, ModelAllocationFull, ModelLifecycleState, NodeCapabilities, NodeIdentity,
    NodeRecordFull, NodeRecordLite, NodeRole,
};
use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime};
use tracing::warn;

fn gen_uuid() -> String {
    let mut b = [0u8; 16];
    getrandom::getrandom(&mut b).expect("rng failed");
    b[6] = (b[6] & 0x0f) | 0x40; // version 4
    b[8] = (b[8] & 0x3f) | 0x80; // variant RFC 4122
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0],
        b[1],
        b[2],
        b[3],
        b[4],
        b[5],
        b[6],
        b[7],
        b[8],
        b[9],
        b[10],
        b[11],
        b[12],
        b[13],
        b[14],
        b[15]
    )
}

#[derive(Debug, Clone)]
pub struct RoomRecord {
    pub id: String,
    pub name: String,
    pub position: i64,
    pub device_ids: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ModelAllocation {
    pub model_name: String,
    pub size_mb: u64,
    pub state: ModelLifecycleState,
    pub last_updated: Instant,
}

#[derive(Debug, Clone)]
pub struct NodeRecord {
    pub identity: NodeIdentity,
    pub hardware: Option<HardwareSpec>,
    pub capabilities: Option<NodeCapabilities>,
    pub last_heartbeat: SystemTime,
    pub models: HashMap<String, ModelAllocation>,
}

#[derive(Debug)]
pub struct Registry {
    nodes: HashMap<String, NodeRecord>,
    conn: Connection,
    /// Known Zigbee devices and groups per lighting node, keyed by node_id.
    light_devices: HashMap<String, (Vec<String>, Vec<String>)>,
}

fn init_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
        CREATE TABLE IF NOT EXISTS nodes (
            id            TEXT PRIMARY KEY,
            hostname      TEXT NOT NULL,
            ip            TEXT NOT NULL,
            role          TEXT NOT NULL,
            last_seen     INTEGER NOT NULL,
            hardware_spec TEXT,
            capabilities  TEXT
        );
        CREATE TABLE IF NOT EXISTS model_allocations (
            node_id      TEXT NOT NULL,
            model_name   TEXT NOT NULL,
            size_mb      INTEGER NOT NULL,
            state        TEXT NOT NULL,
            last_updated INTEGER NOT NULL,
            PRIMARY KEY (node_id, model_name)
        );
        CREATE TABLE IF NOT EXISTS light_devices (
            node_id  TEXT PRIMARY KEY,
            devices  TEXT NOT NULL,
            groups   TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS rooms (
            id       TEXT PRIMARY KEY,
            name     TEXT NOT NULL,
            position INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS room_devices (
            room_id   TEXT NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
            device_id TEXT NOT NULL,
            PRIMARY KEY (room_id, device_id)
        );",
    )
}

fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

impl Registry {
    /// In-memory registry — used by tests and Coordinator::new().
    pub fn new() -> Self {
        let conn = Connection::open_in_memory().expect("in-memory SQLite");
        init_schema(&conn).expect("schema init");
        Self {
            nodes: HashMap::new(),
            conn,
            light_devices: HashMap::new(),
        }
    }

    /// Persistent registry backed by a file. Opens or creates `path`.
    /// Existing rows are loaded into the in-memory map on construction.
    pub fn open(path: &str) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        init_schema(&conn)?;
        let mut reg = Self {
            nodes: HashMap::new(),
            conn,
            light_devices: HashMap::new(),
        };
        reg.load_from_db()?;
        Ok(reg)
    }

    fn load_from_db(&mut self) -> rusqlite::Result<()> {
        type NodeRow = (
            String,
            String,
            String,
            String,
            i64,
            Option<String>,
            Option<String>,
        );

        // ── nodes ──────────────────────────────────────────────────────────
        #[allow(clippy::type_complexity)]
        let node_rows: Vec<NodeRow> = {
            let mut stmt = self.conn.prepare(
                "SELECT id, hostname, ip, role, last_seen, hardware_spec, capabilities FROM nodes",
            )?;
            stmt.query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            })?
            .collect::<rusqlite::Result<_>>()?
        };

        for (id, hostname, ip, role_json, last_seen_secs, hw_json, caps_json) in node_rows {
            let role: NodeRole = serde_json::from_str(&role_json).unwrap_or(NodeRole::Compute);
            let hardware: Option<HardwareSpec> =
                hw_json.and_then(|j| serde_json::from_str(&j).ok());
            let capabilities: Option<NodeCapabilities> =
                caps_json.and_then(|j| serde_json::from_str(&j).ok());
            let last_heartbeat =
                SystemTime::UNIX_EPOCH + Duration::from_secs(last_seen_secs.max(0) as u64);

            self.nodes.insert(
                id.clone(),
                NodeRecord {
                    identity: NodeIdentity {
                        id,
                        hostname,
                        ip,
                        role,
                    },
                    hardware,
                    capabilities,
                    last_heartbeat,
                    models: HashMap::new(),
                },
            );
        }

        // ── model_allocations ──────────────────────────────────────────────
        let alloc_rows: Vec<(String, String, i64, String, i64)> = {
            let mut stmt = self.conn.prepare(
                "SELECT node_id, model_name, size_mb, state, last_updated FROM model_allocations",
            )?;
            stmt.query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })?
            .collect::<rusqlite::Result<_>>()?
        };

        for (node_id, model_name, size_mb, state_json, _last_updated) in alloc_rows {
            let state: ModelLifecycleState =
                serde_json::from_str(&state_json).unwrap_or(ModelLifecycleState::Unloaded);
            if let Some(node) = self.nodes.get_mut(&node_id) {
                node.models.insert(
                    model_name.clone(),
                    ModelAllocation {
                        model_name,
                        size_mb: size_mb as u64,
                        state,
                        last_updated: Instant::now(),
                    },
                );
            }
        }

        // ── light_devices ──────────────────────────────────────────────────
        let ld_rows: Vec<(String, String, String)> = {
            let mut stmt = self
                .conn
                .prepare("SELECT node_id, devices, groups FROM light_devices")?;
            stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
                .collect::<rusqlite::Result<_>>()?
        };

        for (node_id, devices_json, groups_json) in ld_rows {
            let devices: Vec<String> = serde_json::from_str(&devices_json).unwrap_or_default();
            let groups: Vec<String> = serde_json::from_str(&groups_json).unwrap_or_default();
            self.light_devices.insert(node_id, (devices, groups));
        }

        Ok(())
    }

    pub fn update_heartbeat(&mut self, identity: NodeIdentity) {
        let entry = self.nodes.entry(identity.id.clone()).or_insert(NodeRecord {
            identity: identity.clone(),
            hardware: None,
            capabilities: None,
            last_heartbeat: SystemTime::now(),
            models: HashMap::new(),
        });
        entry.identity = identity.clone();
        entry.last_heartbeat = SystemTime::now();

        let role_json = serde_json::to_string(&identity.role).unwrap_or_default();
        if let Err(e) = self.conn.execute(
            "INSERT INTO nodes (id, hostname, ip, role, last_seen)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET
                 hostname  = excluded.hostname,
                 ip        = excluded.ip,
                 role      = excluded.role,
                 last_seen = excluded.last_seen",
            params![
                identity.id,
                identity.hostname,
                identity.ip,
                role_json,
                now_unix_secs()
            ],
        ) {
            warn!(error = %e, "DB heartbeat upsert failed");
        }
    }

    pub fn update_hardware(&mut self, id: &str, hardware: HardwareSpec) {
        if let Some(node) = self.nodes.get_mut(id) {
            node.hardware = Some(hardware.clone());
        }
        let hw_json = serde_json::to_string(&hardware).unwrap_or_default();
        if let Err(e) = self.conn.execute(
            "UPDATE nodes SET hardware_spec = ?1 WHERE id = ?2",
            params![hw_json, id],
        ) {
            warn!(error = %e, "DB hardware update failed");
        }
    }

    pub fn update_capabilities(&mut self, id: &str, capabilities: NodeCapabilities) {
        if let Some(node) = self.nodes.get_mut(id) {
            node.capabilities = Some(capabilities.clone());
        }
        let caps_json = serde_json::to_string(&capabilities).unwrap_or_default();
        if let Err(e) = self.conn.execute(
            "UPDATE nodes SET capabilities = ?1 WHERE id = ?2",
            params![caps_json, id],
        ) {
            warn!(error = %e, "DB capabilities update failed");
        }
    }

    pub fn update_model_status(
        &mut self,
        id: &str,
        model_name: &str,
        size_mb: u64,
        state: ModelLifecycleState,
    ) {
        if let Some(node) = self.nodes.get_mut(id) {
            node.models.insert(
                model_name.to_string(),
                ModelAllocation {
                    model_name: model_name.to_string(),
                    size_mb,
                    state: state.clone(),
                    last_updated: Instant::now(),
                },
            );
        }
        let state_json = serde_json::to_string(&state).unwrap_or_default();
        if let Err(e) = self.conn.execute(
            "INSERT INTO model_allocations (node_id, model_name, size_mb, state, last_updated)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(node_id, model_name) DO UPDATE SET
                 size_mb      = excluded.size_mb,
                 state        = excluded.state,
                 last_updated = excluded.last_updated",
            params![id, model_name, size_mb as i64, state_json, now_unix_secs()],
        ) {
            warn!(error = %e, "DB model_allocations upsert failed");
        }
    }

    pub fn get(&self, id: &str) -> Option<&NodeRecord> {
        self.nodes.get(id)
    }

    pub fn prune_stale(&mut self, max_age: Duration) {
        let now = SystemTime::now();
        self.nodes.retain(|_, record| {
            now.duration_since(record.last_heartbeat)
                .map(|age| age < max_age)
                .unwrap_or(false)
        });
    }

    pub fn count(&self) -> usize {
        self.nodes.len()
    }

    pub fn first_node_id(&self) -> Option<String> {
        self.nodes.keys().next().cloned()
    }

    pub fn eligible_compute_nodes(&self) -> Vec<NodeRecordFull> {
        self.nodes
            .values()
            .filter(|n| n.identity.role == NodeRole::Compute)
            .filter_map(|rec| self.get_node_full(&rec.identity.id))
            .collect()
    }

    pub fn list_nodes(&self) -> Vec<NodeRecordLite> {
        let now = SystemTime::now();
        self.nodes
            .values()
            .map(|rec| {
                let age = now
                    .duration_since(rec.last_heartbeat)
                    .map(|d| d.as_millis())
                    .unwrap_or(0);
                NodeRecordLite {
                    id: rec.identity.id.clone(),
                    hostname: rec.identity.hostname.clone(),
                    ip: rec.identity.ip.clone(),
                    role: rec.identity.role.clone(),
                    last_heartbeat_ms: age,
                }
            })
            .collect()
    }

    pub fn clear_all(&mut self) {
        self.nodes.clear();
        if let Err(e) = self
            .conn
            .execute_batch("DELETE FROM nodes; DELETE FROM model_allocations;")
        {
            warn!(error = %e, "DB clear_all failed");
        }
    }

    /// Returns all Compute nodes whose reported capabilities include `feature`.
    pub fn nodes_with_feature(&self, feature: &str) -> Vec<NodeRecordFull> {
        self.nodes
            .values()
            .filter(|n| {
                n.capabilities
                    .as_ref()
                    .map(|c| c.features.iter().any(|f| f == feature))
                    .unwrap_or(false)
            })
            .filter_map(|rec| self.get_node_full(&rec.identity.id))
            .collect()
    }

    /// Returns the name of any model in Ready state on any LLM-capable Compute node.
    /// Used by the intent router when no model_name is specified.
    /// Returns the name of the largest ready LLM model across all compute nodes.
    /// Largest-by-size is used so intents are always routed to the most capable available model.
    pub fn any_ready_llm_model(&self) -> Option<String> {
        self.nodes
            .values()
            .filter(|n| {
                n.identity.role == NodeRole::Compute
                    && n.capabilities
                        .as_ref()
                        .map(|c| c.features.iter().any(|f| f == "llm"))
                        .unwrap_or(false)
            })
            .flat_map(|n| n.models.iter())
            .filter(|(_, alloc)| alloc.state == ModelLifecycleState::Ready)
            .max_by_key(|(_, alloc)| alloc.size_mb)
            .map(|(name, _)| name.clone())
    }

    /// Store the list of known Zigbee devices and groups reported by a lighting node.
    pub fn update_light_devices(
        &mut self,
        node_id: &str,
        devices: Vec<String>,
        groups: Vec<String>,
    ) {
        let devices_json = serde_json::to_string(&devices).unwrap_or_default();
        let groups_json = serde_json::to_string(&groups).unwrap_or_default();
        if let Err(e) = self.conn.execute(
            "INSERT OR REPLACE INTO light_devices (node_id, devices, groups) VALUES (?1, ?2, ?3)",
            params![node_id, devices_json, groups_json],
        ) {
            warn!(error = %e, "DB light_devices upsert failed");
        }
        self.light_devices
            .insert(node_id.to_owned(), (devices, groups));
    }

    /// Returns all known Zigbee device and group friendly names across all lighting nodes.
    pub fn all_light_device_names(&self) -> (Vec<String>, Vec<String>) {
        let mut devices: std::collections::HashSet<String> = Default::default();
        let mut groups: std::collections::HashSet<String> = Default::default();
        for (devs, grps) in self.light_devices.values() {
            devices.extend(devs.iter().cloned());
            groups.extend(grps.iter().cloned());
        }
        (devices.into_iter().collect(), groups.into_iter().collect())
    }

    pub fn get_node_full(&self, id: &str) -> Option<NodeRecordFull> {
        let now = SystemTime::now();
        let rec = self.nodes.get(id)?;
        let age = now
            .duration_since(rec.last_heartbeat)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        Some(NodeRecordFull {
            id: rec.identity.id.clone(),
            hostname: rec.identity.hostname.clone(),
            ip: rec.identity.ip.clone(),
            role: rec.identity.role.clone(),
            last_heartbeat_ms: age,
            hardware: rec.hardware.clone(),
            capabilities: rec.capabilities.clone(),
            models: rec
                .models
                .values()
                .map(|m| ModelAllocationFull {
                    model_name: m.model_name.clone(),
                    size_mb: m.size_mb,
                    state: m.state.clone(),
                })
                .collect(),
        })
    }

    /// Returns true if any Compute node currently has `model_name` in Loading state.
    /// Used by the inference handler to decide whether to wait for a pull to complete.
    pub fn model_is_loading(&self, model_name: &str) -> bool {
        self.nodes.values().any(|node| {
            node.identity.role == NodeRole::Compute
                && node
                    .models
                    .get(model_name)
                    .map(|m| m.state == ModelLifecycleState::Loading)
                    .unwrap_or(false)
        })
    }

    // ── Rooms ─────────────────────────────────────────────────────────────────

    fn room_device_ids(&self, room_id: &str) -> Vec<String> {
        let mut stmt = match self
            .conn
            .prepare("SELECT device_id FROM room_devices WHERE room_id = ?1 ORDER BY device_id")
        {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "room_device_ids prepare failed");
                return vec![];
            }
        };
        stmt.query_map(params![room_id], |row| row.get(0))
            .map(|rows| {
                rows.collect::<rusqlite::Result<Vec<String>>>()
                    .unwrap_or_default()
            })
            .unwrap_or_default()
    }

    pub fn list_rooms(&self) -> Vec<RoomRecord> {
        let mut stmt = match self
            .conn
            .prepare("SELECT id, name, position FROM rooms ORDER BY position, name")
        {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "list_rooms prepare failed");
                return vec![];
            }
        };
        let rows: Vec<(String, String, i64)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .map(|r| r.collect::<rusqlite::Result<_>>().unwrap_or_default())
            .unwrap_or_default();
        rows.into_iter()
            .map(|(id, name, position)| {
                let device_ids = self.room_device_ids(&id);
                RoomRecord {
                    id,
                    name,
                    position,
                    device_ids,
                }
            })
            .collect()
    }

    pub fn create_room(&mut self, name: &str) -> RoomRecord {
        let id = gen_uuid();
        if let Err(e) = self.conn.execute(
            "INSERT INTO rooms (id, name, position) VALUES (?1, ?2, 0)",
            params![id, name],
        ) {
            warn!(error = %e, "create_room failed");
        }
        RoomRecord {
            id,
            name: name.to_owned(),
            position: 0,
            device_ids: vec![],
        }
    }

    /// Returns true if a room with this id exists.
    pub fn room_exists(&self, id: &str) -> bool {
        self.conn
            .query_row("SELECT 1 FROM rooms WHERE id = ?1", params![id], |_| Ok(()))
            .is_ok()
    }

    pub fn delete_room(&mut self, id: &str) {
        // CASCADE in room_devices handles membership cleanup.
        if let Err(e) = self
            .conn
            .execute("DELETE FROM rooms WHERE id = ?1", params![id])
        {
            warn!(error = %e, "delete_room failed");
        }
    }

    pub fn rename_room(&mut self, id: &str, name: &str) {
        if let Err(e) = self.conn.execute(
            "UPDATE rooms SET name = ?1 WHERE id = ?2",
            params![name, id],
        ) {
            warn!(error = %e, "rename_room failed");
        }
    }

    /// Move a device into a room, removing it from any other room first.
    pub fn add_device_to_room(&mut self, room_id: &str, device_id: &str) {
        if let Err(e) = self.conn.execute(
            "DELETE FROM room_devices WHERE device_id = ?1",
            params![device_id],
        ) {
            warn!(error = %e, "add_device_to_room evict failed");
        }
        if let Err(e) = self.conn.execute(
            "INSERT OR IGNORE INTO room_devices (room_id, device_id) VALUES (?1, ?2)",
            params![room_id, device_id],
        ) {
            warn!(error = %e, "add_device_to_room insert failed");
        }
    }

    pub fn remove_device_from_room(&mut self, room_id: &str, device_id: &str) {
        if let Err(e) = self.conn.execute(
            "DELETE FROM room_devices WHERE room_id = ?1 AND device_id = ?2",
            params![room_id, device_id],
        ) {
            warn!(error = %e, "remove_device_from_room failed");
        }
    }

    /// Returns the room_id the device is currently assigned to, if any.
    pub fn get_room_for_device(&self, device_id: &str) -> Option<String> {
        self.conn
            .query_row(
                "SELECT room_id FROM room_devices WHERE device_id = ?1",
                params![device_id],
                |row| row.get(0),
            )
            .ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::NodeRole;

    fn sample_identity(id: &str) -> NodeIdentity {
        NodeIdentity {
            id: id.to_string(),
            hostname: "test-host".into(),
            ip: "127.0.0.1".into(),
            role: NodeRole::Compute,
        }
    }

    fn sample_hardware() -> HardwareSpec {
        HardwareSpec {
            cpu_model: "Test CPU".into(),
            cpu_cores: 4,
            cpu_threads: 8,
            ram_gb: 16.0,
            os: "linux".into(),
            arch: "x86_64".into(),
            gpu: None,
        }
    }

    fn sample_capabilities() -> NodeCapabilities {
        NodeCapabilities {
            cpu_inference: true,
            gpu_inference: false,
            ane_inference: false,
            max_model_size_gb: 8.0,
            features: vec!["llm".into()],
        }
    }

    #[test]
    fn test_heartbeat_inserts_node() {
        let mut reg = Registry::new();
        reg.update_heartbeat(sample_identity("node1"));
        assert!(reg.get("node1").is_some());
    }

    #[test]
    fn test_update_hardware() {
        let mut reg = Registry::new();
        reg.update_heartbeat(sample_identity("node1"));
        reg.update_hardware("node1", sample_hardware());
        assert!(reg.get("node1").unwrap().hardware.is_some());
    }

    #[test]
    fn test_update_capabilities() {
        let mut reg = Registry::new();
        reg.update_heartbeat(sample_identity("node1"));
        reg.update_capabilities("node1", sample_capabilities());
        assert!(reg.get("node1").unwrap().capabilities.is_some());
    }

    #[test]
    fn test_prune_stale() {
        let mut reg = Registry::new();
        reg.update_heartbeat(sample_identity("node1"));
        if let Some(node) = reg.nodes.get_mut("node1") {
            node.last_heartbeat = SystemTime::now() - Duration::from_secs(9999);
        }
        reg.prune_stale(Duration::from_secs(10));
        assert_eq!(reg.count(), 0);
    }

    #[test]
    fn test_list_nodes() {
        let mut reg = Registry::new();
        reg.update_heartbeat(sample_identity("node1"));
        reg.update_heartbeat(sample_identity("node2"));
        let nodes = reg.list_nodes();
        assert_eq!(nodes.len(), 2);
        let ids: Vec<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
        assert!(ids.contains(&"node1"));
        assert!(ids.contains(&"node2"));
    }

    #[test]
    fn test_get_node_full() {
        let mut reg = Registry::new();
        reg.update_heartbeat(sample_identity("node1"));
        reg.update_hardware("node1", sample_hardware());
        reg.update_capabilities("node1", sample_capabilities());
        let full = reg.get_node_full("node1").unwrap();
        assert_eq!(full.id, "node1");
        assert_eq!(full.hostname, "test-host");
        assert!(full.hardware.is_some());
        assert!(full.capabilities.is_some());
    }

    #[test]
    fn test_get_node_full_missing() {
        let reg = Registry::new();
        assert!(reg.get_node_full("nonexistent").is_none());
    }

    fn make_identity(id: &str) -> NodeIdentity {
        NodeIdentity {
            id: id.into(),
            hostname: "test-node".into(),
            ip: "10.0.0.1".into(),
            role: NodeRole::Compute,
        }
    }

    #[test]
    fn eligible_compute_nodes_filters_by_role() {
        let mut registry = Registry::new();
        let controller = NodeIdentity {
            id: "controller".into(),
            hostname: "controller-host".into(),
            ip: "127.0.0.1".into(),
            role: NodeRole::Controller,
        };
        let compute = NodeIdentity {
            id: "compute".into(),
            hostname: "compute-host".into(),
            ip: "127.0.0.1".into(),
            role: NodeRole::Compute,
        };
        registry.update_heartbeat(controller.clone());
        registry.update_heartbeat(compute.clone());
        let eligible = registry.eligible_compute_nodes();
        assert_eq!(eligible.len(), 1);
        assert_eq!(eligible[0].id, compute.id);
    }

    fn make_hardware() -> HardwareSpec {
        HardwareSpec {
            cpu_model: "AMD Ryzen AI 5 340 w/ Radeon 840M".into(),
            cpu_cores: 12,
            cpu_threads: 12,
            ram_gb: 7.39,
            os: "linux".into(),
            arch: "x86_64".into(),
            gpu: None,
        }
    }

    fn make_caps() -> NodeCapabilities {
        NodeCapabilities {
            cpu_inference: true,
            gpu_inference: false,
            ane_inference: false,
            max_model_size_gb: 3.69,
            features: vec!["llm".into()],
        }
    }

    #[test]
    fn list_nodes_returns_lite_records() {
        let mut reg = Registry::new();
        reg.update_heartbeat(make_identity("node-1"));
        let nodes = reg.list_nodes();
        assert_eq!(nodes.len(), 1);
        let n = &nodes[0];
        assert_eq!(n.id, "node-1");
        assert_eq!(n.hostname, "test-node");
        assert_eq!(n.ip, "10.0.0.1");
        assert!(n.last_heartbeat_ms < u128::MAX);
    }

    #[test]
    fn get_node_full_includes_hw_and_caps() {
        let mut reg = Registry::new();
        reg.update_heartbeat(make_identity("node-1"));
        let hw = make_hardware();
        reg.update_hardware("node-1", hw.clone());
        let caps = make_caps();
        reg.update_capabilities("node-1", caps.clone());
        let full = reg.get_node_full("node-1").expect("node should exist");
        assert_eq!(full.id, "node-1");
        let fhw = full.hardware.expect("hardware should be present");
        assert_eq!(fhw.cpu_model, hw.cpu_model);
        let fcaps = full.capabilities.expect("caps should be present");
        assert_eq!(fcaps.cpu_inference, caps.cpu_inference);
    }

    #[test]
    fn get_node_full_missing_returns_none() {
        let reg = Registry::new();
        assert!(reg.get_node_full("does-not-exist").is_none());
    }

    #[test]
    fn update_model_status_tracks_allocations() {
        let mut reg = Registry::new();
        reg.update_heartbeat(make_identity("node-1"));
        reg.update_model_status("node-1", "qwen2.5-7b", 4200, ModelLifecycleState::Loading);
        let alloc = reg
            .nodes
            .get("node-1")
            .and_then(|n| n.models.get("qwen2.5-7b"))
            .expect("model allocation should exist");
        assert_eq!(alloc.size_mb, 4200);
        assert_eq!(alloc.state, ModelLifecycleState::Loading);
    }

    #[test]
    fn persistence_survives_restart() {
        let path = "/tmp/ai_mesh_registry_persistence_test.db";
        let _ = std::fs::remove_file(path);

        {
            let mut reg = Registry::open(path).expect("open db");
            reg.update_heartbeat(NodeIdentity {
                id: "persist-node".into(),
                hostname: "persist-host".into(),
                ip: "10.0.0.18".into(),
                role: NodeRole::Compute,
            });
            reg.update_model_status(
                "persist-node",
                "qwen2.5:0.5b",
                500,
                ModelLifecycleState::Ready,
            );
        } // Registry dropped — SQLite connection closed

        let reg2 = Registry::open(path).expect("reopen db");
        let node = reg2
            .get("persist-node")
            .expect("node should survive coordinator restart");
        assert_eq!(node.identity.hostname, "persist-host");
        let alloc = node
            .models
            .get("qwen2.5:0.5b")
            .expect("model allocation should survive coordinator restart");
        assert_eq!(alloc.state, ModelLifecycleState::Ready);
        assert_eq!(alloc.size_mb, 500);

        let _ = std::fs::remove_file(path);
    }

    // Regression test for the rotate-token bug: after a coordinator restart the
    // SQLite-persisted Ready state makes select_node_for_inference return a node
    // whose llama-server is not actually running.  clear_all() (called via
    // reset-registry after Phase 3) must erase that stale state so that
    // wait-ready blocks until the agent reports Ready for real.
    #[test]
    fn clear_all_removes_stale_ready_state_after_coordinator_restart() {
        use crate::scheduler::Scheduler;

        let path = "/tmp/ai_mesh_stale_ready_regression_test.db";
        let _ = std::fs::remove_file(path);

        // Simulate pre-rotation run: node connects and loads a model.
        {
            let mut reg = Registry::open(path).expect("open db");
            reg.update_heartbeat(NodeIdentity {
                id: "beelink1".into(),
                hostname: "BEELINK1".into(),
                ip: "192.168.1.14".into(),
                role: NodeRole::Compute,
            });
            reg.update_capabilities(
                "beelink1",
                NodeCapabilities {
                    features: vec!["llm".into()],
                    max_model_size_gb: 8.0,
                    ..NodeCapabilities::default()
                },
            );
            reg.update_model_status("beelink1", "qwen2.5:7b", 4096, ModelLifecycleState::Ready);
        }

        // Coordinator restarts (rotate-token Phase 3): SQLite reload brings
        // stale Ready back into memory — this is the bug without the fix.
        let mut reg = Registry::open(path).expect("reopen after simulated restart");
        let selected = Scheduler::new(&reg).select_node_for_inference("qwen2.5:7b");
        assert!(
            selected.is_some(),
            "stale Ready from SQLite must be visible (documents the pre-fix behaviour)"
        );

        // reset-registry (the fix) clears the stale state.
        reg.clear_all();
        let selected_after = Scheduler::new(&reg).select_node_for_inference("qwen2.5:7b");
        assert!(
            selected_after.is_none(),
            "after clear_all, no node should be returned for inference"
        );

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn nodes_with_feature_finds_lighting_nodes() {
        let mut reg = Registry::new();
        reg.update_heartbeat(make_identity("llm-only"));
        reg.update_capabilities(
            "llm-only",
            NodeCapabilities {
                features: vec!["llm".into()],
                ..NodeCapabilities::default()
            },
        );
        reg.update_heartbeat(make_identity("llm-and-lighting"));
        reg.update_capabilities(
            "llm-and-lighting",
            NodeCapabilities {
                features: vec!["llm".into(), "lighting".into()],
                ..NodeCapabilities::default()
            },
        );

        let lighting_nodes = reg.nodes_with_feature("lighting");
        assert_eq!(lighting_nodes.len(), 1);
        assert_eq!(lighting_nodes[0].id, "llm-and-lighting");
    }

    #[test]
    fn any_ready_llm_model_returns_model_name() {
        let mut reg = Registry::new();
        reg.update_heartbeat(make_identity("node-1"));
        reg.update_capabilities(
            "node-1",
            NodeCapabilities {
                features: vec!["llm".into()],
                ..NodeCapabilities::default()
            },
        );
        reg.update_model_status("node-1", "qwen2.5:7b", 4096, ModelLifecycleState::Ready);

        assert_eq!(reg.any_ready_llm_model(), Some("qwen2.5:7b".into()));
    }

    #[test]
    fn any_ready_llm_model_returns_none_when_loading() {
        let mut reg = Registry::new();
        reg.update_heartbeat(make_identity("node-1"));
        reg.update_capabilities(
            "node-1",
            NodeCapabilities {
                features: vec!["llm".into()],
                ..NodeCapabilities::default()
            },
        );
        reg.update_model_status("node-1", "qwen2.5:7b", 4096, ModelLifecycleState::Loading);

        assert_eq!(reg.any_ready_llm_model(), None);
    }

    #[test]
    fn any_ready_llm_model_prefers_largest() {
        let mut reg = Registry::new();
        for (id, model, mb) in [
            ("pi1", "qwen2.5:1.5b", 1024u64),
            ("beelink1", "qwen2.5:7b", 4096u64),
        ] {
            reg.update_heartbeat(make_identity(id));
            reg.update_capabilities(
                id,
                NodeCapabilities {
                    features: vec!["llm".into()],
                    ..NodeCapabilities::default()
                },
            );
            reg.update_model_status(id, model, mb, ModelLifecycleState::Ready);
        }
        assert_eq!(reg.any_ready_llm_model(), Some("qwen2.5:7b".into()));
    }

    #[test]
    fn update_light_devices_stores_and_retrieves() {
        let mut reg = Registry::new();
        reg.update_light_devices("pi1", vec!["test_bulb".into()], vec!["all".into()]);
        let (devices, groups) = reg.all_light_device_names();
        assert!(devices.contains(&"test_bulb".to_string()));
        assert!(groups.contains(&"all".to_string()));
    }

    #[test]
    fn light_devices_persist_across_open() {
        let path = "/tmp/test-light-devices-registry.db";
        let _ = std::fs::remove_file(path);

        {
            let mut reg = Registry::open(path).unwrap();
            reg.update_light_devices(
                "pi1",
                vec!["test_bulb".into(), "desk_lamp".into()],
                vec!["all".into()],
            );
        }

        let reg = Registry::open(path).unwrap();
        let (devices, groups) = reg.all_light_device_names();
        assert!(devices.contains(&"test_bulb".to_string()));
        assert!(devices.contains(&"desk_lamp".to_string()));
        assert!(groups.contains(&"all".to_string()));

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn update_light_devices_overwrites_previous() {
        let mut reg = Registry::new();
        reg.update_light_devices("pi1", vec!["old_bulb".into()], vec![]);
        reg.update_light_devices("pi1", vec!["new_bulb".into()], vec!["all".into()]);
        let (devices, groups) = reg.all_light_device_names();
        assert!(!devices.contains(&"old_bulb".to_string()));
        assert!(devices.contains(&"new_bulb".to_string()));
        assert!(groups.contains(&"all".to_string()));
    }

    #[test]
    fn all_light_device_names_deduplicates_across_nodes() {
        // Simulates a stale SQLite row from an old node_id alongside the current one.
        let mut reg = Registry::new();
        reg.update_light_devices("old-uuid", vec!["test_bulb".into()], vec!["all".into()]);
        reg.update_light_devices("new-uuid", vec!["test_bulb".into()], vec!["all".into()]);
        let (devices, groups) = reg.all_light_device_names();
        assert_eq!(devices.len(), 1);
        assert_eq!(groups.len(), 1);
    }

    // ── Rooms ─────────────────────────────────────────────────────────────────

    #[test]
    fn create_room_appears_in_list() {
        let mut reg = Registry::new();
        let r = reg.create_room("Living Room");
        assert!(!r.id.is_empty());
        assert_eq!(r.name, "Living Room");
        assert_eq!(r.position, 0);
        assert!(r.device_ids.is_empty());

        let rooms = reg.list_rooms();
        assert_eq!(rooms.len(), 1);
        assert_eq!(rooms[0].id, r.id);
        assert_eq!(rooms[0].name, "Living Room");
    }

    #[test]
    fn delete_room_removes_from_list() {
        let mut reg = Registry::new();
        let r = reg.create_room("Bedroom");
        reg.delete_room(&r.id);
        assert!(reg.list_rooms().is_empty());
    }

    #[test]
    fn delete_room_cascades_device_memberships() {
        let mut reg = Registry::new();
        let r = reg.create_room("Lounge");
        reg.add_device_to_room(&r.id, "test_bulb");
        assert_eq!(reg.get_room_for_device("test_bulb"), Some(r.id.clone()));

        reg.delete_room(&r.id);
        assert!(reg.get_room_for_device("test_bulb").is_none());
    }

    #[test]
    fn rename_room_updates_name() {
        let mut reg = Registry::new();
        let r = reg.create_room("Old Name");
        reg.rename_room(&r.id, "New Name");
        let rooms = reg.list_rooms();
        assert_eq!(rooms[0].name, "New Name");
    }

    #[test]
    fn add_device_to_room_stores_membership() {
        let mut reg = Registry::new();
        let r = reg.create_room("Kitchen");
        reg.add_device_to_room(&r.id, "ceiling_spot");
        assert_eq!(reg.get_room_for_device("ceiling_spot"), Some(r.id.clone()));
        let rooms = reg.list_rooms();
        assert!(rooms[0].device_ids.contains(&"ceiling_spot".to_string()));
    }

    #[test]
    fn add_device_moves_it_between_rooms() {
        let mut reg = Registry::new();
        let a = reg.create_room("Room A");
        let b = reg.create_room("Room B");
        reg.add_device_to_room(&a.id, "desk_lamp");
        assert_eq!(reg.get_room_for_device("desk_lamp"), Some(a.id.clone()));

        reg.add_device_to_room(&b.id, "desk_lamp");
        assert_eq!(reg.get_room_for_device("desk_lamp"), Some(b.id.clone()));
        // Must no longer be in room A
        let rooms = reg.list_rooms();
        let room_a = rooms.iter().find(|r| r.id == a.id).unwrap();
        assert!(!room_a.device_ids.contains(&"desk_lamp".to_string()));
    }

    #[test]
    fn remove_device_from_room_clears_membership() {
        let mut reg = Registry::new();
        let r = reg.create_room("Office");
        reg.add_device_to_room(&r.id, "floor_lamp");
        reg.remove_device_from_room(&r.id, "floor_lamp");
        assert!(reg.get_room_for_device("floor_lamp").is_none());
    }

    #[test]
    fn get_room_for_device_returns_none_for_unassigned() {
        let reg = Registry::new();
        assert!(reg.get_room_for_device("unassigned_bulb").is_none());
    }

    #[test]
    fn room_exists_returns_correct_values() {
        let mut reg = Registry::new();
        let r = reg.create_room("Test");
        assert!(reg.room_exists(&r.id));
        assert!(!reg.room_exists("nonexistent-id"));
    }

    #[test]
    fn rooms_persist_across_open() {
        let path = "/tmp/test-rooms-registry.db";
        let _ = std::fs::remove_file(path);

        let room_id;
        {
            let mut reg = Registry::open(path).unwrap();
            let r = reg.create_room("Hall");
            room_id = r.id.clone();
            reg.add_device_to_room(&r.id, "hall_bulb");
        }

        let reg = Registry::open(path).unwrap();
        let rooms = reg.list_rooms();
        assert_eq!(rooms.len(), 1);
        assert_eq!(rooms[0].name, "Hall");
        assert_eq!(rooms[0].id, room_id);
        assert!(rooms[0].device_ids.contains(&"hall_bulb".to_string()));

        let _ = std::fs::remove_file(path);
    }
}
