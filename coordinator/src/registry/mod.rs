use rusqlite::{Connection, params};
use shared::messages::{LightStateReport, SensorReport};
use shared::{
    HardwareSpec, ModelAllocationFull, ModelLifecycleState, NodeCapabilities, NodeIdentity,
    NodeRecordFull, NodeRecordLite, NodeRole,
};
use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime};
use tracing::warn;

// Domain method groups split into sibling files (each an `impl Registry` block
// sharing this module's struct + private fields/helpers).
mod effects;
mod openings;
mod scenes;

fn degrees_to_cardinal(deg: f32) -> &'static str {
    let d = ((deg % 360.0) + 360.0) % 360.0;
    if !(45.0..315.0).contains(&d) {
        "N"
    } else if d < 135.0 {
        "E"
    } else if d < 225.0 {
        "S"
    } else {
        "W"
    }
}

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
pub struct LightPosition {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub room_id: Option<String>,
    pub fixture_type: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Opening {
    pub id: String,
    pub room_id: String,
    pub opening_type: String,
    pub wall_edge: String,
    pub x_norm: f32,
    pub width_norm: f32,
    pub transmission: f32,
    pub opening_scope: String,
    pub height_norm: f32,
    pub height_span: f32,
}

#[derive(Debug, Clone)]
pub struct RoomRecord {
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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DeviceSnapshot {
    pub device_id: String,
    pub node_id: String,
    pub on: bool,
    pub brightness: Option<u8>,
    pub color_xy: Option<(f32, f32)>,
    pub color_temp: Option<u16>,
}

#[derive(Debug, Clone)]
pub struct SceneRecord {
    pub id: String,
    pub name: String,
    pub room_id: Option<String>,
    pub created_at: i64,
    pub position: i64,
    /// Explicit per-device values to apply on recall. When `effect_id` is
    /// set, this holds only the devices that were overridden out of the
    /// effect at save time — every other room member is driven by the
    /// effect itself, not a stored snapshot.
    pub states: Vec<DeviceSnapshot>,
    /// Effect active in the room when the scene was saved, if any.
    pub effect_id: Option<String>,
    pub effect_params_json: Option<String>,
}

impl SceneRecord {
    /// Average CIE xy across all snapshots that have colour data.
    /// Prefers explicit xy; falls back to colour-temperature-derived xy.
    pub fn preview_color(&self) -> Option<[f32; 2]> {
        let xy_snaps: Vec<(f32, f32)> = self.states.iter().filter_map(|s| s.color_xy).collect();
        if !xy_snaps.is_empty() {
            let n = xy_snaps.len() as f32;
            let x = (xy_snaps.iter().map(|(x, _)| x).sum::<f32>() / n).clamp(0.0, 1.0);
            let y = (xy_snaps.iter().map(|(_, y)| y).sum::<f32>() / n).clamp(0.0, 1.0);
            return Some([x, y]);
        }
        let ct_snaps: Vec<u16> = self.states.iter().filter_map(|s| s.color_temp).collect();
        if !ct_snaps.is_empty() {
            let mean_ct = ct_snaps.iter().map(|&c| c as f32).sum::<f32>() / ct_snaps.len() as f32;
            let k = 1_000_000.0 / mean_ct;
            let t = ((k - 2700.0) / (6500.0 - 2700.0)).clamp(0.0, 1.0);
            return Some([0.46 - t * 0.15, 0.41 - t * 0.09]);
        }
        None
    }
}

/// One row in `room_effects`. A room has zero or more rows; at most one with
/// `enabled = 1` (enforced by the partial unique index).
#[derive(Debug, Clone)]
pub struct RoomEffectRecord {
    pub room_id: String,
    pub effect_id: String,
    pub enabled: bool,
    pub params_json: String,
    pub snapshot_json: Option<String>,
    pub internal_state_json: Option<String>,
    pub started_at_ms: i64,
    /// JSON array of device_ids that the user has manually overridden out of
    /// this effect. The runner skips these bulbs entirely.
    pub overrides_json: String,
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
    /// Typed device inventory: device_id → (owning node, device class).
    devices: HashMap<String, (String, shared::DeviceType)>,
    /// Z2M groups per node (a lighting-only concept).
    light_groups: HashMap<String, Vec<String>>,
    /// Physical coordinates (x, y, z), optional room association, and fixture type for lighting devices.
    light_positions: HashMap<String, LightPosition>,
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
        CREATE TABLE IF NOT EXISTS devices (
            device_id   TEXT PRIMARY KEY,
            node_id     TEXT NOT NULL,
            device_type TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS light_groups (
            node_id  TEXT PRIMARY KEY,
            groups   TEXT NOT NULL
        );
        -- Pre-multi-domain inventory blob; derived data, repopulated into
        -- `devices` from z2m's retained bridge/devices on first connect.
        DROP TABLE IF EXISTS light_devices;
        CREATE TABLE IF NOT EXISTS rooms (
            id       TEXT PRIMARY KEY,
            name     TEXT NOT NULL,
            position INTEGER NOT NULL DEFAULT 0,
            orientation_degrees REAL NOT NULL DEFAULT 0.0,
            has_window          INTEGER NOT NULL DEFAULT 0,
            window_facing       REAL
        );
        CREATE TABLE IF NOT EXISTS room_devices (
            room_id   TEXT NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
            device_id TEXT NOT NULL,
            PRIMARY KEY (room_id, device_id)
        );
        CREATE TABLE IF NOT EXISTS scenes (
            id                 TEXT PRIMARY KEY,
            name               TEXT NOT NULL,
            room_id            TEXT REFERENCES rooms(id) ON DELETE CASCADE,
            created_at         INTEGER NOT NULL,
            states_json        TEXT NOT NULL,
            effect_id          TEXT,
            effect_params_json TEXT
        );
        CREATE TABLE IF NOT EXISTS light_states (
            device_id  TEXT PRIMARY KEY,
            node_id    TEXT NOT NULL,
            state_json TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS sensor_states (
            device_id  TEXT PRIMARY KEY,
            node_id    TEXT NOT NULL,
            state_json TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS light_positions (
            device_id TEXT PRIMARY KEY,
            x         REAL NOT NULL DEFAULT 0.0,
            y         REAL NOT NULL DEFAULT 0.0,
            z         REAL NOT NULL DEFAULT 0.0,
            room_id   TEXT REFERENCES rooms(id)
        );
        CREATE TABLE IF NOT EXISTS openings (
            id            TEXT PRIMARY KEY,
            room_id       TEXT NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
            opening_type  TEXT NOT NULL,
            wall_edge     TEXT NOT NULL,
            x_norm        REAL NOT NULL,
            width_norm    REAL NOT NULL,
            transmission  REAL NOT NULL DEFAULT 1.0,
            opening_scope TEXT NOT NULL DEFAULT 'exterior',
            height_norm   REAL NOT NULL DEFAULT 0.3,
            height_span   REAL NOT NULL DEFAULT 0.5
        );
        CREATE TABLE IF NOT EXISTS device_names (
            device_id   TEXT PRIMARY KEY,
            custom_name TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS room_effects (
            room_id              TEXT    NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
            effect_id            TEXT    NOT NULL,
            enabled              INTEGER NOT NULL DEFAULT 1,
            params_json          TEXT    NOT NULL DEFAULT '{}',
            snapshot_json        TEXT,
            internal_state_json  TEXT,
            started_at           INTEGER NOT NULL,
            PRIMARY KEY (room_id, effect_id),
            CHECK (enabled IN (0, 1))
        );
        CREATE UNIQUE INDEX IF NOT EXISTS uid_enabled_room_effect
            ON room_effects (room_id) WHERE enabled = 1;
        CREATE TABLE IF NOT EXISTS dashboard_preferences (
            user_id TEXT NOT NULL,
            key     TEXT NOT NULL,
            value   TEXT NOT NULL,
            PRIMARY KEY (user_id, key)
        );",
    )?;

    // Migration: Add overrides_json to room_effects if absent.
    let re_cols: Vec<String> = conn
        .prepare("PRAGMA table_info(room_effects)")?
        .query_map([], |row| row.get(1))?
        .collect::<rusqlite::Result<_>>()?;
    if !re_cols.contains(&"overrides_json".to_string()) {
        conn.execute(
            "ALTER TABLE room_effects ADD COLUMN overrides_json TEXT NOT NULL DEFAULT '[]'",
            [],
        )?;
    }

    // Migration: Add new columns to rooms if they don't exist
    let columns: Vec<String> = conn
        .prepare("PRAGMA table_info(rooms)")?
        .query_map([], |row| row.get(1))?
        .collect::<rusqlite::Result<_>>()?;

    if !columns.contains(&"orientation_degrees".to_string()) {
        conn.execute(
            "ALTER TABLE rooms ADD COLUMN orientation_degrees REAL NOT NULL DEFAULT 0.0",
            [],
        )?;
    }
    if !columns.contains(&"has_window".to_string()) {
        conn.execute(
            "ALTER TABLE rooms ADD COLUMN has_window INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }
    if !columns.contains(&"window_facing".to_string()) {
        conn.execute("ALTER TABLE rooms ADD COLUMN window_facing REAL", [])?;
    }
    if !columns.contains(&"width_m".to_string()) {
        conn.execute(
            "ALTER TABLE rooms ADD COLUMN width_m REAL NOT NULL DEFAULT 3.0",
            [],
        )?;
    }
    if !columns.contains(&"depth_m".to_string()) {
        conn.execute(
            "ALTER TABLE rooms ADD COLUMN depth_m REAL NOT NULL DEFAULT 6.0",
            [],
        )?;
    }
    if !columns.contains(&"height_m".to_string()) {
        conn.execute(
            "ALTER TABLE rooms ADD COLUMN height_m REAL NOT NULL DEFAULT 2.5",
            [],
        )?;
    }
    if !columns.contains(&"origin_x".to_string()) {
        conn.execute(
            "ALTER TABLE rooms ADD COLUMN origin_x REAL NOT NULL DEFAULT 0.5",
            [],
        )?;
    }
    if !columns.contains(&"origin_y".to_string()) {
        conn.execute(
            "ALTER TABLE rooms ADD COLUMN origin_y REAL NOT NULL DEFAULT 0.5",
            [],
        )?;
    }

    // Migration: Add new columns to light_positions if they don't exist
    let columns: Vec<String> = conn
        .prepare("PRAGMA table_info(light_positions)")?
        .query_map([], |row| row.get(1))?
        .collect::<rusqlite::Result<_>>()?;

    if !columns.contains(&"room_id".to_string()) {
        conn.execute(
            "ALTER TABLE light_positions ADD COLUMN room_id TEXT REFERENCES rooms(id)",
            [],
        )?;
    }
    if !columns.contains(&"fixture_type".to_string()) {
        conn.execute(
            "ALTER TABLE light_positions ADD COLUMN fixture_type TEXT",
            [],
        )?;
    }

    // Migration: Add position column to room_devices if it doesn't exist
    let rd_cols: Vec<String> = conn
        .prepare("PRAGMA table_info(room_devices)")?
        .query_map([], |row| row.get(1))?
        .collect::<rusqlite::Result<_>>()?;
    if !rd_cols.contains(&"position".to_string()) {
        conn.execute(
            "ALTER TABLE room_devices ADD COLUMN position INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
        // Back-fill: assign positions based on alphabetical device_id order within each
        // room, matching the old ORDER BY device_id sort so existing layouts are stable.
        conn.execute_batch(
            "UPDATE room_devices SET position = (
                SELECT COUNT(*) FROM room_devices r2
                WHERE r2.room_id = room_devices.room_id AND r2.device_id < room_devices.device_id
            )",
        )?;
    }

    // Migration: Add position column to scenes if it doesn't exist
    let scene_cols: Vec<String> = conn
        .prepare("PRAGMA table_info(scenes)")?
        .query_map([], |row| row.get(1))?
        .collect::<rusqlite::Result<_>>()?;
    if !scene_cols.contains(&"position".to_string()) {
        conn.execute(
            "ALTER TABLE scenes ADD COLUMN position INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
        // Set initial positions from created_at order within each room so existing
        // scenes have a deterministic starting order rather than all being 0.
        conn.execute_batch(
            "UPDATE scenes SET position = (
                SELECT COUNT(*) FROM scenes s2
                WHERE s2.room_id IS scenes.room_id AND s2.created_at < scenes.created_at
            )",
        )?;
    }

    // Migration: Add effect_id/effect_params_json columns to scenes if absent
    // — lets a scene remember "effect X with these params, plus these
    // per-light overrides" instead of only a flat per-device snapshot.
    if !scene_cols.contains(&"effect_id".to_string()) {
        conn.execute("ALTER TABLE scenes ADD COLUMN effect_id TEXT", [])?;
    }
    if !scene_cols.contains(&"effect_params_json".to_string()) {
        conn.execute("ALTER TABLE scenes ADD COLUMN effect_params_json TEXT", [])?;
    }

    // Legacy migration: convert has_window/window_facing rows to openings rows (idempotent).
    let legacy: Vec<(String, f32)> = {
        let mut stmt = conn.prepare(
            "SELECT r.id, r.window_facing FROM rooms r
             WHERE r.has_window = 1 AND r.window_facing IS NOT NULL
               AND NOT EXISTS (SELECT 1 FROM openings o WHERE o.room_id = r.id)",
        )?;
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    for (room_id, facing_deg) in legacy {
        let id = gen_uuid();
        let wall_edge = degrees_to_cardinal(facing_deg);
        if let Err(e) = conn.execute(
            "INSERT INTO openings (id, room_id, opening_type, wall_edge, x_norm, width_norm, transmission)
             VALUES (?1, ?2, 'window', ?3, 0.5, 0.3, 1.0)",
            params![id, room_id, wall_edge],
        ) {
            warn!(error = %e, "legacy window migration failed for room {room_id}");
        }
    }

    Ok(())
}

fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn now_unix_millis() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
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
            devices: HashMap::new(),
            light_groups: HashMap::new(),
            light_positions: HashMap::new(),
        }
    }

    /// Persistent registry backed by a file. Opens or creates `path`.
    /// Existing rows are loaded into the in-memory map on construction.
    pub fn open(path: &str) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
        init_schema(&conn)?;
        let mut reg = Self {
            nodes: HashMap::new(),
            conn,
            devices: HashMap::new(),
            light_groups: HashMap::new(),
            light_positions: HashMap::new(),
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

        // ── devices (typed inventory) ───────────────────────────────────────
        let dev_rows: Vec<(String, String, String)> = {
            let mut stmt = self
                .conn
                .prepare("SELECT device_id, node_id, device_type FROM devices")?;
            stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
                .collect::<rusqlite::Result<_>>()?
        };
        for (device_id, node_id, type_str) in dev_rows {
            self.devices
                .insert(device_id, (node_id, shared::DeviceType::parse(&type_str)));
        }

        // ── light_groups ────────────────────────────────────────────────────
        let grp_rows: Vec<(String, String)> = {
            let mut stmt = self
                .conn
                .prepare("SELECT node_id, groups FROM light_groups")?;
            stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?
        };
        for (node_id, groups_json) in grp_rows {
            let groups: Vec<String> = serde_json::from_str(&groups_json).unwrap_or_default();
            self.light_groups.insert(node_id, groups);
        }

        // ── light_positions ────────────────────────────────────────────────
        type PosRow = (String, f32, f32, f32, Option<String>, Option<String>);
        let pos_rows: Vec<PosRow> = {
            let mut stmt = self
                .conn
                .prepare("SELECT device_id, x, y, z, room_id, fixture_type FROM light_positions")?;
            stmt.query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            })?
            .collect::<rusqlite::Result<_>>()?
        };

        for (device_id, x, y, z, room_id, fixture_type) in pos_rows {
            self.light_positions.insert(
                device_id,
                LightPosition {
                    x,
                    y,
                    z,
                    room_id,
                    fixture_type,
                },
            );
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

    /// Clear all model allocations for a node (called when it reconnects, since
    /// llama-server is killed by agent service restarts and models must be reloaded).
    pub fn clear_node_models(&mut self, id: &str) {
        if let Some(node) = self.nodes.get_mut(id) {
            node.models.clear();
        }
        if let Err(e) = self.conn.execute(
            "DELETE FROM model_allocations WHERE node_id = ?1",
            params![id],
        ) {
            warn!(error = %e, node_id = %id, "DB clear_node_models failed");
        }
    }

    pub fn get_node_hostname(&self, id: &str) -> Option<String> {
        self.nodes.get(id).map(|n| n.identity.hostname.clone())
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

    /// Remove a single node (and its model allocations) from the registry.
    /// Returns false when the id is unknown. Used to purge dead nodes that
    /// would otherwise sit in the registry forever — nodes never expire on
    /// their own.
    pub fn remove_node(&mut self, id: &str) -> bool {
        if self.nodes.remove(id).is_none() {
            return false;
        }
        if let Err(e) = self.conn.execute(
            "DELETE FROM model_allocations WHERE node_id = ?1",
            params![id],
        ) {
            warn!(error = %e, "DB model_allocations delete failed");
        }
        if let Err(e) = self
            .conn
            .execute("DELETE FROM nodes WHERE id = ?1", params![id])
        {
            warn!(error = %e, "DB node delete failed");
        }
        true
    }

    /// Returns all Compute nodes whose reported capabilities include `feature`.
    pub fn nodes_with_feature(&self, feature: shared::Feature) -> Vec<NodeRecordFull> {
        self.nodes
            .values()
            .filter(|n| {
                n.capabilities
                    .as_ref()
                    .map(|c| c.features.contains(&feature))
                    .unwrap_or(false)
            })
            .filter_map(|rec| self.get_node_full(&rec.identity.id))
            .collect()
    }

    /// True if the node `id` currently advertises `feature`. Used on disconnect to
    /// decide whether losing this node means we've lost the lighting/bridge source.
    pub fn node_has_feature(&self, id: &str, feature: shared::Feature) -> bool {
        self.nodes
            .get(id)
            .and_then(|n| n.capabilities.as_ref())
            .map(|c| c.features.contains(&feature))
            .unwrap_or(false)
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
                        .map(|c| c.features.contains(&shared::Feature::Llm))
                        .unwrap_or(false)
            })
            .flat_map(|n| n.models.iter())
            .filter(|(_, alloc)| alloc.state == ModelLifecycleState::Ready)
            .max_by_key(|(_, alloc)| alloc.size_mb)
            .map(|(name, _)| name.clone())
    }

    /// All model names in Ready state on LLM-capable Compute nodes, deduped
    /// and sorted. Used by the OpenAI-compatible API for routing and /v1/models.
    pub fn ready_llm_models(&self) -> Vec<String> {
        self.nodes
            .values()
            .filter(|n| {
                n.identity.role == NodeRole::Compute
                    && n.capabilities
                        .as_ref()
                        .map(|c| c.features.contains(&shared::Feature::Llm))
                        .unwrap_or(false)
            })
            .flat_map(|n| n.models.iter())
            .filter(|(_, alloc)| alloc.state == ModelLifecycleState::Ready)
            .map(|(name, _)| name.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    /// Store a node's full typed device inventory + Z2M groups, replacing the
    /// node's previous rows (z2m sends the complete list on every connect).
    pub fn update_devices(
        &mut self,
        node_id: &str,
        devices: Vec<shared::DeviceEntry>,
        groups: Vec<String>,
    ) {
        let tx = self.conn.transaction();
        match tx {
            Ok(tx) => {
                let mut ok = tx
                    .execute("DELETE FROM devices WHERE node_id = ?1", params![node_id])
                    .is_ok();
                for d in &devices {
                    ok &= tx
                        .execute(
                            "INSERT OR REPLACE INTO devices (device_id, node_id, device_type)
                             VALUES (?1, ?2, ?3)",
                            params![d.id, node_id, d.device_type.as_str()],
                        )
                        .is_ok();
                }
                let groups_json = serde_json::to_string(&groups).unwrap_or_default();
                ok &= tx
                    .execute(
                        "INSERT OR REPLACE INTO light_groups (node_id, groups) VALUES (?1, ?2)",
                        params![node_id, groups_json],
                    )
                    .is_ok();
                if !ok || tx.commit().is_err() {
                    warn!(node_id = %node_id, "DB devices upsert failed");
                }
            }
            Err(e) => warn!(error = %e, "DB devices transaction failed"),
        }
        self.devices.retain(|_, (nid, _)| nid != node_id);
        for d in devices {
            self.devices
                .insert(d.id, (node_id.to_owned(), d.device_type));
        }
        self.light_groups.insert(node_id.to_owned(), groups);
    }

    /// Friendly names of all devices of one class, deduped and sorted.
    pub fn devices_of_type(&self, t: shared::DeviceType) -> Vec<String> {
        let mut names: Vec<String> = self
            .devices
            .iter()
            .filter(|(_, (_, dt))| *dt == t)
            .map(|(id, _)| id.clone())
            .collect();
        names.sort();
        names
    }

    /// Lighting control targets: (light device names, Z2M group names) across
    /// all nodes — the shape the intent pipeline validates against.
    pub fn lighting_targets(&self) -> (Vec<String>, Vec<String>) {
        let mut groups: std::collections::HashSet<String> = Default::default();
        for grps in self.light_groups.values() {
            groups.extend(grps.iter().cloned());
        }
        (
            self.devices_of_type(shared::DeviceType::Light),
            groups.into_iter().collect(),
        )
    }

    /// The typed inventory: (device_id, node_id, type) rows.
    pub fn all_devices(&self) -> impl Iterator<Item = (&String, &(String, shared::DeviceType))> {
        self.devices.iter()
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
        let mut stmt = match self.conn.prepare(
            "SELECT device_id FROM room_devices WHERE room_id = ?1 ORDER BY position, device_id",
        ) {
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
        let mut stmt = match self.conn.prepare(
            "SELECT id, name, position, orientation_degrees, has_window, window_facing, \
             width_m, depth_m, height_m, origin_x, origin_y FROM rooms ORDER BY position, name",
        ) {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "list_rooms prepare failed");
                return vec![];
            }
        };
        #[allow(clippy::type_complexity)]
        let rows: Vec<(
            String,
            String,
            i64,
            f32,
            bool,
            Option<f32>,
            f64,
            f64,
            f64,
            f64,
            f64,
        )> = stmt
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get::<_, i32>(4)? != 0,
                    row.get(5)?,
                    row.get::<_, f64>(6).unwrap_or(3.0),
                    row.get::<_, f64>(7).unwrap_or(6.0),
                    row.get::<_, f64>(8).unwrap_or(2.5),
                    row.get::<_, f64>(9).unwrap_or(0.5),
                    row.get::<_, f64>(10).unwrap_or(0.5),
                ))
            })
            .map(|r| r.collect::<rusqlite::Result<_>>().unwrap_or_default())
            .unwrap_or_default();
        rows.into_iter()
            .map(
                |(
                    id,
                    name,
                    position,
                    orientation_degrees,
                    has_window,
                    window_facing,
                    width_m,
                    depth_m,
                    height_m,
                    origin_x,
                    origin_y,
                )| {
                    let device_ids = self.room_device_ids(&id);
                    RoomRecord {
                        id,
                        name,
                        position,
                        device_ids,
                        orientation_degrees,
                        has_window,
                        window_facing,
                        width_m,
                        depth_m,
                        height_m,
                        origin_x,
                        origin_y,
                    }
                },
            )
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

