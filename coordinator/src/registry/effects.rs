// Active per-room effect state (room_effects table) + per-device overrides.
use super::{Registry, RoomEffectRecord};
use rusqlite::{OptionalExtension, params};
use tracing::warn;

impl Registry {
    /// Set the active effect for a room. Disables any previously-enabled
    /// effect in the same room, then upserts the incoming effect row in the
    /// same transaction so the partial unique index never sees two enabled
    /// rows at once.
    ///
    /// `snapshot_json` is the pre-effect baseline captured by the runner at
    /// activation time, OR — for an effect→effect handoff — the previous
    /// effect's snapshot copied verbatim (preserving the original baseline).
    pub fn set_active_effect(
        &mut self,
        room_id: &str,
        effect_id: &str,
        params_json: &str,
        snapshot_json: Option<&str>,
        started_at_ms: i64,
    ) -> rusqlite::Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE room_effects SET enabled = 0 WHERE room_id = ?1 AND enabled = 1",
            params![room_id],
        )?;
        tx.execute(
            "INSERT INTO room_effects (room_id, effect_id, enabled, params_json, snapshot_json, started_at)
             VALUES (?1, ?2, 1, ?3, ?4, ?5)
             ON CONFLICT(room_id, effect_id) DO UPDATE SET
                 enabled        = 1,
                 params_json    = excluded.params_json,
                 snapshot_json  = COALESCE(excluded.snapshot_json, room_effects.snapshot_json),
                 started_at     = excluded.started_at,
                 overrides_json = '[]'",
            params![room_id, effect_id, params_json, snapshot_json, started_at_ms],
        )?;
        tx.commit()
    }

    /// Disable the currently-enabled effect for a room. Returns the snapshot
    /// the runner should restore (if any) so it can dispatch the revert
    /// commands.
    pub fn disable_active_effect(&mut self, room_id: &str) -> rusqlite::Result<Option<String>> {
        let tx = self.conn.transaction()?;
        let snapshot: Option<String> = tx
            .query_row(
                "SELECT snapshot_json FROM room_effects WHERE room_id = ?1 AND enabled = 1",
                params![room_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten();
        tx.execute(
            "UPDATE room_effects SET enabled = 0 WHERE room_id = ?1 AND enabled = 1",
            params![room_id],
        )?;
        tx.commit()?;
        Ok(snapshot)
    }

    /// Currently-enabled effect for a room (if any).
    pub fn get_active_effect(&self, room_id: &str) -> Option<RoomEffectRecord> {
        self.conn
            .query_row(
                "SELECT room_id, effect_id, enabled, params_json, snapshot_json, internal_state_json, started_at, overrides_json \
                 FROM room_effects WHERE room_id = ?1 AND enabled = 1",
                params![room_id],
                |row| {
                    let enabled: i64 = row.get(2)?;
                    Ok(RoomEffectRecord {
                        room_id: row.get::<_, String>(0)?,
                        effect_id: row.get::<_, String>(1)?,
                        enabled: enabled != 0,
                        params_json: row.get::<_, String>(3)?,
                        snapshot_json: row.get::<_, Option<String>>(4)?,
                        internal_state_json: row.get::<_, Option<String>>(5)?,
                        started_at_ms: row.get::<_, i64>(6)?,
                        overrides_json: row.get::<_, Option<String>>(7)?.unwrap_or_else(|| "[]".into()),
                    })
                },
            )
            .optional()
            .unwrap_or(None)
    }

    /// All enabled effect rows (across all rooms) — used by the runner on
    /// coordinator start to rehydrate live effects.
    pub fn list_active_effects(&self) -> Vec<RoomEffectRecord> {
        let Ok(mut stmt) = self.conn.prepare(
            "SELECT room_id, effect_id, enabled, params_json, snapshot_json, internal_state_json, started_at, overrides_json \
             FROM room_effects WHERE enabled = 1",
        ) else {
            return Vec::new();
        };
        let rows = stmt
            .query_map([], |row| {
                let enabled: i64 = row.get(2)?;
                Ok(RoomEffectRecord {
                    room_id: row.get::<_, String>(0)?,
                    effect_id: row.get::<_, String>(1)?,
                    enabled: enabled != 0,
                    params_json: row.get::<_, String>(3)?,
                    snapshot_json: row.get::<_, Option<String>>(4)?,
                    internal_state_json: row.get::<_, Option<String>>(5)?,
                    started_at_ms: row.get::<_, i64>(6)?,
                    overrides_json: row
                        .get::<_, Option<String>>(7)?
                        .unwrap_or_else(|| "[]".into()),
                })
            })
            .ok();
        rows.map(|iter| iter.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
    }

    /// Persist updated internal state for a live effect. Called by the runner
    /// on the effect's declared cadence.
    pub fn update_effect_internal_state(
        &mut self,
        room_id: &str,
        effect_id: &str,
        state_json: Option<&str>,
    ) {
        if let Err(e) = self.conn.execute(
            "UPDATE room_effects SET internal_state_json = ?1 WHERE room_id = ?2 AND effect_id = ?3",
            params![state_json, room_id, effect_id],
        ) {
            warn!(error = %e, room_id, effect_id, "update_effect_internal_state failed");
        }
    }

    /// Add or remove a device from the override list for the active effect in
    /// a room. Returns the new override list, or `None` if there is no active
    /// effect for the room.
    pub fn set_effect_override(
        &mut self,
        room_id: &str,
        device_id: &str,
        excluded: bool,
    ) -> rusqlite::Result<Option<Vec<String>>> {
        let current: Option<String> = self
            .conn
            .query_row(
                "SELECT overrides_json FROM room_effects WHERE room_id = ?1 AND enabled = 1",
                params![room_id],
                |row| row.get(0),
            )
            .optional()?
            .flatten();

        let Some(json) = current else { return Ok(None) };

        let mut overrides: Vec<String> = serde_json::from_str(&json).unwrap_or_else(|e| {
            warn!(error = %e, room_id, "overrides_json corrupt — resetting to empty");
            vec![]
        });

        if excluded {
            if !overrides.iter().any(|d| d == device_id) {
                overrides.push(device_id.to_string());
            }
        } else {
            overrides.retain(|d| d != device_id);
        }

        let new_json = serde_json::to_string(&overrides).unwrap_or_else(|_| "[]".into());
        self.conn.execute(
            "UPDATE room_effects SET overrides_json = ?1 WHERE room_id = ?2 AND enabled = 1",
            params![new_json, room_id],
        )?;
        Ok(Some(overrides))
    }
}
