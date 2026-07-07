// Switch -> action binding CRUD (switch_bindings table). See
// `SwitchBindingRecord`'s doc comment in mod.rs for the shape, and
// server.rs's `MeshMessage::SwitchAction` handler for where a binding
// actually gets dispatched to lights.
use super::{Registry, SwitchBindingRecord};
use rusqlite::{OptionalExtension, params};

fn row_to_binding(row: &rusqlite::Row) -> rusqlite::Result<SwitchBindingRecord> {
    Ok(SwitchBindingRecord {
        id: row.get(0)?,
        device_id: row.get(1)?,
        action: row.get(2)?,
        target_kind: row.get(3)?,
        target_id: row.get(4)?,
        command: row.get(5)?,
        step_delta: row.get(6)?,
    })
}

const SELECT_COLS: &str = "id, device_id, action, target_kind, target_id, command, step_delta";

impl Registry {
    /// One binding per (device_id, action) — a given button press or dial
    /// rotation direction does exactly one thing. Re-binding the same pair
    /// replaces the previous target/command outright (same "last save
    /// wins" shape as re-saving a scene under an existing name). Returns
    /// the binding's id.
    pub fn create_switch_binding(
        &mut self,
        device_id: &str,
        action: &str,
        target_kind: &str,
        target_id: &str,
        command: &str,
        step_delta: Option<i32>,
    ) -> rusqlite::Result<String> {
        let new_id = uuid::Uuid::new_v4().to_string();
        self.conn.execute(
            "INSERT INTO switch_bindings (id, device_id, action, target_kind, target_id, command, step_delta)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(device_id, action) DO UPDATE SET
                 target_kind = excluded.target_kind,
                 target_id   = excluded.target_id,
                 command     = excluded.command,
                 step_delta  = excluded.step_delta",
            params![new_id, device_id, action, target_kind, target_id, command, step_delta],
        )?;
        // On a conflict the freshly-generated `new_id` is discarded in
        // favour of the pre-existing row's id — read back whichever one is
        // actually stored rather than assuming it's `new_id`.
        self.conn.query_row(
            "SELECT id FROM switch_bindings WHERE device_id = ?1 AND action = ?2",
            params![device_id, action],
            |row| row.get(0),
        )
    }