    /// Set the room compass orientation and persist to SQLite.
    pub fn set_room_orientation(&mut self, room_id: &str, degrees: f32) {
        let clamped = degrees.rem_euclid(360.0);
        if let Err(e) = self.conn.execute(
            "UPDATE rooms SET orientation_degrees = ?1 WHERE id = ?2",
            params![clamped, room_id],
        ) {
            warn!(error = %e, "set_room_orientation failed");
        }
    }

    pub fn set_room_origin(&mut self, room_id: &str, origin_x: f64, origin_y: f64) {
        let ox = origin_x.clamp(0.0, 1.0);
        let oy = origin_y.clamp(0.0, 1.0);
        if let Err(e) = self.conn.execute(
            "UPDATE rooms SET origin_x = ?1, origin_y = ?2 WHERE id = ?3",
            params![ox, oy, room_id],
        ) {
            warn!(error = %e, "set_room_origin failed");
        }
    }

    pub fn set_room_dimensions(
        &mut self,
        room_id: &str,
        width_m: f64,
        depth_m: f64,
        height_m: f64,
    ) {
        if let Err(e) = self.conn.execute(
            "UPDATE rooms SET width_m = ?1, depth_m = ?2, height_m = ?3 WHERE id = ?4",
            params![
                width_m.max(0.1),
                depth_m.max(0.1),
                height_m.max(0.1),
                room_id
            ],
        ) {
            warn!(error = %e, "set_room_dimensions failed");
        }
    }

