// Window/door openings per room (used by the spatial light effects).
use super::{Opening, Registry, gen_uuid};
use rusqlite::params;
use std::collections::HashMap;
use tracing::warn;

impl Registry {
    pub fn create_opening(
        &mut self,
        room_id: &str,
        opening_type: &str,
        wall_edge: &str,
        x_norm: f32,
        width_norm: f32,
        transmission: f32,
    ) -> Opening {
        let id = gen_uuid();
        if let Err(e) = self.conn.execute(
            "INSERT INTO openings (id, room_id, opening_type, wall_edge, x_norm, width_norm, transmission)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![id, room_id, opening_type, wall_edge, x_norm, width_norm, transmission],
        ) {
            warn!(error = %e, "create_opening failed");
        }
        Opening {
            id,
            room_id: room_id.to_owned(),
            opening_type: opening_type.to_owned(),
            wall_edge: wall_edge.to_owned(),
            x_norm,
            width_norm,
            transmission,
            opening_scope: "exterior".to_owned(),
            height_norm: 0.3,
            height_span: 0.5,
        }
    }

    pub fn get_openings_for_room(&self, room_id: &str) -> Vec<Opening> {
        let mut stmt = match self.conn.prepare(
            "SELECT id, room_id, opening_type, wall_edge, x_norm, width_norm, transmission,
                    opening_scope, height_norm, height_span
             FROM openings WHERE room_id = ?1",
        ) {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "get_openings_for_room prepare failed");
                return vec![];
            }
        };
        stmt.query_map(params![room_id], |row| {
            Ok(Opening {
                id: row.get(0)?,
                room_id: row.get(1)?,
                opening_type: row.get(2)?,
                wall_edge: row.get(3)?,
                x_norm: row.get(4)?,
                width_norm: row.get(5)?,
                transmission: row.get(6)?,
                opening_scope: row.get(7)?,
                height_norm: row.get(8)?,
                height_span: row.get(9)?,
            })
        })
        .map(|rows| rows.filter_map(|r| r.ok()).collect())
        .unwrap_or_default()
    }

    pub fn get_all_openings_by_room(&self) -> HashMap<String, Vec<Opening>> {
        let mut stmt = match self.conn.prepare(
            "SELECT id, room_id, opening_type, wall_edge, x_norm, width_norm, transmission,
                    opening_scope, height_norm, height_span
             FROM openings",
        ) {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "get_all_openings_by_room prepare failed");
                return HashMap::new();
            }
        };
        let mut map: HashMap<String, Vec<Opening>> = HashMap::new();
        if let Ok(rows) = stmt.query_map([], |row| {
            Ok(Opening {
                id: row.get(0)?,
                room_id: row.get(1)?,
                opening_type: row.get(2)?,
                wall_edge: row.get(3)?,
                x_norm: row.get(4)?,
                width_norm: row.get(5)?,
                transmission: row.get(6)?,
                opening_scope: row.get(7)?,
                height_norm: row.get(8)?,
                height_span: row.get(9)?,
            })
        }) {
            for o in rows.filter_map(|r| r.ok()) {
                map.entry(o.room_id.clone()).or_default().push(o);
            }
        }
        map
    }

    pub fn update_opening(
        &mut self,
        id: &str,
        x_norm: Option<f32>,
        width_norm: Option<f32>,
        transmission: Option<f32>,
        wall_edge: Option<&str>,
    ) {
        if let Some(v) = wall_edge
            && let Err(e) = self.conn.execute(
                "UPDATE openings SET wall_edge = ?1 WHERE id = ?2",
                params![v, id],
            )
        {
            warn!(error = %e, "update_opening wall_edge failed");
        }
        if let Some(v) = x_norm
            && let Err(e) = self.conn.execute(
                "UPDATE openings SET x_norm = ?1 WHERE id = ?2",
                params![v, id],
            )
        {
            warn!(error = %e, "update_opening x_norm failed");
        }
        if let Some(v) = width_norm
            && let Err(e) = self.conn.execute(
                "UPDATE openings SET width_norm = ?1 WHERE id = ?2",
                params![v, id],
            )
        {
            warn!(error = %e, "update_opening width_norm failed");
        }
        if let Some(v) = transmission
            && let Err(e) = self.conn.execute(
                "UPDATE openings SET transmission = ?1 WHERE id = ?2",
                params![v, id],
            )
        {
            warn!(error = %e, "update_opening transmission failed");
        }
    }

    pub fn opening_exists(&self, id: &str) -> bool {
        self.conn
            .query_row("SELECT 1 FROM openings WHERE id = ?1", params![id], |_| {
                Ok(())
            })
            .is_ok()
    }

    pub fn delete_opening(&mut self, id: &str) {
        if let Err(e) = self
            .conn
            .execute("DELETE FROM openings WHERE id = ?1", params![id])
        {
            warn!(error = %e, "delete_opening failed");
        }
    }
}
