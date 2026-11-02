//! `agents.db`: each agent's history (one row whenever what the tile shows
//! changes) and when Run now was last pressed, so both survive a restart.

use std::path::Path;

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

pub struct Store {
    db: Connection,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HistoryRow {
    pub at: DateTime<Utc>,
    pub state: String,
    pub line: String,
    pub waiting: usize,
}

impl Store {
    pub fn open(path: &Path) -> rusqlite::Result<Store> {
        let db = Connection::open(path)?;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS history (
                 agent TEXT NOT NULL, at TEXT NOT NULL, state TEXT NOT NULL,
                 line TEXT NOT NULL, waiting INTEGER NOT NULL);
             CREATE INDEX IF NOT EXISTS history_agent ON history (agent, at);
             CREATE TABLE IF NOT EXISTS runs (agent TEXT PRIMARY KEY, started TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS chats (agent TEXT PRIMARY KEY, session TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS messages (
                 agent TEXT NOT NULL, at TEXT NOT NULL, role TEXT NOT NULL, text TEXT NOT NULL);
             CREATE INDEX IF NOT EXISTS messages_agent ON messages (agent, at);",
        )?;
        Ok(Store { db })
    }

    /// Adds a row unless it says the same as the agent's last one.
    pub fn record(&self, agent: &str, row: &HistoryRow) -> rusqlite::Result<bool> {
        let last = self.recent(agent, 1)?.into_iter().next();
        if last
            .is_some_and(|l| l.state == row.state && l.line == row.line && l.waiting == row.waiting)
        {
            return Ok(false);
        }
        self.db.execute(
            "INSERT INTO history (agent, at, state, line, waiting) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                agent,
                row.at.to_rfc3339(),
                row.state,
                row.line,
                row.waiting as i64
            ],
        )?;
        Ok(true)
    }

    /// The agent's newest `n` rows, newest first.
    pub fn recent(&self, agent: &str, n: usize) -> rusqlite::Result<Vec<HistoryRow>> {
        let mut q = self
            .db
            .prepare("SELECT at, state, line, waiting FROM history WHERE agent = ?1 ORDER BY at DESC, rowid DESC LIMIT ?2")?;
        let rows = q.query_map(params![agent, n as i64], |r| {
            let at: String = r.get(0)?;
            Ok(HistoryRow {
                at: DateTime::parse_from_rfc3339(&at)
                    .map(|t| t.with_timezone(&Utc))
                    .unwrap_or_default(),
                state: r.get(1)?,
                line: r.get(2)?,
                waiting: r.get::<_, i64>(3)? as usize,
            })
        })?;
        rows.collect()
    }

    pub fn set_run_started(&self, agent: &str, at: DateTime<Utc>) -> rusqlite::Result<()> {
        self.db.execute(
            "INSERT INTO runs (agent, started) VALUES (?1, ?2) ON CONFLICT (agent) DO UPDATE SET started = ?2",
            params![agent, at.to_rfc3339()],
        )?;
        Ok(())
    }

    pub fn run_started(&self, agent: &str) -> rusqlite::Result<Option<DateTime<Utc>>> {
        let at: Option<String> = self
            .db
            .query_row(
                "SELECT started FROM runs WHERE agent = ?1",
                params![agent],
                |r| r.get(0),
            )
            .optional()?;
        Ok(at
            .and_then(|a| DateTime::parse_from_rfc3339(&a).ok())
            .map(|t| t.with_timezone(&Utc)))
    }
}

/// One message in a worker's chat, as the page shows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Message {
    pub at: DateTime<Utc>,
    /// `jon`, `worker` or `tool`.
    pub role: String,
    pub text: String,
}

impl Store {
    /// The `claude` session the worker's chat resumes, if one is open.
    pub fn chat_session(&self, agent: &str) -> rusqlite::Result<Option<String>> {
        self.db
            .query_row(
                "SELECT session FROM chats WHERE agent = ?1",
                params![agent],
                |r| r.get(0),
            )
            .optional()
    }

    pub fn set_chat_session(&self, agent: &str, session: &str) -> rusqlite::Result<()> {
        self.db.execute(
            "INSERT INTO chats (agent, session) VALUES (?1, ?2) ON CONFLICT (agent) DO UPDATE SET session = ?2",
            params![agent, session],
        )?;
        Ok(())
    }