    pub fn list_switch_bindings(&self) -> rusqlite::Result<Vec<SwitchBindingRecord>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {SELECT_COLS} FROM switch_bindings ORDER BY device_id, action"
        ))?;
        let rows = stmt.query_map([], row_to_binding)?;
        rows.collect()
    }

    /// Looked up on every `SwitchAction` report — `None` (the overwhelming
    /// common case, most button presses have no binding) is not an error.
    pub fn find_switch_binding(
        &self,
        device_id: &str,
        action: &str,
    ) -> Option<SwitchBindingRecord> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {SELECT_COLS} FROM switch_bindings WHERE device_id = ?1 AND action = ?2"
                ),
                params![device_id, action],
                row_to_binding,
            )
            .optional()
            .ok()
            .flatten()
    }

    pub fn delete_switch_binding(&mut self, id: &str) -> rusqlite::Result<bool> {
        let n = self
            .conn
            .execute("DELETE FROM switch_bindings WHERE id = ?1", params![id])?;
        Ok(n > 0)
    }

    /// Resolve a binding's target to the concrete device ids it should
    /// affect *right now* — a room's or group's membership can change after
    /// the binding was created, so this always re-reads live membership
    /// rather than caching a device list on the binding row itself.
    pub fn resolve_switch_binding_targets(&self, binding: &SwitchBindingRecord) -> Vec<String> {
        match binding.target_kind.as_str() {
            "room" => self
                .get_room(&binding.target_id)
                .map(|r| r.device_ids)
                .unwrap_or_default(),
            "group" => self
                .get_room_group(&binding.target_id)
                .map(|g| g.device_ids)
                .unwrap_or_default(),
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::Registry;

    fn make_room(reg: &mut Registry) -> String {
        reg.create_room("Larder").id
    }

    #[test]
    fn create_and_find_roundtrip() {
        let mut reg = Registry::new();
        let room_id = make_room(&mut reg);
        let id = reg
            .create_switch_binding("dial1", "button_1_press", "room", &room_id, "toggle", None)
            .unwrap();
        let found = reg.find_switch_binding("dial1", "button_1_press").unwrap();
        assert_eq!(found.id, id);
        assert_eq!(found.command, "toggle");
        assert_eq!(found.target_id, room_id);
    }

    #[test]
    fn find_returns_none_when_no_binding_exists() {
        let reg = Registry::new();
        assert!(reg.find_switch_binding("dial1", "button_1_press").is_none());
    }

    #[test]
    fn rebinding_same_device_and_action_replaces_the_previous_one() {
        let mut reg = Registry::new();
        let room_id = make_room(&mut reg);
        let first_id = reg
            .create_switch_binding("dial1", "button_1_press", "room", &room_id, "on", None)
            .unwrap();
        let second_id = reg
            .create_switch_binding("dial1", "button_1_press", "room", &room_id, "off", None)
            .unwrap();
        assert_eq!(
            first_id, second_id,
            "same (device_id, action) should keep the same row id"
        );
        let found = reg.find_switch_binding("dial1", "button_1_press").unwrap();
        assert_eq!(found.command, "off");
        assert_eq!(reg.list_switch_bindings().unwrap().len(), 1);
    }

    #[test]
    fn brightness_step_binding_stores_delta() {
        let mut reg = Registry::new();
        let room_id = make_room(&mut reg);
        reg.create_switch_binding(
            "dial1",
            "brightness_step_up",
            "room",
            &room_id,
            "brightness_step",
            Some(25),
        )
        .unwrap();
        let found = reg
            .find_switch_binding("dial1", "brightness_step_up")
            .unwrap();
        assert_eq!(found.step_delta, Some(25));
    }

    #[test]
    fn delete_switch_binding_removes_it() {
        let mut reg = Registry::new();
        let room_id = make_room(&mut reg);
        let id = reg
            .create_switch_binding("dial1", "button_1_press", "room", &room_id, "toggle", None)
            .unwrap();
        assert!(reg.delete_switch_binding(&id).unwrap());
        assert!(reg.find_switch_binding("dial1", "button_1_press").is_none());
    }

    #[test]
    fn delete_switch_binding_returns_false_for_unknown_id() {
        let mut reg = Registry::new();
        assert!(!reg.delete_switch_binding("nope").unwrap());
    }

    #[test]
    fn resolve_targets_room() {
        let mut reg = Registry::new();
        let room_id = make_room(&mut reg);
        reg.add_device_to_room(&room_id, "bulb1");
        reg.add_device_to_room(&room_id, "bulb2");
        let binding = SwitchBindingRecord {
            id: "b1".into(),
            device_id: "dial1".into(),
            action: "button_1_press".into(),
            target_kind: "room".into(),
            target_id: room_id,
            command: "toggle".into(),
            step_delta: None,
        };
        let mut targets = reg.resolve_switch_binding_targets(&binding);
        targets.sort();
        assert_eq!(targets, vec!["bulb1".to_string(), "bulb2".to_string()]);
    }

    #[test]
    fn resolve_targets_group() {
        let mut reg = Registry::new();
        let room_id = make_room(&mut reg);
        reg.add_device_to_room(&room_id, "bulb1");
        let group_id = reg.create_room_group(&room_id, "Counter").id;
        reg.set_device_group("bulb1", Some(&group_id));
        let binding = SwitchBindingRecord {
            id: "b1".into(),
            device_id: "dial1".into(),
            action: "button_1_press".into(),
            target_kind: "group".into(),
            target_id: group_id,
            command: "toggle".into(),
            step_delta: None,
        };
        assert_eq!(
            reg.resolve_switch_binding_targets(&binding),
            vec!["bulb1".to_string()]
        );
    }

    #[test]
    fn resolve_targets_unknown_kind_returns_empty() {
        let reg = Registry::new();
        let binding = SwitchBindingRecord {
            id: "b1".into(),
            device_id: "dial1".into(),
            action: "button_1_press".into(),
            target_kind: "bogus".into(),
            target_id: "whatever".into(),
            command: "toggle".into(),
            step_delta: None,
        };
        assert!(reg.resolve_switch_binding_targets(&binding).is_empty());
    }
}