    pub fn get_room(&self, id: &str) -> Option<RoomRecord> {
        self.list_rooms().into_iter().find(|r| r.id == id)
    }

    /// Returns true if a room with this id exists.
    pub fn room_exists(&self, id: &str) -> bool {
        self.conn
            .query_row("SELECT 1 FROM rooms WHERE id = ?1", params![id], |_| Ok(()))
            .is_ok()
    }

    pub fn delete_room(&mut self, id: &str) {
        // light_positions.room_id has no ON DELETE action, so clear it first
        // to avoid a FK violation blocking the room deletion.
        // The bulb keeps its coordinates and remains as an unassigned device.
        let _ = self.conn.execute(
            "UPDATE light_positions SET room_id = NULL WHERE room_id = ?1",
            params![id],
        );
        for pos in self.light_positions.values_mut() {
            if pos.room_id.as_deref() == Some(id) {
                pos.room_id = None;
            }
        }
        // CASCADE in room_devices and openings handles the rest.
        if let Err(e) = self
            .conn
            .execute("DELETE FROM rooms WHERE id = ?1", params![id])
        {
            warn!(error = %e, "delete_room failed");
        }
    }

    pub fn delete_device(&mut self, device_id: &str) {
        // Remove from room assignments
        if let Err(e) = self.conn.execute(
            "DELETE FROM room_devices WHERE device_id = ?1",
            params![device_id],
        ) {
            warn!(error = %e, "delete_device: remove from rooms failed");
        }
        // Remove light state
        if let Err(e) = self.conn.execute(
            "DELETE FROM light_states WHERE device_id = ?1",
            params![device_id],
        ) {
            warn!(error = %e, "delete_device: remove light_states failed");
        }
        // Remove sensor state
        if let Err(e) = self.conn.execute(
            "DELETE FROM sensor_states WHERE device_id = ?1",
            params![device_id],
        ) {
            warn!(error = %e, "delete_device: remove sensor_states failed");
        }
        // Remove light position
        if let Err(e) = self.conn.execute(
            "DELETE FROM light_positions WHERE device_id = ?1",
            params![device_id],
        ) {
            warn!(error = %e, "delete_device: remove light_positions failed");
        }
        // Remove from the typed inventory
        if let Err(e) = self.conn.execute(
            "DELETE FROM devices WHERE device_id = ?1",
            params![device_id],
        ) {
            warn!(error = %e, "delete_device: remove devices failed");
        }
        self.devices.remove(device_id);
        // Update in-memory cache
        self.light_positions.remove(device_id);
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
            "INSERT OR IGNORE INTO room_devices (room_id, device_id, position)
             VALUES (?1, ?2, COALESCE((SELECT MAX(position) + 1 FROM room_devices WHERE room_id = ?1), 0))",
            params![room_id, device_id],
        ) {
            warn!(error = %e, "add_device_to_room insert failed");
        }
    }