    pub fn add_message(&self, agent: &str, role: &str, text: &str) -> rusqlite::Result<()> {
        self.db.execute(
            "INSERT INTO messages (agent, at, role, text) VALUES (?1, ?2, ?3, ?4)",
            params![agent, Utc::now().to_rfc3339(), role, text],
        )?;
        Ok(())
    }

    /// The open conversation's messages, oldest first.
    pub fn messages(&self, agent: &str) -> rusqlite::Result<Vec<Message>> {
        let mut q = self
            .db
            .prepare("SELECT at, role, text FROM messages WHERE agent = ?1 ORDER BY at, rowid")?;
        let rows = q.query_map(params![agent], |r| {
            let at: String = r.get(0)?;
            Ok(Message {
                at: DateTime::parse_from_rfc3339(&at)
                    .map(|t| t.with_timezone(&Utc))
                    .unwrap_or_default(),
                role: r.get(1)?,
                text: r.get(2)?,
            })
        })?;
        rows.collect()
    }

    /// Starts a new conversation: forgets the session and the messages.
    pub fn clear_chat(&self, agent: &str) -> rusqlite::Result<()> {
        self.db
            .execute("DELETE FROM chats WHERE agent = ?1", params![agent])?;
        self.db
            .execute("DELETE FROM messages WHERE agent = ?1", params![agent])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(at: &str, state: &str, line: &str, waiting: usize) -> HistoryRow {
        HistoryRow {
            at: DateTime::parse_from_rfc3339(at)
                .unwrap()
                .with_timezone(&Utc),
            state: state.into(),
            line: line.into(),
            waiting,
        }
    }

    #[test]
    fn records_only_changes_and_reads_newest_first() {
        let s = Store::open(Path::new(":memory:")).unwrap();
        assert!(
            s.record("Desk", &row("2026-11-03T07:40:00Z", "ok", "first", 2))
                .unwrap()
        );
        assert!(
            !s.record("Desk", &row("2026-11-03T07:55:00Z", "ok", "first", 2))
                .unwrap()
        );
        assert!(
            s.record("Desk", &row("2026-11-03T08:10:00Z", "ok", "first", 1))
                .unwrap()
        );
        assert!(
            s.record("Standards", &row("2026-11-03T08:10:00Z", "new", "", 0))
                .unwrap()
        );
        let r = s.recent("Desk", 10).unwrap();
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].waiting, 1);
        assert_eq!(r[1].line, "first");
    }

    #[test]
    fn keeps_one_chat_per_agent_until_cleared() {
        let s = Store::open(Path::new(":memory:")).unwrap();
        assert_eq!(s.chat_session("Desk").unwrap(), None);
        s.set_chat_session("Desk", "a").unwrap();
        s.set_chat_session("Desk", "b").unwrap();
        s.add_message("Desk", "jon", "hello").unwrap();
        s.add_message("Desk", "worker", "hi").unwrap();
        s.add_message("Standards", "jon", "other").unwrap();
        assert_eq!(s.chat_session("Desk").unwrap().as_deref(), Some("b"));
        let m = s.messages("Desk").unwrap();
        assert_eq!(
            m.iter().map(|m| m.role.as_str()).collect::<Vec<_>>(),
            ["jon", "worker"]
        );
        s.clear_chat("Desk").unwrap();
        assert_eq!(s.chat_session("Desk").unwrap(), None);
        assert!(s.messages("Desk").unwrap().is_empty());
        assert_eq!(s.messages("Standards").unwrap().len(), 1);
    }

    #[test]
    fn keeps_the_latest_run_now() {
        let s = Store::open(Path::new(":memory:")).unwrap();
        assert_eq!(s.run_started("Desk").unwrap(), None);
        let a = row("2026-11-03T07:40:00Z", "", "", 0).at;
        let b = row("2026-11-03T09:00:00Z", "", "", 0).at;
        s.set_run_started("Desk", a).unwrap();
        s.set_run_started("Desk", b).unwrap();
        assert_eq!(s.run_started("Desk").unwrap(), Some(b));
    }
}
