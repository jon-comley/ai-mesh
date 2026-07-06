// Per-room, per-wall reference photos (Phase 4 layout-editor aid — see
// room_wall_photos' schema comment in mod.rs for why this exists).
use super::Registry;
use rusqlite::params;
use tracing::warn;

impl Registry {
    pub fn set_wall_photo(&mut self, room_id: &str, wall_edge: &str, data_uri: &str) {
        if let Err(e) = self.conn.execute(
            "INSERT INTO room_wall_photos (room_id, wall_edge, data_uri) VALUES (?1, ?2, ?3)
             ON CONFLICT(room_id, wall_edge) DO UPDATE SET data_uri = excluded.data_uri",
            params![room_id, wall_edge, data_uri],
        ) {
            warn!(error = %e, "set_wall_photo failed");
        }
    }

    pub fn delete_wall_photo(&mut self, room_id: &str, wall_edge: &str) {
        if let Err(e) = self.conn.execute(
            "DELETE FROM room_wall_photos WHERE room_id = ?1 AND wall_edge = ?2",
            params![room_id, wall_edge],
        ) {
            warn!(error = %e, "delete_wall_photo failed");
        }
    }

    pub fn wall_photo_exists(&self, room_id: &str, wall_edge: &str) -> bool {
        self.conn
            .query_row(
                "SELECT 1 FROM room_wall_photos WHERE room_id = ?1 AND wall_edge = ?2",
                params![room_id, wall_edge],
                |_| Ok(()),
            )
            .is_ok()
    }

    /// (wall_edge, data_uri) pairs for every photo attached to this room —
    /// sparse, only walls that actually have one.
    pub fn get_wall_photos(&self, room_id: &str) -> Vec<(String, String)> {
        let mut stmt = match self
            .conn
            .prepare("SELECT wall_edge, data_uri FROM room_wall_photos WHERE room_id = ?1")
        {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "get_wall_photos prepare failed");
                return vec![];
            }
        };
        stmt.query_map(params![room_id], |row| Ok((row.get(0)?, row.get(1)?)))
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use crate::registry::Registry;

    #[test]
    fn set_wall_photo_then_get_returns_it() {
        let mut reg = Registry::new();
        let room = reg.create_room("Kitchen");
        reg.set_wall_photo(&room.id, "N", "data:image/jpeg;base64,abc");
        let photos = reg.get_wall_photos(&room.id);
        assert_eq!(
            photos,
            vec![("N".to_string(), "data:image/jpeg;base64,abc".to_string())]
        );
    }

    #[test]
    fn set_wall_photo_twice_replaces_not_duplicates() {
        let mut reg = Registry::new();
        let room = reg.create_room("Kitchen");
        reg.set_wall_photo(&room.id, "N", "data:one");
        reg.set_wall_photo(&room.id, "N", "data:two");
        let photos = reg.get_wall_photos(&room.id);
        assert_eq!(photos, vec![("N".to_string(), "data:two".to_string())]);
    }

    #[test]
    fn delete_wall_photo_removes_it() {
        let mut reg = Registry::new();
        let room = reg.create_room("Kitchen");
        reg.set_wall_photo(&room.id, "N", "data:one");
        reg.delete_wall_photo(&room.id, "N");
        assert!(reg.get_wall_photos(&room.id).is_empty());
    }

    #[test]
    fn wall_photo_exists_reflects_creation_and_deletion() {
        let mut reg = Registry::new();
        let room = reg.create_room("Kitchen");
        assert!(!reg.wall_photo_exists(&room.id, "N"));
        reg.set_wall_photo(&room.id, "N", "data:one");
        assert!(reg.wall_photo_exists(&room.id, "N"));
        reg.delete_wall_photo(&room.id, "N");
        assert!(!reg.wall_photo_exists(&room.id, "N"));
    }

    #[test]
    fn delete_room_cascades_to_wall_photos() {
        let mut reg = Registry::new();
        let room = reg.create_room("Kitchen");
        reg.set_wall_photo(&room.id, "N", "data:one");
        reg.delete_room(&room.id);
        assert!(reg.get_wall_photos(&room.id).is_empty());
    }

    #[test]
    fn get_wall_photos_is_sparse_and_per_room() {
        let mut reg = Registry::new();
        let a = reg.create_room("Kitchen");
        let b = reg.create_room("Lounge");
        reg.set_wall_photo(&a.id, "N", "data:kitchen-n");
        reg.set_wall_photo(&a.id, "E", "data:kitchen-e");
        reg.set_wall_photo(&b.id, "S", "data:lounge-s");
        let mut a_photos = reg.get_wall_photos(&a.id);
        a_photos.sort();
        assert_eq!(
            a_photos,
            vec![
                ("E".to_string(), "data:kitchen-e".to_string()),
                ("N".to_string(), "data:kitchen-n".to_string()),
            ]
        );
        assert_eq!(
            reg.get_wall_photos(&b.id),
            vec![("S".to_string(), "data:lounge-s".to_string())]
        );
    }
}