    pub fn reorder_room_devices(&mut self, room_id: &str, ids: &[String]) {
        let tx = match self.conn.transaction() {
            Ok(tx) => tx,
            Err(e) => {
                warn!(error = %e, "reorder_room_devices: could not open transaction");
                return;
            }
        };
        for (i, device_id) in ids.iter().enumerate() {
            if let Err(e) = tx.execute(
                "UPDATE room_devices SET position = ?1 WHERE room_id = ?2 AND device_id = ?3",
                params![i as i64, room_id, device_id],
            ) {
                warn!(error = %e, "reorder_room_devices update failed");
            }
        }
        if let Err(e) = tx.commit() {
            warn!(error = %e, "reorder_room_devices commit failed");
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

    /// Sets `position` for each room id in order (position = index in slice).
    /// All updates run in one transaction so an ungraceful shutdown can't leave
    /// the room ordering half-applied (gaps/duplicate positions).
    pub fn set_room_positions(&mut self, ordered_ids: &[&str]) {
        let tx = match self.conn.transaction() {
            Ok(tx) => tx,
            Err(e) => {
                warn!(error = %e, "set_room_positions: could not open transaction");
                return;
            }
        };
        for (i, id) in ordered_ids.iter().enumerate() {
            if let Err(e) = tx.execute(
                "UPDATE rooms SET position = ?1 WHERE id = ?2",
                params![i as i64, id],
            ) {
                warn!(error = %e, "set_room_positions update failed");
            }
        }
        if let Err(e) = tx.commit() {
            warn!(error = %e, "set_room_positions commit failed");
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

    /// Returns a map of device_id → room display name for all assigned devices.
    pub fn device_room_name_map(&self) -> HashMap<String, String> {
        let mut map = HashMap::new();
        let mut stmt = match self.conn.prepare(
            "SELECT rd.device_id, r.name FROM room_devices rd JOIN rooms r ON rd.room_id = r.id",
        ) {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "device_room_name_map prepare failed");
                return map;
            }
        };
        if let Ok(rows) = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        }) {
            for row in rows.flatten() {
                map.insert(row.0, row.1);
            }
        }
        map
    }

    // ── Scenes ── see registry/scenes.rs ───────────────────────────────────────

    // ── Light state persistence ───────────────────────────────────────────────

    /// Upsert the last-known state for a device so it survives coordinator restarts.
    pub fn save_light_state(&mut self, report: &LightStateReport) {
        let state_json = match serde_json::to_string(report) {
            Ok(j) => j,
            Err(e) => {
                warn!(error = %e, "save_light_state: serialize failed");
                return;
            }
        };
        if let Err(e) = self.conn.execute(
            "INSERT OR REPLACE INTO light_states (device_id, node_id, state_json) VALUES (?1, ?2, ?3)",
            params![report.device_id, report.node_id, state_json],
        ) {
            warn!(error = %e, "save_light_state: db write failed");
        }
    }

