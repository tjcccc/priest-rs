use async_trait::async_trait;
use chrono::{DateTime, NaiveDateTime, Utc};
use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::Mutex;
use uuid::Uuid;

use crate::errors::PriestError;
use super::model::{Session, Turn};
use super::store::SessionStore;

pub struct SqliteSessionStore {
    conn: Mutex<Connection>,
}

impl SqliteSessionStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, PriestError> {
        let conn = Connection::open(path).map_err(|e| PriestError::SessionStoreError {
            message: e.to_string(),
        })?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS sessions (
                 id           TEXT PRIMARY KEY,
                 profile_name TEXT NOT NULL,
                 created_at   TEXT NOT NULL,
                 updated_at   TEXT NOT NULL,
                 metadata     TEXT NOT NULL DEFAULT '{}'
             );
             CREATE TABLE IF NOT EXISTS turns (
                 id         INTEGER PRIMARY KEY AUTOINCREMENT,
                 session_id TEXT NOT NULL REFERENCES sessions(id),
                 role       TEXT NOT NULL,
                 content    TEXT NOT NULL,
                 timestamp  TEXT NOT NULL
             );",
        )
        .map_err(|e| PriestError::SessionStoreError { message: e.to_string() })?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    fn parse_ts(s: &str) -> DateTime<Utc> {
        // Lenient: accept with or without microseconds, with or without +00:00
        let clean = s.trim_end_matches("+00:00").trim_end_matches('Z');
        let fmt_micro = "%Y-%m-%dT%H:%M:%S%.6f";
        let fmt_sec   = "%Y-%m-%dT%H:%M:%S";
        NaiveDateTime::parse_from_str(clean, fmt_micro)
            .or_else(|_| NaiveDateTime::parse_from_str(clean, fmt_sec))
            .map(|ndt| ndt.and_utc())
            .unwrap_or_else(|_| Utc::now())
    }
}

#[async_trait]
impl SessionStore for SqliteSessionStore {
    async fn get(&self, id: &str) -> Result<Option<Session>, PriestError> {
        let conn = self.conn.lock().unwrap();

        let row: Option<(String, String, String, String, String)> = conn
            .query_row(
                "SELECT id, profile_name, created_at, updated_at, metadata FROM sessions WHERE id = ?1",
                params![id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .optional()
            .map_err(|e| PriestError::SessionStoreError { message: e.to_string() })?;

        let Some((sid, profile_name, created_at_s, updated_at_s, _meta)) = row else {
            return Ok(None);
        };

        let mut stmt = conn
            .prepare("SELECT role, content, timestamp FROM turns WHERE session_id = ?1 ORDER BY id ASC")
            .map_err(|e| PriestError::SessionStoreError { message: e.to_string() })?;

        let turns: Vec<Turn> = stmt
            .query_map(params![sid], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?))
            })
            .map_err(|e| PriestError::SessionStoreError { message: e.to_string() })?
            .filter_map(|r| r.ok())
            .map(|(role, content, ts)| Turn { role, content, timestamp: Self::parse_ts(&ts) })
            .collect();

        Ok(Some(Session {
            id: sid,
            profile_name,
            created_at: Self::parse_ts(&created_at_s),
            updated_at: Self::parse_ts(&updated_at_s),
            turns,
            metadata: Default::default(),
        }))
    }

    async fn create(&self, profile_name: &str, id: Option<&str>) -> Result<Session, PriestError> {
        let session_id = id.map(|s| s.to_string()).unwrap_or_else(|| Uuid::new_v4().to_string());
        let session = Session::new(session_id.clone(), profile_name);
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO sessions (id, profile_name, created_at, updated_at, metadata) VALUES (?1, ?2, ?3, ?4, '{}')",
            params![
                session.id,
                session.profile_name,
                Session::format_timestamp(&session.created_at),
                Session::format_timestamp(&session.updated_at),
            ],
        )
        .map_err(|e| PriestError::SessionStoreError { message: e.to_string() })?;
        Ok(session)
    }

    async fn save(&self, session: &Session) -> Result<(), PriestError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE sessions SET updated_at = ?1, metadata = '{}' WHERE id = ?2",
            params![Session::format_timestamp(&session.updated_at), session.id],
        )
        .map_err(|e| PriestError::SessionStoreError { message: e.to_string() })?;

        conn.execute("DELETE FROM turns WHERE session_id = ?1", params![session.id])
            .map_err(|e| PriestError::SessionStoreError { message: e.to_string() })?;

        for turn in &session.turns {
            conn.execute(
                "INSERT INTO turns (session_id, role, content, timestamp) VALUES (?1, ?2, ?3, ?4)",
                params![session.id, turn.role, turn.content, Session::format_timestamp(&turn.timestamp)],
            )
            .map_err(|e| PriestError::SessionStoreError { message: e.to_string() })?;
        }
        Ok(())
    }
}

trait OptionalExt<T> {
    fn optional(self) -> Result<Option<T>, rusqlite::Error>;
}

impl<T> OptionalExt<T> for Result<T, rusqlite::Error> {
    fn optional(self) -> Result<Option<T>, rusqlite::Error> {
        match self {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }
}
