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
                    ON messages(session_id, timestamp);

                CREATE TABLE IF NOT EXISTS scheduled_tasks (
                    id TEXT PRIMARY KEY,
                    session_id TEXT NOT NULL,
                    user_id TEXT NOT NULL,
                    execute_at TEXT NOT NULL,
                    payload TEXT NOT NULL,
                    status TEXT NOT NULL DEFAULT 'pending',
                    created_at TEXT NOT NULL DEFAULT (datetime('now'))
                );

                CREATE INDEX IF NOT EXISTS idx_tasks_execute_at
                    ON scheduled_tasks(execute_at) WHERE status = 'pending';",
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

    /// Schedule a task for future execution.
    pub fn schedule_task(
        &self,
        session_id: &str,
        user_id: &str,
        execute_at: chrono::DateTime<chrono::Utc>,
        payload: &str,
    ) -> Result<String> {
        let task_id = uuid::Uuid::new_v4().to_string();
        self.conn
            .execute(
                "INSERT INTO scheduled_tasks (id, session_id, user_id, execute_at, payload, status)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'pending')",
                params![
                    task_id,
                    session_id,
                    user_id,
                    execute_at.to_rfc3339(),
                    payload
                ],
            )
            .map_err(|e| Error::Database(format!("failed to schedule task: {e}")))?;
        Ok(task_id)
    }

    /// Poll for pending tasks that are due for execution.
    pub fn poll_due_tasks(&self) -> Result<Vec<ScheduledTask>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT t.id, t.session_id, s.channel_id, t.user_id, t.execute_at, t.payload, s.metadata
                 FROM scheduled_tasks t
                 JOIN sessions s ON t.session_id = s.id
                 WHERE t.status = 'pending' AND datetime(t.execute_at) <= datetime('now')
                 ORDER BY t.execute_at ASC
                 LIMIT 10",
            )
            .map_err(|e| Error::Database(format!("failed to prepare poll query: {e}")))?;

        let rows = stmt
            .query_map([], |row| {
                let execute_at_raw: String = row.get(4)?;
                let metadata_raw: String = row.get(6)?;
                Ok(ScheduledTask {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    channel_id: row.get(2)?,
                    user_id: row.get(3)?,
                    execute_at: parse_timestamp(&execute_at_raw),
                    payload: row.get(5)?,
                    session_metadata: serde_json::from_str(&metadata_raw)
                        .unwrap_or(serde_json::Value::Null),
                })
            })
            .map_err(|e| Error::Database(format!("failed to poll tasks: {e}")))?;

        let mut tasks = Vec::new();
        for row in rows {
            tasks.push(row.map_err(|e| Error::Database(format!("failed to read task row: {e}")))?);
        }
        Ok(tasks)
    }

    /// Mark a scheduled task as completed.
    pub fn complete_task(&self, task_id: &str) -> Result<()> {
        self.conn
            .execute(
                "UPDATE scheduled_tasks SET status = 'completed' WHERE id = ?1",
                params![task_id],
            )
            .map_err(|e| Error::Database(format!("failed to complete task: {e}")))?;
        Ok(())
    }

    /// Mark a scheduled task as failed so it won't be retried.
    pub fn fail_task(&self, task_id: &str) -> Result<()> {
        self.conn
            .execute(
                "UPDATE scheduled_tasks SET status = 'failed' WHERE id = ?1",
                params![task_id],
            )
            .map_err(|e| Error::Database(format!("failed to mark task as failed: {e}")))?;
        Ok(())
    }

    /// Load the metadata JSON for a session.
    pub fn load_session_metadata(&self, session_id: &str) -> Result<Option<serde_json::Value>> {
        let mut stmt = self
            .conn
            .prepare("SELECT metadata FROM sessions WHERE id = ?1")
            .map_err(|e| Error::Database(format!("failed to prepare metadata query: {e}")))?;

        let result: Option<String> = stmt.query_row(params![session_id], |row| row.get(0)).ok();

        match result {
            Some(raw) => {
                let value: serde_json::Value =
                    serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null);
                if value.is_null() {
                    Ok(None)
                } else {
                    Ok(Some(value))
                }
            }
            None => Ok(None),
        }
    }

    /// Delete all but the most recent `keep` messages for a session.
    /// Returns the number of deleted rows.
    pub fn prune_old_messages(&self, session_id: &str, keep: usize) -> Result<usize> {
        let deleted = self
            .conn
            .execute(
                "DELETE FROM messages WHERE session_id = ?1 AND rowid NOT IN (
                    SELECT rowid FROM messages WHERE session_id = ?1
                    ORDER BY rowid DESC LIMIT ?2
                )",
                params![session_id, keep as i64],
            )
            .map_err(|e| Error::Database(format!("failed to prune old messages: {e}")))?;
        Ok(deleted)
    }

    /// Count pending scheduled tasks for a given session.
    pub fn count_pending_tasks_for_session(&self, session_id: &str) -> Result<i64> {
        let count: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM scheduled_tasks WHERE session_id = ?1 AND status = 'pending'",
                params![session_id],
                |row| row.get(0),
            )
            .map_err(|e| Error::Database(format!("failed to count pending tasks: {e}")))?;
        Ok(count)
    }
}