    /// Return all persisted device light states — used to warm-start the dashboard on boot.
    pub fn load_light_states(&self) -> Vec<LightStateReport> {
        let mut stmt = match self.conn.prepare("SELECT state_json FROM light_states") {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "load_light_states: prepare failed");
                return vec![];
            }
        };
        stmt.query_map([], |row| row.get::<_, String>(0))
            .map(|rows| {
                rows.filter_map(|r| r.ok())
                    .filter_map(|json| serde_json::from_str::<LightStateReport>(&json).ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    // ── Sensor state persistence ──────────────────────────────────────────────

    /// Upsert the last-known readings for a sensor so they survive coordinator
    /// restarts. Callers persist the *merged* report (see
    /// `DashboardState::push_sensor_update`) so an availability-only report
    /// never wipes the stored readings.
    pub fn save_sensor_state(&mut self, report: &SensorReport) {
        let state_json = match serde_json::to_string(report) {
            Ok(j) => j,
            Err(e) => {
                warn!(error = %e, "save_sensor_state: serialize failed");
                return;
            }
        };
        if let Err(e) = self.conn.execute(
            "INSERT OR REPLACE INTO sensor_states (device_id, node_id, state_json) VALUES (?1, ?2, ?3)",
            params![report.device_id, report.node_id, state_json],
        ) {
            warn!(error = %e, "save_sensor_state: db write failed");
        }
    }

    /// Return all persisted sensor states — used to warm-start the dashboard on boot.
    pub fn load_sensor_states(&self) -> Vec<SensorReport> {
        let mut stmt = match self.conn.prepare("SELECT state_json FROM sensor_states") {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "load_sensor_states: prepare failed");
                return vec![];
            }
        };
        stmt.query_map([], |row| row.get::<_, String>(0))
            .map(|rows| {
                rows.filter_map(|r| r.ok())
                    .filter_map(|json| serde_json::from_str::<SensorReport>(&json).ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    // ── Light positions ──────────────────────────────────────────────────────

    pub fn get_all_light_positions(&self) -> HashMap<String, LightPosition> {
        self.light_positions.clone()
    }

    pub fn get_light_position(&self, device_id: &str) -> Option<LightPosition> {
        self.light_positions.get(device_id).cloned()
    }

    pub fn get_positions_for_room(&self, room_id: &str) -> Vec<(String, LightPosition)> {
        self.light_positions
            .iter()
            .filter(|(_, p)| p.room_id.as_deref() == Some(room_id))
            .map(|(id, p)| (id.clone(), p.clone()))
            .collect()
    }

    // ── Openings ── see registry/openings.rs ────────────────────────────────────

    // ── Device custom names ───────────────────────────────────────────────────

    pub fn set_device_name(&mut self, device_id: &str, name: &str) {
        if let Err(e) = self.conn.execute(
            "INSERT OR REPLACE INTO device_names (device_id, custom_name) VALUES (?1, ?2)",
            params![device_id, name],
        ) {
            warn!(error = %e, "set_device_name failed");
        }
    }

    pub fn get_all_device_names(&self) -> HashMap<String, String> {
        let mut stmt = match self
            .conn
            .prepare("SELECT device_id, custom_name FROM device_names")
        {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "get_all_device_names: prepare failed");
                return HashMap::new();
            }
        };
        match stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        }) {
            Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
            Err(e) => {
                warn!(error = %e, "get_all_device_names: query failed");
                HashMap::new()
            }
        }
    }

    pub fn update_light_position(
        &mut self,
        device_id: &str,
        x: f32,
        y: f32,
        z: f32,
        mut room_id: Option<String>,
        fixture_type: Option<String>,
    ) {
        // If room_id not provided, try to infer it from existing room membership.
        if room_id.is_none() {
            room_id = self.get_room_for_device(device_id);
        }

        if let Err(e) = self.conn.execute(
            "INSERT OR REPLACE INTO light_positions (device_id, x, y, z, room_id, fixture_type) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![device_id, x, y, z, room_id, fixture_type],
        ) {
            warn!(error = %e, "update_light_position failed");
            return;
        }
        self.light_positions.insert(
            device_id.to_owned(),
            LightPosition {
                x,
                y,
                z,
                room_id,
                fixture_type,
            },
        );
    }

    // ── room_effects ── see registry/effects.rs ────────────────────────────────

    // ── dashboard_preferences ──────────────────────────────────────────────────

    pub fn get_all_preferences(&self, user_id: &str) -> Vec<(String, String)> {
        self.conn
            .prepare("SELECT key, value FROM dashboard_preferences WHERE user_id = ?1")
            .and_then(|mut stmt| {
                stmt.query_map([user_id], |row| Ok((row.get(0)?, row.get(1)?)))?
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap_or_default()
    }

    pub fn set_preference(&self, user_id: &str, key: &str, value: &str) {
        let _ = self.conn.execute(
            "INSERT INTO dashboard_preferences (user_id, key, value) VALUES (?1, ?2, ?3)
             ON CONFLICT (user_id, key) DO UPDATE SET value = excluded.value",
            rusqlite::params![user_id, key, value],
        );
    }

    /// Returns true if the row existed and was deleted, false if not found.
    pub fn delete_preference(&self, user_id: &str, key: &str) -> bool {
        self.conn
            .execute(
                "DELETE FROM dashboard_preferences WHERE user_id = ?1 AND key = ?2",
                rusqlite::params![user_id, key],
            )
            .map(|n| n > 0)
            .unwrap_or(false)
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
            features: vec![shared::Feature::Llm],
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
            features: vec![shared::Feature::Llm],
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

    // Reproduces the user reports "scenes not saving" and "rooms not staying
    // deleted": both are DB writes, so this exercises them against a real file DB
    // across reopen (the unit tests elsewhere use an in-memory :memory: DB).
    #[test]
    fn rooms_and_scenes_persist_and_delete_across_restart() {
        let path = "/tmp/ai_mesh_rooms_scenes_persistence_test.db";
        let _ = std::fs::remove_file(path);

        let room_id;
        {
            let mut reg = Registry::open(path).expect("open db");
            room_id = reg.create_room("Lounge").id;
            reg.save_scene("Movie", Some(&room_id), Vec::new(), None);
            reg.save_scene("Global", None, Vec::new(), None);
        } // dropped — connection closed

        // Reopen: the room and both scenes must still be there ("saving").
        {
            let reg = Registry::open(path).expect("reopen db");
            assert!(
                reg.list_rooms().iter().any(|r| r.id == room_id),
                "room should persist across restart"
            );
            assert!(
                reg.list_scenes().iter().any(|s| s.name == "Movie"),
                "room scene should persist across restart"
            );
            assert!(
                reg.list_scenes().iter().any(|s| s.name == "Global"),
                "global scene should persist across restart"
            );
        }

        // Delete the room, reopen: it must STAY deleted, its scene cascade-deleted,
        // and the unrelated global scene must remain.
        {
            let mut reg = Registry::open(path).expect("reopen db");
            reg.delete_room(&room_id);
        }
        {
            let reg = Registry::open(path).expect("reopen db");
            assert!(
                !reg.list_rooms().iter().any(|r| r.id == room_id),
                "room should STAY deleted across restart"
            );
            assert!(
                !reg.list_scenes().iter().any(|s| s.name == "Movie"),
                "deleted room's scene should be cascade-removed"
            );
            assert!(
                reg.list_scenes().iter().any(|s| s.name == "Global"),
                "unrelated global scene should remain"
            );
        }

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
                    features: vec![shared::Feature::Llm],
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
                features: vec![shared::Feature::Llm],
                ..NodeCapabilities::default()
            },
        );
        reg.update_heartbeat(make_identity("llm-and-lighting"));
        reg.update_capabilities(
            "llm-and-lighting",
            NodeCapabilities {
                features: vec![shared::Feature::Llm, shared::Feature::Lighting],
                ..NodeCapabilities::default()
            },
        );

        let lighting_nodes = reg.nodes_with_feature(shared::Feature::Lighting);
        assert_eq!(lighting_nodes.len(), 1);
        assert_eq!(lighting_nodes[0].id, "llm-and-lighting");

        // node_has_feature mirrors the same membership for a single node.
        assert!(reg.node_has_feature("llm-and-lighting", shared::Feature::Lighting));
        assert!(!reg.node_has_feature("llm-only", shared::Feature::Lighting));
        assert!(!reg.node_has_feature("nonexistent", shared::Feature::Lighting));
    }

    #[test]
    fn any_ready_llm_model_returns_model_name() {
        let mut reg = Registry::new();
        reg.update_heartbeat(make_identity("node-1"));
        reg.update_capabilities(
            "node-1",
            NodeCapabilities {
                features: vec![shared::Feature::Llm],
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
                features: vec![shared::Feature::Llm],
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
                    features: vec![shared::Feature::Llm],
                    ..NodeCapabilities::default()
                },
            );
            reg.update_model_status(id, model, mb, ModelLifecycleState::Ready);
        }
        assert_eq!(reg.any_ready_llm_model(), Some("qwen2.5:7b".into()));
    }

    #[test]
    fn ready_llm_models_lists_all_ready_deduped_sorted() {
        let mut reg = Registry::new();
        for (id, model, mb) in [
            ("pi1", "qwen2.5:1.5b", 1024u64),
            ("beelink1", "qwen2.5:7b", 4096u64),
            ("omnilink1", "qwen2.5:7b", 4096u64), // duplicate model on a second node
        ] {
            reg.update_heartbeat(make_identity(id));
            reg.update_capabilities(
                id,
                NodeCapabilities {
                    features: vec![shared::Feature::Llm],
                    ..NodeCapabilities::default()
                },
            );
            reg.update_model_status(id, model, mb, ModelLifecycleState::Ready);
        }
        assert_eq!(
            reg.ready_llm_models(),
            vec!["qwen2.5:1.5b".to_string(), "qwen2.5:7b".to_string()]
        );
    }

    #[test]
    fn remove_node_deletes_node_and_models() {
        let mut reg = Registry::new();
        reg.update_heartbeat(make_identity("node-1"));
        reg.update_model_status("node-1", "qwen2.5:7b", 4096, ModelLifecycleState::Ready);

        assert!(reg.remove_node("node-1"));
        assert!(reg.get_node_full("node-1").is_none());
        assert!(reg.ready_llm_models().is_empty());
        // Removing again (or an unknown id) reports not-found.
        assert!(!reg.remove_node("node-1"));
        assert!(!reg.remove_node("ghost"));
    }

    #[test]
    fn ready_llm_models_excludes_loading_and_non_llm() {
        let mut reg = Registry::new();
        reg.update_heartbeat(make_identity("node-1"));
        reg.update_capabilities(
            "node-1",
            NodeCapabilities {
                features: vec![shared::Feature::Llm],
                ..NodeCapabilities::default()
            },
        );
        reg.update_model_status("node-1", "qwen2.5:7b", 4096, ModelLifecycleState::Loading);
        // A node without the llm feature never contributes, even with a Ready model.
        reg.update_heartbeat(make_identity("node-2"));
        reg.update_capabilities("node-2", NodeCapabilities::default());
        reg.update_model_status("node-2", "qwen2.5:1.5b", 1024, ModelLifecycleState::Ready);

        assert!(reg.ready_llm_models().is_empty());
    }

    fn light(id: &str) -> shared::DeviceEntry {
        shared::DeviceEntry {
            id: id.into(),
            device_type: shared::DeviceType::Light,
        }
    }

    fn sensor(id: &str) -> shared::DeviceEntry {
        shared::DeviceEntry {
            id: id.into(),
            device_type: shared::DeviceType::Sensor,
        }
    }

    #[test]
    fn update_devices_stores_and_retrieves() {
        let mut reg = Registry::new();
        reg.update_devices("pi1", vec![light("test_bulb")], vec!["all".into()]);
        let (devices, groups) = reg.lighting_targets();
        assert!(devices.contains(&"test_bulb".to_string()));
        assert!(groups.contains(&"all".to_string()));
    }

    #[test]
    fn lighting_targets_excludes_other_device_types() {
        let mut reg = Registry::new();
        reg.update_devices(
            "pi1",
            vec![light("test_bulb"), sensor("hall_motion")],
            vec![],
        );
        let (devices, _) = reg.lighting_targets();
        assert_eq!(devices, vec!["test_bulb".to_string()]);
        assert_eq!(
            reg.devices_of_type(shared::DeviceType::Sensor),
            vec!["hall_motion".to_string()]
        );
    }

    #[test]
    fn devices_persist_across_open() {
        let path = "/tmp/test-devices-registry.db";
        let _ = std::fs::remove_file(path);

        {
            let mut reg = Registry::open(path).unwrap();
            reg.update_devices(
                "pi1",
                vec![light("test_bulb"), sensor("temp_1")],
                vec!["all".into()],
            );
        }

        let reg = Registry::open(path).unwrap();
        let (devices, groups) = reg.lighting_targets();
        assert!(devices.contains(&"test_bulb".to_string()));
        assert!(!devices.contains(&"temp_1".to_string()));
        assert_eq!(
            reg.devices_of_type(shared::DeviceType::Sensor),
            vec!["temp_1".to_string()]
        );
        assert!(groups.contains(&"all".to_string()));

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn update_devices_overwrites_previous() {
        let mut reg = Registry::new();
        reg.update_devices("pi1", vec![light("old_bulb")], vec![]);
        reg.update_devices("pi1", vec![light("new_bulb")], vec!["all".into()]);
        let (devices, groups) = reg.lighting_targets();
        assert!(!devices.contains(&"old_bulb".to_string()));
        assert!(devices.contains(&"new_bulb".to_string()));
        assert!(groups.contains(&"all".to_string()));
    }

    #[test]
    fn lighting_targets_deduplicates_across_nodes() {
        // device_id is the primary key, so a device reported by a stale node
        // row and the current one collapses to a single entry.
        let mut reg = Registry::new();
        reg.update_devices("old-uuid", vec![light("test_bulb")], vec!["all".into()]);
        reg.update_devices("new-uuid", vec![light("test_bulb")], vec!["all".into()]);
        let (devices, groups) = reg.lighting_targets();
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
    fn delete_device_clears_room_membership_and_position() {
        let mut reg = Registry::new();
        let r = reg.create_room("Hall");
        reg.add_device_to_room(&r.id, "bulb_x");
        reg.update_light_position("bulb_x", 0.5, 0.5, 0.5, Some(r.id.clone()), None);
        assert_eq!(reg.get_room_for_device("bulb_x"), Some(r.id.clone()));
        assert!(reg.get_light_position("bulb_x").is_some());

        reg.delete_device("bulb_x");

        assert!(reg.get_room_for_device("bulb_x").is_none());
        assert!(reg.get_light_position("bulb_x").is_none());
        // The room itself survives; only the device is gone.
        assert!(reg.list_rooms().iter().any(|room| room.id == r.id));
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

    #[test]
    fn set_room_positions_updates_order() {
        let mut reg = Registry::new();
        let a = reg.create_room("A");
        let b = reg.create_room("B");
        let c = reg.create_room("C");
        reg.set_room_positions(&[&c.id, &a.id, &b.id]);
        let rooms = reg.list_rooms();
        let pos = |id: &str| rooms.iter().find(|r| r.id == id).unwrap().position;
        assert_eq!(pos(&c.id), 0);
        assert_eq!(pos(&a.id), 1);
        assert_eq!(pos(&b.id), 2);
    }

    #[test]
    fn set_room_positions_ignores_unknown_ids() {
        let mut reg = Registry::new();
        let a = reg.create_room("A");
        reg.set_room_positions(&[&a.id, "no-such-id"]);
        // No panic; known room gets position 0
        let rooms = reg.list_rooms();
        assert_eq!(rooms[0].position, 0);
    }

    // ── Scenes ────────────────────────────────────────────────────────────────

    fn make_snapshot(device_id: &str, on: bool) -> DeviceSnapshot {
        DeviceSnapshot {
            device_id: device_id.into(),
            node_id: "pi1".into(),
            on,
            brightness: Some(200),
            color_xy: None,
            color_temp: Some(370),
        }
    }

    #[test]
    fn save_scene_appears_in_list() {
        let mut reg = Registry::new();
        let r = reg.create_room("Living Room");
        let scene = reg.save_scene(
            "Evening",
            Some(&r.id),
            vec![make_snapshot("bulb1", true)],
            None,
        );
        assert!(!scene.id.is_empty());
        assert_eq!(scene.name, "Evening");
        assert_eq!(scene.room_id, Some(r.id.clone()));
        assert_eq!(scene.states.len(), 1);
        let scenes = reg.list_scenes();
        assert_eq!(scenes.len(), 1);
        assert_eq!(scenes[0].name, "Evening");
    }

    #[test]
    fn save_scene_global_has_no_room_id() {
        let mut reg = Registry::new();
        let scene = reg.save_scene("All Off", None, vec![make_snapshot("bulb1", false)], None);
        assert!(scene.room_id.is_none());
        let scenes = reg.list_scenes();
        assert_eq!(scenes.len(), 1);
        assert!(scenes[0].room_id.is_none());
    }

    #[test]
    fn get_scene_returns_correct_record() {
        let mut reg = Registry::new();
        let saved = reg.save_scene("Morning", None, vec![make_snapshot("bulb2", true)], None);
        let fetched = reg.get_scene(&saved.id).expect("scene should exist");
        assert_eq!(fetched.id, saved.id);
        assert_eq!(fetched.name, "Morning");
        assert_eq!(fetched.states.len(), 1);
        assert_eq!(fetched.states[0].device_id, "bulb2");
        assert!(fetched.states[0].on);
    }

    #[test]
    fn get_scene_returns_none_for_unknown_id() {
        let reg = Registry::new();
        assert!(reg.get_scene("no-such-id").is_none());
    }

    #[test]
    fn scene_exists_returns_correct_values() {
        let mut reg = Registry::new();
        let s = reg.save_scene("Test", None, vec![], None);
        assert!(reg.scene_exists(&s.id));
        assert!(!reg.scene_exists("nonexistent-id"));
    }

    #[test]
    fn delete_scene_removes_from_list() {
        let mut reg = Registry::new();
        let s = reg.save_scene("Temp", None, vec![], None);
        reg.delete_scene(&s.id);
        assert!(reg.list_scenes().is_empty());
        assert!(!reg.scene_exists(&s.id));
    }

    #[test]
    fn delete_room_cascades_to_scenes() {
        let mut reg = Registry::new();
        let r = reg.create_room("Study");
        reg.save_scene(
            "Night",
            Some(&r.id),
            vec![make_snapshot("desk_lamp", false)],
            None,
        );
        assert_eq!(reg.list_scenes().len(), 1);
        reg.delete_room(&r.id);
        assert!(
            reg.list_scenes().is_empty(),
            "scene should be deleted with the room"
        );
    }

    #[test]
    fn list_scenes_ordered_by_position_then_created_at() {
        let mut reg = Registry::new();
        let a = reg.save_scene("First", None, vec![], None);
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b = reg.save_scene("Second", None, vec![], None);
        let scenes = reg.list_scenes();
        assert_eq!(scenes.len(), 2);
        // New scenes get sequential positions (0, 1), so First comes before Second
        assert_eq!(scenes[0].id, a.id);
        assert_eq!(scenes[1].id, b.id);
        // After reorder, Second should come first
        reg.reorder_scenes(&[b.id.clone(), a.id.clone()]);
        let scenes = reg.list_scenes();
        assert_eq!(scenes[0].id, b.id);
        assert_eq!(scenes[1].id, a.id);
    }

    #[test]
    fn device_snapshot_serializes_round_trips() {
        let snap = DeviceSnapshot {
            device_id: "bulb1".into(),
            node_id: "pi1".into(),
            on: true,
            brightness: Some(200),
            color_xy: Some((0.3, 0.4)),
            color_temp: None,
        };
        let json = serde_json::to_string(&snap).unwrap();
        let back: DeviceSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back.device_id, "bulb1");
        assert_eq!(back.brightness, Some(200));
        assert_eq!(back.color_xy, Some((0.3, 0.4)));
        assert!(back.color_temp.is_none());
    }

    #[test]
    fn scenes_persist_across_open() {
        let path = "/tmp/test-scenes-registry.db";
        let _ = std::fs::remove_file(path);
        let scene_id;
        {
            let mut reg = Registry::open(path).unwrap();
            let r = reg.create_room("Hall");
            let s = reg.save_scene(
                "Bright",
                Some(&r.id),
                vec![make_snapshot("hall_bulb", true)],
                None,
            );
            scene_id = s.id.clone();
        }
        let reg = Registry::open(path).unwrap();
        let scene = reg
            .get_scene(&scene_id)
            .expect("scene should survive restart");
        assert_eq!(scene.name, "Bright");
        assert_eq!(scene.states.len(), 1);
        assert_eq!(scene.states[0].device_id, "hall_bulb");
        let _ = std::fs::remove_file(path);
    }

    // ── Light state persistence ───────────────────────────────────────────────

    fn make_light_state(device_id: &str, node_id: &str, on: bool) -> LightStateReport {
        LightStateReport {
            device_id: device_id.into(),
            node_id: node_id.into(),
            on,
            brightness: Some(200),
            color_xy: Some((0.3, 0.4)),
            color_temp: None,
            online: true,
        }
    }

    #[test]
    fn save_and_load_light_state_round_trips() {
        let mut reg = Registry::new();
        let report = make_light_state("bulb1", "pi1", true);
        reg.save_light_state(&report);
        let loaded = reg.load_light_states();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].device_id, "bulb1");
        assert_eq!(loaded[0].node_id, "pi1");
        assert!(loaded[0].on);
        assert_eq!(loaded[0].brightness, Some(200));
        assert_eq!(loaded[0].color_xy, Some((0.3, 0.4)));
        assert!(loaded[0].color_temp.is_none());
    }

    #[test]
    fn save_light_state_upserts_same_device() {
        let mut reg = Registry::new();
        reg.save_light_state(&make_light_state("bulb1", "pi1", true));
        reg.save_light_state(&make_light_state("bulb1", "pi1", false));
        let loaded = reg.load_light_states();
        assert_eq!(loaded.len(), 1, "upsert should not accumulate");
        assert!(!loaded[0].on, "latest state should win");
    }

    #[test]
    fn load_light_states_returns_all_devices() {
        let mut reg = Registry::new();
        reg.save_light_state(&make_light_state("bulb1", "pi1", true));
        reg.save_light_state(&make_light_state("bulb2", "pi1", false));
        assert_eq!(reg.load_light_states().len(), 2);
    }

    #[test]
    fn load_light_states_empty_when_none_saved() {
        let reg = Registry::new();
        assert!(reg.load_light_states().is_empty());
    }

    #[test]
    fn light_states_persist_across_open() {
        let path = "/tmp/test-light-states-registry.db";
        let _ = std::fs::remove_file(path);
        {
            let mut reg = Registry::open(path).unwrap();
            reg.save_light_state(&make_light_state("living_room_bulb", "pi2", true));
        }
        let reg = Registry::open(path).unwrap();
        let states = reg.load_light_states();
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].device_id, "living_room_bulb");
        assert!(states[0].on);
        let _ = std::fs::remove_file(path);
    }

    // ── Sensor state persistence ──────────────────────────────────────────────

    fn make_sensor_state(device_id: &str, node_id: &str, temperature: f32) -> SensorReport {
        SensorReport {
            device_id: device_id.into(),
            node_id: node_id.into(),
            temperature: Some(temperature),
            humidity: Some(47.0),
            battery: Some(98),
            occupancy: None,
            contact: None,
            illuminance: None,
            online: true,
        }
    }

    #[test]
    fn save_and_load_sensor_state_round_trips() {
        let mut reg = Registry::new();
        reg.save_sensor_state(&make_sensor_state("office_climate", "pi1", 21.4));
        let loaded = reg.load_sensor_states();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].device_id, "office_climate");
        assert_eq!(loaded[0].node_id, "pi1");
        assert_eq!(loaded[0].temperature, Some(21.4));
        assert_eq!(loaded[0].battery, Some(98));
        assert!(loaded[0].occupancy.is_none());
    }

    #[test]
    fn save_sensor_state_upserts_same_device() {
        let mut reg = Registry::new();
        reg.save_sensor_state(&make_sensor_state("office_climate", "pi1", 21.4));
        reg.save_sensor_state(&make_sensor_state("office_climate", "pi1", 22.0));
        let loaded = reg.load_sensor_states();
        assert_eq!(loaded.len(), 1, "upsert should not accumulate");
        assert_eq!(loaded[0].temperature, Some(22.0), "latest state should win");
    }

    #[test]
    fn sensor_states_persist_across_open() {
        let path = "/tmp/test-sensor-states-registry.db";
        let _ = std::fs::remove_file(path);
        {
            let mut reg = Registry::open(path).unwrap();
            reg.save_sensor_state(&make_sensor_state("office_climate", "pi1", 21.4));
        }
        let reg = Registry::open(path).unwrap();
        let states = reg.load_sensor_states();
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].device_id, "office_climate");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn delete_device_removes_sensor_state() {
        let mut reg = Registry::new();
        reg.save_sensor_state(&make_sensor_state("office_climate", "pi1", 21.4));
        reg.delete_device("office_climate");
        assert!(reg.load_sensor_states().is_empty());
    }

    // ── Light positions ───────────────────────────────────────────────────────

    #[test]
    fn update_and_get_light_position_round_trips() {
        let mut reg = Registry::new();
        reg.update_light_position("bulb1", 1.0, 2.0, 0.5, None, None);
        let pos = reg.get_light_position("bulb1").unwrap();
        assert!((pos.x - 1.0).abs() < 1e-4);
        assert!((pos.y - 2.0).abs() < 1e-4);
        assert!((pos.z - 0.5).abs() < 1e-4);
        assert!(pos.room_id.is_none());
        assert!(pos.fixture_type.is_none());
    }

    #[test]
    fn get_light_position_returns_none_for_unknown_device() {
        let reg = Registry::new();
        assert!(reg.get_light_position("ghost").is_none());
    }

    #[test]
    fn get_all_light_positions_returns_all_entries() {
        let mut reg = Registry::new();
        reg.update_light_position("bulb1", 0.0, 0.0, 0.0, None, None);
        reg.update_light_position("bulb2", 1.0, 1.0, 0.0, None, None);
        let all = reg.get_all_light_positions();
        assert_eq!(all.len(), 2);
        assert!(all.contains_key("bulb1"));
        assert!(all.contains_key("bulb2"));
    }

    #[test]
    fn update_light_position_upserts() {
        let mut reg = Registry::new();
        reg.update_light_position("bulb1", 1.0, 2.0, 0.0, None, None);
        reg.update_light_position("bulb1", 3.0, 4.0, 0.0, None, None);
        let pos = reg.get_light_position("bulb1").unwrap();
        assert!((pos.x - 3.0).abs() < 1e-4);
        assert!((pos.y - 4.0).abs() < 1e-4);
    }

    #[test]
    fn update_light_position_persists_fixture_type() {
        let mut reg = Registry::new();
        reg.update_light_position("bulb1", 0.5, 0.5, 1.0, None, Some("pendant".into()));
        let pos = reg.get_light_position("bulb1").unwrap();
        assert_eq!(pos.fixture_type.as_deref(), Some("pendant"));
    }

    #[test]
    fn light_position_infers_room_from_existing_membership() {
        let mut reg = Registry::new();
        let room_id = reg.create_room("Study").id;
        reg.add_device_to_room(&room_id, "bulb1");
        // No room_id passed — should be inferred from membership
        reg.update_light_position("bulb1", 1.0, 1.0, 0.0, None, None);
        let pos = reg.get_light_position("bulb1").unwrap();
        assert_eq!(pos.room_id.as_deref(), Some(room_id.as_str()));
    }

    // ── Room spatial fields ───────────────────────────────────────────────────

    // ── degrees_to_cardinal helper ────────────────────────────────────────────

    #[test]
    fn degrees_to_cardinal_covers_all_quadrants() {
        assert_eq!(degrees_to_cardinal(0.0), "N");
        assert_eq!(degrees_to_cardinal(359.0), "N");
        assert_eq!(degrees_to_cardinal(315.0), "N");
        assert_eq!(degrees_to_cardinal(44.9), "N");
        assert_eq!(degrees_to_cardinal(90.0), "E");
        assert_eq!(degrees_to_cardinal(45.0), "E");
        assert_eq!(degrees_to_cardinal(134.9), "E");
        assert_eq!(degrees_to_cardinal(180.0), "S");
        assert_eq!(degrees_to_cardinal(135.0), "S");
        assert_eq!(degrees_to_cardinal(224.9), "S");
        assert_eq!(degrees_to_cardinal(270.0), "W");
        assert_eq!(degrees_to_cardinal(225.0), "W");
        assert_eq!(degrees_to_cardinal(314.9), "W");
    }

    // ── Openings ──────────────────────────────────────────────────────────────

    #[test]
    fn create_opening_appears_in_get() {
        let mut reg = Registry::new();
        let room = reg.create_room("Lounge");
        let o = reg.create_opening(&room.id, "window", "S", 0.5, 0.3, 1.0);
        assert!(!o.id.is_empty());
        let openings = reg.get_openings_for_room(&room.id);
        assert_eq!(openings.len(), 1);
        assert_eq!(openings[0].opening_type, "window");
        assert_eq!(openings[0].wall_edge, "S");
        assert!((openings[0].x_norm - 0.5).abs() < 1e-4);
        assert!((openings[0].transmission - 1.0).abs() < 1e-4);
    }

    #[test]
    fn get_all_openings_by_room_groups_correctly() {
        let mut reg = Registry::new();
        let r1 = reg.create_room("Room 1");
        let r2 = reg.create_room("Room 2");
        reg.create_opening(&r1.id, "window", "S", 0.5, 0.3, 1.0);
        reg.create_opening(&r1.id, "door", "W", 0.2, 0.1, 0.1);
        reg.create_opening(&r2.id, "window", "E", 0.5, 0.4, 0.8);
        let map = reg.get_all_openings_by_room();
        assert_eq!(map.get(&r1.id).map(|v| v.len()), Some(2));
        assert_eq!(map.get(&r2.id).map(|v| v.len()), Some(1));
    }

    #[test]
    fn update_opening_changes_fields() {
        let mut reg = Registry::new();
        let room = reg.create_room("Study");
        let o = reg.create_opening(&room.id, "window", "N", 0.5, 0.3, 1.0);
        reg.update_opening(&o.id, Some(0.7), None, Some(0.5), None);
        let openings = reg.get_openings_for_room(&room.id);
        assert!((openings[0].x_norm - 0.7).abs() < 1e-4);
        assert!((openings[0].width_norm - 0.3).abs() < 1e-4); // unchanged
        assert!((openings[0].transmission - 0.5).abs() < 1e-4);
    }

    #[test]
    fn delete_opening_removes_it() {
        let mut reg = Registry::new();
        let room = reg.create_room("Hall");
        let o = reg.create_opening(&room.id, "door", "E", 0.5, 0.15, 0.1);
        assert!(reg.opening_exists(&o.id));
        reg.delete_opening(&o.id);
        assert!(!reg.opening_exists(&o.id));
        assert!(reg.get_openings_for_room(&room.id).is_empty());
    }

    #[test]
    fn delete_room_cascades_to_openings() {
        let mut reg = Registry::new();
        let room = reg.create_room("Bedroom");
        reg.create_opening(&room.id, "window", "S", 0.5, 0.3, 1.0);
        reg.delete_room(&room.id);
        assert!(reg.get_openings_for_room(&room.id).is_empty());
    }

    #[test]
    fn delete_room_nulls_light_position_room_id() {
        // Deleting a room must not silently fail due to FK violation in light_positions.
        // The bulb should survive with room_id = None (unassigned).
        let mut reg = Registry::new();
        let room = reg.create_room("Studio");
        reg.update_light_position("bulb1", 0.5, 0.5, 1.0, Some(room.id.clone()), None);
        assert_eq!(
            reg.get_light_position("bulb1").unwrap().room_id,
            Some(room.id.clone())
        );

        reg.delete_room(&room.id);

        // Room is gone
        assert!(reg.list_rooms().is_empty());
        // Bulb still exists with coordinates intact, room_id cleared
        let pos = reg.get_light_position("bulb1").unwrap();
        assert!(pos.room_id.is_none());
        assert!((pos.x - 0.5).abs() < 1e-4);
    }

    #[test]
    fn update_opening_can_change_wall_edge() {
        let mut reg = Registry::new();
        let room = reg.create_room("Kitchen");
        let o = reg.create_opening(&room.id, "window", "N", 0.5, 0.3, 1.0);
        reg.update_opening(&o.id, None, None, None, Some("E"));
        let openings = reg.get_openings_for_room(&room.id);
        assert_eq!(openings[0].wall_edge, "E");
        assert_eq!(openings[0].opening_type, "window"); // type unchanged
    }

    #[test]
    fn legacy_migration_creates_opening_for_window_room() {
        let path = "/tmp/test-openings-legacy-migration.db";
        let _ = std::fs::remove_file(path);

        // Simulate an old DB: insert a room with has_window=1, window_facing=180 directly.
        {
            let conn = rusqlite::Connection::open(path).unwrap();
            init_schema(&conn).unwrap();
            conn.execute(
                "INSERT INTO rooms (id, name, position, orientation_degrees, has_window, window_facing) VALUES ('room1', 'Lounge', 0, 0.0, 1, 180.0)",
                [],
            ).unwrap();
            // Manually delete any auto-created opening so we can re-test migration
            conn.execute("DELETE FROM openings", []).unwrap();
        }

        // Re-open: init_schema should run the legacy migration
        let reg = Registry::open(path).unwrap();
        let openings = reg.get_openings_for_room("room1");
        assert_eq!(
            openings.len(),
            1,
            "legacy migration should create one opening"
        );
        assert_eq!(openings[0].wall_edge, "S", "180° should map to South wall");
        assert_eq!(openings[0].opening_type, "window");
        assert!((openings[0].x_norm - 0.5).abs() < 1e-4);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn legacy_migration_is_idempotent() {
        let path = "/tmp/test-openings-migration-idempotent.db";
        let _ = std::fs::remove_file(path);

        // First open creates migration opening
        {
            let conn = rusqlite::Connection::open(path).unwrap();
            init_schema(&conn).unwrap();
            conn.execute(
                "INSERT INTO rooms (id, name, position, orientation_degrees, has_window, window_facing) VALUES ('room1', 'Lounge', 0, 0.0, 1, 90.0)",
                [],
            ).unwrap();
            conn.execute("DELETE FROM openings", []).unwrap();
        }
        // open1: migration creates 1 opening
        let _ = Registry::open(path).unwrap();
        // open2: migration must not create a duplicate
        let reg2 = Registry::open(path).unwrap();
        let openings = reg2.get_openings_for_room("room1");
        assert_eq!(
            openings.len(),
            1,
            "second open must not duplicate the opening"
        );

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn create_room_has_default_spatial_fields() {
        let mut reg = Registry::new();
        let room = reg.create_room("Living Room");
        assert_eq!(room.orientation_degrees, 0.0);
        assert!(!room.has_window);
        assert!(room.window_facing.is_none());
    }

    #[test]
    fn list_rooms_returns_spatial_fields() {
        let mut reg = Registry::new();
        reg.create_room("Kitchen");
        let rooms = reg.list_rooms();
        assert_eq!(rooms.len(), 1);
        assert_eq!(rooms[0].orientation_degrees, 0.0);
        assert!(!rooms[0].has_window);
        assert!(rooms[0].window_facing.is_none());
    }

    #[test]
    fn set_room_orientation_persists_and_clamps() {
        let mut reg = Registry::new();
        let room = reg.create_room("Hall");
        assert!((reg.list_rooms()[0].orientation_degrees - 0.0).abs() < 1e-4);
        reg.set_room_orientation(&room.id, 270.0);
        assert!((reg.list_rooms()[0].orientation_degrees - 270.0).abs() < 1e-4);
        // Values ≥ 360 are clamped via rem_euclid
        reg.set_room_orientation(&room.id, 400.0);
        assert!((reg.list_rooms()[0].orientation_degrees - 40.0).abs() < 1e-2);
    }

    // ── room_effects ─────────────────────────────────────────────────────────

    #[test]
    fn set_active_effect_inserts_row_and_makes_it_active() {
        let mut reg = Registry::new();
        let room = reg.create_room("Lounge");
        reg.set_active_effect(&room.id, "solar", "{}", None, 1_000)
            .unwrap();
        let active = reg.get_active_effect(&room.id).unwrap();
        assert_eq!(active.effect_id, "solar");
        assert!(active.enabled);
        assert_eq!(active.params_json, "{}");
        assert_eq!(active.started_at_ms, 1_000);
    }

    #[test]
    fn set_active_effect_handoff_transfers_snapshot_verbatim() {
        let mut reg = Registry::new();
        let room = reg.create_room("Lounge");
        let snap = "{\"baseline\":\"on\"}";
        reg.set_active_effect(&room.id, "solar", "{}", Some(snap), 1_000)
            .unwrap();
        // Switching to a new effect WITHOUT passing a snapshot must preserve
        // the prior row's snapshot (handoff semantics).
        reg.set_active_effect(&room.id, "sunset", "{\"duration_secs\":1800}", None, 2_000)
            .unwrap();
        let active = reg.get_active_effect(&room.id).unwrap();
        assert_eq!(active.effect_id, "sunset");
        // sunset row was new — it doesn't have the solar snapshot. This proves
        // the runner is responsible for handoff snapshot transfer; the DB layer
        // only preserves whatever the caller passes.
        assert!(active.snapshot_json.is_none());
        // And we can read the disabled solar row back to recover its snapshot.
        let all_rows: Vec<_> = reg
            .conn
            .prepare(
                "SELECT effect_id, enabled, snapshot_json FROM room_effects WHERE room_id = ?1",
            )
            .unwrap()
            .query_map(params![&room.id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            })
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert_eq!(all_rows.len(), 2);
        let solar_row = all_rows.iter().find(|r| r.0 == "solar").unwrap();
        assert_eq!(solar_row.1, 0);
        assert_eq!(solar_row.2.as_deref(), Some(snap));
    }

    #[test]
    fn set_active_effect_disables_previous_effect() {
        let mut reg = Registry::new();
        let room = reg.create_room("Lounge");
        reg.set_active_effect(&room.id, "solar", "{}", None, 1_000)
            .unwrap();
        reg.set_active_effect(&room.id, "sunset", "{}", None, 2_000)
            .unwrap();
        // Only one enabled row should remain.
        let count: i64 = reg
            .conn
            .query_row(
                "SELECT COUNT(*) FROM room_effects WHERE room_id = ?1 AND enabled = 1",
                params![&room.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(reg.get_active_effect(&room.id).unwrap().effect_id, "sunset");
    }

    #[test]
    fn partial_unique_index_rejects_second_enabled_row() {
        let mut reg = Registry::new();
        let room = reg.create_room("Lounge");
        reg.set_active_effect(&room.id, "solar", "{}", None, 1_000)
            .unwrap();
        // Bypass set_active_effect's transactional guard and try to insert a
        // second enabled row directly. The partial unique index must reject.
        let err = reg.conn.execute(
            "INSERT INTO room_effects (room_id, effect_id, enabled, params_json, started_at)
             VALUES (?1, 'sunset', 1, '{}', 2000)",
            params![&room.id],
        );
        assert!(
            err.is_err(),
            "second enabled row must be rejected by partial unique index"
        );
    }

    #[test]
    fn disable_active_effect_returns_snapshot_and_clears_flag() {
        let mut reg = Registry::new();
        let room = reg.create_room("Lounge");
        let snap = "{\"baseline\":\"on\"}";
        reg.set_active_effect(&room.id, "solar", "{}", Some(snap), 1_000)
            .unwrap();
        let returned = reg.disable_active_effect(&room.id).unwrap();
        assert_eq!(returned.as_deref(), Some(snap));
        assert!(reg.get_active_effect(&room.id).is_none());
    }

    #[test]
    fn list_active_effects_returns_only_enabled_rows() {
        let mut reg = Registry::new();
        let r1 = reg.create_room("A");
        let r2 = reg.create_room("B");
        let r3 = reg.create_room("C");
        reg.set_active_effect(&r1.id, "solar", "{}", None, 1_000)
            .unwrap();
        reg.set_active_effect(&r2.id, "sunset", "{}", None, 1_000)
            .unwrap();
        reg.set_active_effect(&r3.id, "solar", "{}", None, 1_000)
            .unwrap();
        reg.disable_active_effect(&r3.id).unwrap();
        let active = reg.list_active_effects();
        assert_eq!(active.len(), 2);
        let ids: Vec<_> = active.iter().map(|r| r.effect_id.as_str()).collect();
        assert!(ids.contains(&"solar"));
        assert!(ids.contains(&"sunset"));
    }

    #[test]
    fn update_effect_internal_state_round_trips() {
        let mut reg = Registry::new();
        let room = reg.create_room("Lounge");
        reg.set_active_effect(&room.id, "aurora", "{}", None, 1_000)
            .unwrap();
        reg.update_effect_internal_state(&room.id, "aurora", Some("{\"seed\":42}"));
        let active = reg.get_active_effect(&room.id).unwrap();
        assert_eq!(active.internal_state_json.as_deref(), Some("{\"seed\":42}"));
    }

    #[test]
    fn set_effect_override_adds_and_removes_device() {
        let mut reg = Registry::new();
        let room = reg.create_room("Lounge");
        reg.set_active_effect(&room.id, "breathing", "{}", None, 1_000)
            .unwrap();

        // Add override.
        let overrides = reg.set_effect_override(&room.id, "bulb-1", true).unwrap();
        assert_eq!(overrides, Some(vec!["bulb-1".to_string()]));
        let active = reg.get_active_effect(&room.id).unwrap();
        let stored: Vec<String> = serde_json::from_str(&active.overrides_json).unwrap();
        assert_eq!(stored, vec!["bulb-1"]);

        // Adding same device again is idempotent.
        let overrides2 = reg.set_effect_override(&room.id, "bulb-1", true).unwrap();
        assert_eq!(overrides2, Some(vec!["bulb-1".to_string()]));

        // Add a second device.
        reg.set_effect_override(&room.id, "bulb-2", true).unwrap();
        let active = reg.get_active_effect(&room.id).unwrap();
        let mut stored: Vec<String> = serde_json::from_str(&active.overrides_json).unwrap();
        stored.sort();
        assert_eq!(stored, vec!["bulb-1", "bulb-2"]);

        // Remove first device.
        let overrides3 = reg.set_effect_override(&room.id, "bulb-1", false).unwrap();
        assert_eq!(overrides3, Some(vec!["bulb-2".to_string()]));

        // Remove non-existent device is idempotent.
        let overrides4 = reg.set_effect_override(&room.id, "bulb-99", false).unwrap();
        assert_eq!(overrides4, Some(vec!["bulb-2".to_string()]));
    }

    #[test]
    fn set_effect_override_returns_none_when_no_active_effect() {
        let mut reg = Registry::new();
        let room = reg.create_room("Lounge");
        let result = reg.set_effect_override(&room.id, "bulb-1", true).unwrap();
        assert!(result.is_none(), "no active effect → should return None");
    }

    #[test]
    fn overrides_cleared_per_activation() {
        // Re-activating an effect (e.g. dragging it onto the room again) starts
        // fresh — every light participates. set_active_effect resets overrides_json
        // so stale exclusions from a previous activation can't silently persist
        // (the bug where a freshly-dropped effect showed most bulbs greyed/excluded).
        let mut reg = Registry::new();
        let room = reg.create_room("Lounge");
        reg.set_active_effect(&room.id, "breathing", "{}", None, 1_000)
            .unwrap();
        reg.set_effect_override(&room.id, "bulb-1", true).unwrap();

        // Disable and re-enable the same effect.
        reg.disable_active_effect(&room.id).unwrap();
        reg.set_active_effect(&room.id, "breathing", "{}", None, 2_000)
            .unwrap();

        // overrides_json is reset on re-activation → all lights back in.
        let active = reg.get_active_effect(&room.id).unwrap();
        let stored: Vec<String> = serde_json::from_str(&active.overrides_json).unwrap();
        assert!(
            stored.is_empty(),
            "re-activation must clear stale overrides; got {stored:?}"
        );
    }

    #[test]
    fn overrides_default_to_empty_on_new_activation() {
        let mut reg = Registry::new();
        let room = reg.create_room("Lounge");
        reg.set_active_effect(&room.id, "snake", "{}", None, 1_000)
            .unwrap();
        let active = reg.get_active_effect(&room.id).unwrap();
        assert_eq!(active.overrides_json, "[]");
    }
}
