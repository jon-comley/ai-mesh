// Scene persistence: a named per-device snapshot, recallable per room.
use super::{DeviceSnapshot, Registry, SceneRecord, gen_uuid, now_unix_millis};
use rusqlite::params;
use tracing::warn;

impl Registry {
    /// `effect` is `(effect_id, params_json)` when the room had an active
    /// effect at save time — the caller (HTTP handler) is responsible for
    /// resolving that and for restricting `states` to just the overridden
    /// devices in that case (see `save_scene` in http/api/scenes.rs).
    pub fn save_scene(
        &mut self,
        name: &str,
        room_id: Option<&str>,
        states: Vec<DeviceSnapshot>,
        effect: Option<(&str, &str)>,
    ) -> SceneRecord {
        let id = gen_uuid();
        let created_at = now_unix_millis();
        let states_json = serde_json::to_string(&states).unwrap_or_else(|_| "[]".into());
        let (effect_id, effect_params_json) = match effect {
            Some((eid, params)) => (Some(eid.to_owned()), Some(params.to_owned())),
            None => (None, None),
        };
        // New scenes go to the end of the list for their room
        let position: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(MAX(position) + 1, 0) FROM scenes WHERE room_id IS ?1",
                params![room_id],
                |row| row.get(0),
            )
            .unwrap_or(0);
        if let Err(e) = self.conn.execute(
            "INSERT INTO scenes (id, name, room_id, created_at, states_json, position, effect_id, effect_params_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![id, name, room_id, created_at, states_json, position, effect_id, effect_params_json],
        ) {
            warn!(error = %e, "save_scene failed");
        }
        SceneRecord {
            id,
            name: name.to_owned(),
            room_id: room_id.map(|s| s.to_owned()),
            created_at,
            position,
            states,
            effect_id,
            effect_params_json,
        }
    }

    pub fn list_scenes(&self) -> Vec<SceneRecord> {
        let mut stmt = match self.conn.prepare(
            "SELECT id, name, room_id, created_at, states_json, position, effect_id, effect_params_json FROM scenes ORDER BY position ASC, created_at ASC",
        ) {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "list_scenes prepare failed");
                return vec![];
            }
        };
        type SceneRow = (
            String,
            String,
            Option<String>,
            i64,
            String,
            i64,
            Option<String>,
            Option<String>,
        );
        stmt.query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
            ))
        })
        .map(|rows| {
            rows.filter_map(|r| r.ok())
                .map(
                    |(
                        id,
                        name,
                        room_id,
                        created_at,
                        states_json,
                        position,
                        effect_id,
                        effect_params_json,
                    ): SceneRow| {
                        let states: Vec<DeviceSnapshot> =
                            serde_json::from_str(&states_json).unwrap_or_default();
                        SceneRecord {
                            id,
                            name,
                            room_id,
                            created_at,
                            position,
                            states,
                            effect_id,
                            effect_params_json,
                        }
                    },
                )
                .collect()
        })
        .unwrap_or_default()
    }

    pub fn get_scene(&self, id: &str) -> Option<SceneRecord> {
        self.conn
            .query_row(
                "SELECT id, name, room_id, created_at, states_json, position, effect_id, effect_params_json FROM scenes WHERE id = ?1",
                params![id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, Option<String>>(7)?,
                    ))
                },
            )
            .ok()
            .map(|(id, name, room_id, created_at, states_json, position, effect_id, effect_params_json)| {
                let states: Vec<DeviceSnapshot> =
                    serde_json::from_str(&states_json).unwrap_or_default();
                SceneRecord {
                    id,
                    name,
                    room_id,
                    created_at,
                    position,
                    states,
                    effect_id,
                    effect_params_json,
                }
            })
    }

    pub fn reorder_scenes(&mut self, ids: &[String]) {
        for (i, id) in ids.iter().enumerate() {
            let _ = self.conn.execute(
                "UPDATE scenes SET position = ?1 WHERE id = ?2",
                params![i as i64, id],
            );
        }
    }

    pub fn delete_scene(&mut self, id: &str) {
        if let Err(e) = self
            .conn
            .execute("DELETE FROM scenes WHERE id = ?1", params![id])
        {
            warn!(error = %e, "delete_scene failed");
        }
    }

    pub fn scene_exists(&self, id: &str) -> bool {
        self.conn
            .query_row(
                "SELECT 1 FROM scenes WHERE id = ?1",
                params![id],
                |_| Ok(()),
            )
            .is_ok()
    }
}