/// Represents a scheduled background task.
#[derive(Debug, Clone)]
pub struct ScheduledTask {
    pub id: String,
    pub session_id: String,
    pub channel_id: String,
    pub user_id: String,
    pub execute_at: chrono::DateTime<chrono::Utc>,
    pub payload: String,
    pub session_metadata: serde_json::Value,
}

fn parse_timestamp(value: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .unwrap_or_else(|_| chrono::Utc::now())
}

#[cfg(test)]
mod tests {
    use super::SessionStore;
    use chrono::Duration;

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

    #[test]
    fn schedule_and_poll_tasks() {
        let store = SessionStore::in_memory().expect("in-memory store should open");
        let session_id = "session-task";

        // Ensure session exists for JOIN
        store
            .upsert_session(
                session_id,
                "discord",
                "user-1",
                &serde_json::json!({"foo":"bar"}),
            )
            .expect("upsert session");

        // Schedule a task in the past (immediately due)
        let due_time = chrono::Utc::now() - Duration::minutes(1);
        let task_id = store
            .schedule_task(session_id, "user-1", due_time, "check logs")
            .expect("schedule task should succeed");

        // Schedule a future task (not due)
        store
            .schedule_task(
                session_id,
                "user-1",
                chrono::Utc::now() + Duration::minutes(10),
                "future",
            )
            .expect("schedule future task should succeed");

        let due = store.poll_due_tasks().expect("poll should succeed");
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].id, task_id);
        assert_eq!(due[0].channel_id, "discord"); // Verified via JOIN
        assert_eq!(due[0].payload, "check logs");
        assert_eq!(
            due[0].session_metadata.get("foo").and_then(|v| v.as_str()),
            Some("bar")
        );

        store
            .complete_task(&task_id)
            .expect("complete should succeed");

        let due_after = store
            .poll_due_tasks()
            .expect("poll after complete should succeed");
        assert_eq!(due_after.len(), 0);
    }

    #[test]
    fn fail_task_prevents_re_poll() {
        let store = SessionStore::in_memory().expect("in-memory store should open");
        store
            .upsert_session("s1", "web", "u1", &serde_json::json!({}))
            .unwrap();

        let due_time = chrono::Utc::now() - Duration::minutes(1);
        let task_id = store.schedule_task("s1", "u1", due_time, "boom").unwrap();

        let due = store.poll_due_tasks().unwrap();
        assert_eq!(due.len(), 1);

        store.fail_task(&task_id).unwrap();

        let due_after = store.poll_due_tasks().unwrap();
        assert_eq!(due_after.len(), 0);
    }

    #[test]
    fn load_session_metadata_round_trip() {
        let store = SessionStore::in_memory().expect("in-memory store should open");
        let meta = serde_json::json!({"summary": "We discussed Rust."});
        store
            .upsert_session("s1", "web", "u1", &meta)
            .expect("upsert should succeed");

        let loaded = store
            .load_session_metadata("s1")
            .expect("load should succeed");
        assert_eq!(
            loaded.unwrap().get("summary").and_then(|v| v.as_str()),
            Some("We discussed Rust.")
        );
    }

    #[test]
    fn load_session_metadata_missing_session() {
        let store = SessionStore::in_memory().expect("in-memory store should open");
        let loaded = store
            .load_session_metadata("nonexistent")
            .expect("load should succeed");
        assert!(loaded.is_none());
    }

    #[test]
    fn prune_old_messages_keeps_recent() {
        let store = SessionStore::in_memory().expect("in-memory store should open");
        store
            .upsert_session("s1", "web", "u1", &serde_json::json!({}))
            .unwrap();

        for i in 0..10 {
            store
                .append_message(
                    "s1",
                    "user",
                    &format!("msg-{i}"),
                    chrono::Utc::now(),
                    &serde_json::json!({}),
                )
                .unwrap();
        }

        let deleted = store
            .prune_old_messages("s1", 3)
            .expect("prune should succeed");
        assert_eq!(deleted, 7);

        let remaining = store.load_recent_messages("s1", 100).unwrap();
        assert_eq!(remaining.len(), 3);
        assert_eq!(remaining[0].content, "msg-7");
        assert_eq!(remaining[1].content, "msg-8");
        assert_eq!(remaining[2].content, "msg-9");
    }

    #[test]
    fn count_pending_tasks_for_session() {
        let store = SessionStore::in_memory().expect("in-memory store should open");
        store
            .upsert_session("s1", "web", "u1", &serde_json::json!({}))
            .unwrap();
        store
            .upsert_session("s2", "web", "u2", &serde_json::json!({}))
            .unwrap();

        let future = chrono::Utc::now() + Duration::minutes(10);

        assert_eq!(store.count_pending_tasks_for_session("s1").unwrap(), 0);

        store.schedule_task("s1", "u1", future, "a").unwrap();
        store.schedule_task("s1", "u1", future, "b").unwrap();
        store.schedule_task("s2", "u2", future, "c").unwrap();

        assert_eq!(store.count_pending_tasks_for_session("s1").unwrap(), 2);
        assert_eq!(store.count_pending_tasks_for_session("s2").unwrap(), 1);
    }
}
