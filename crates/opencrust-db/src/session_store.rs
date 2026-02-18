use opencrust_common::{Error, Result};
use rusqlite::Connection;
use rusqlite::params;
use std::path::Path;
use tracing::info;

/// Persisted message row loaded from the session store.
#[derive(Debug, Clone)]
pub struct StoredMessage {
    pub direction: String,
    pub content: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub metadata: serde_json::Value,
}

/// Persistent storage for conversation sessions and message history.
pub struct SessionStore {
    conn: Connection,
}

impl SessionStore {
    pub fn open(db_path: &Path) -> Result<Self> {
        info!("opening session store at {}", db_path.display());
        let conn = Connection::open(db_path)
            .map_err(|e| Error::Database(format!("failed to open database: {e}")))?;

        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .map_err(|e| Error::Database(format!("failed to set pragmas: {e}")))?;

        let store = Self { conn };
        store.run_migrations()?;
        Ok(store)
    }

    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()
            .map_err(|e| Error::Database(format!("failed to open in-memory database: {e}")))?;

        let store = Self { conn };
        store.run_migrations()?;
        Ok(store)
    }

    fn run_migrations(&self) -> Result<()> {
        self.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS sessions (
                    id TEXT PRIMARY KEY,
                    channel_id TEXT NOT NULL,
                    user_id TEXT NOT NULL,
                    created_at TEXT NOT NULL DEFAULT (datetime('now')),
                    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                    metadata TEXT DEFAULT '{}'
                );

                CREATE TABLE IF NOT EXISTS messages (
                    id TEXT PRIMARY KEY,
                    session_id TEXT NOT NULL REFERENCES sessions(id),
                    direction TEXT NOT NULL,
                    content TEXT NOT NULL,
                    timestamp TEXT NOT NULL,
                    metadata TEXT DEFAULT '{}',
                    created_at TEXT NOT NULL DEFAULT (datetime('now'))
                );

                CREATE INDEX IF NOT EXISTS idx_messages_session
                    ON messages(session_id, timestamp);",
            )
            .map_err(|e| Error::Database(format!("migration failed: {e}")))?;

        Ok(())
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Create or update a session row.
    pub fn upsert_session(
        &self,
        session_id: &str,
        channel_id: &str,
        user_id: &str,
        metadata: &serde_json::Value,
    ) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO sessions (id, channel_id, user_id, metadata)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(id) DO UPDATE SET
                   channel_id = excluded.channel_id,
                   user_id = excluded.user_id,
                   metadata = excluded.metadata,
                   updated_at = datetime('now')",
                params![session_id, channel_id, user_id, metadata.to_string()],
            )
            .map_err(|e| Error::Database(format!("failed to upsert session: {e}")))?;
        Ok(())
    }

    /// Append a single message to a session.
    pub fn append_message(
        &self,
        session_id: &str,
        direction: &str,
        content: &str,
        timestamp: chrono::DateTime<chrono::Utc>,
        metadata: &serde_json::Value,
    ) -> Result<()> {
        let message_id = uuid::Uuid::new_v4().to_string();
        self.conn
            .execute(
                "INSERT INTO messages (id, session_id, direction, content, timestamp, metadata)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    message_id,
                    session_id,
                    direction,
                    content,
                    timestamp.to_rfc3339(),
                    metadata.to_string()
                ],
            )
            .map_err(|e| Error::Database(format!("failed to append message: {e}")))?;
        Ok(())
    }

    /// Load recent messages for a session in chronological order.
    pub fn load_recent_messages(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<StoredMessage>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT direction, content, timestamp, metadata
                 FROM messages
                 WHERE session_id = ?1
                 ORDER BY rowid DESC
                 LIMIT ?2",
            )
            .map_err(|e| Error::Database(format!("failed to prepare message query: {e}")))?;

        let rows = stmt
            .query_map(params![session_id, limit as i64], |row| {
                let timestamp_raw: String = row.get(2)?;
                let metadata_raw: String = row.get(3)?;
                Ok(StoredMessage {
                    direction: row.get(0)?,
                    content: row.get(1)?,
                    timestamp: parse_timestamp(&timestamp_raw),
                    metadata: serde_json::from_str(&metadata_raw)
                        .unwrap_or(serde_json::Value::Null),
                })
            })
            .map_err(|e| Error::Database(format!("failed to load messages: {e}")))?;

        let mut messages = Vec::new();
        for row in rows {
            messages.push(
                row.map_err(|e| Error::Database(format!("failed to read message row: {e}")))?,
            );
        }

        // Query is DESC for efficient tail fetch; return in chronological order.
        messages.reverse();
        Ok(messages)
    }
}

fn parse_timestamp(value: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .unwrap_or_else(|_| chrono::Utc::now())
}

#[cfg(test)]
mod tests {
    use super::SessionStore;

    #[test]
    fn upsert_and_load_recent_messages_round_trip() {
        let store = SessionStore::in_memory().expect("in-memory store should open");
        let session_id = "session-1";

        store
            .upsert_session(
                session_id,
                "discord",
                "user-1",
                &serde_json::json!({"continuity_key":"bus:global"}),
            )
            .expect("session upsert should succeed");

        store
            .append_message(
                session_id,
                "user",
                "hello",
                chrono::Utc::now(),
                &serde_json::json!({}),
            )
            .expect("user message append should succeed");

        store
            .append_message(
                session_id,
                "assistant",
                "hi there",
                chrono::Utc::now(),
                &serde_json::json!({}),
            )
            .expect("assistant message append should succeed");

        let messages = store
            .load_recent_messages(session_id, 10)
            .expect("message load should succeed");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].direction, "user");
        assert_eq!(messages[0].content, "hello");
        assert_eq!(messages[1].direction, "assistant");
        assert_eq!(messages[1].content, "hi there");
    }
}
