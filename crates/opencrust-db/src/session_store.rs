use opencrust_common::{Error, Result};
use rusqlite::Connection;
use rusqlite::params;
use std::path::Path;
use tracing::{info, warn};

use crate::migrations::{USAGE_SCHEMA_V1, USAGE_SCHEMA_V2_COLUMNS, USAGE_SCHEMA_V2_INDEX_SQL};

/// Persisted message row loaded from the session store.
#[derive(Debug, Clone)]
pub struct StoredMessage {
    pub direction: String,
    pub content: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub metadata: serde_json::Value,
}

/// Aggregated token usage statistics.
#[derive(Debug, Clone, Default)]
pub struct UsageRecord {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

/// Attribution data for a token usage record.
#[derive(Debug, Clone)]
pub struct UsageAttribution<'a> {
    pub user_id: &'a str,
    pub channel_id: &'a str,
    pub provider: &'a str,
    pub model: &'a str,
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

        // Apply versioned migrations (idempotent: CREATE TABLE IF NOT EXISTS)
        self.conn
            .execute_batch(USAGE_SCHEMA_V1.sql)
            .map_err(|e| Error::Database(format!("usage migration failed: {e}")))?;

        // v2: add user_id and channel_id to usage_log (idempotent)
        for (col, col_type) in USAGE_SCHEMA_V2_COLUMNS {
            let sql = format!("ALTER TABLE usage_log ADD COLUMN {col} {col_type}");
            if let Err(e) = self.conn.execute(&sql, [])
                && !e.to_string().contains("duplicate column")
            {
                return Err(Error::Database(format!("usage v2 migration failed: {e}")));
            }
        }
        self.conn
            .execute_batch(USAGE_SCHEMA_V2_INDEX_SQL)
            .map_err(|e| Error::Database(format!("usage v2 index migration failed: {e}")))?;

        // Idempotent column additions for scheduling overhaul
        let columns = [
            ("retry_count", "INTEGER DEFAULT 0"),
            ("max_retries", "INTEGER DEFAULT 3"),
            ("next_retry_at", "TEXT"),
            ("heartbeat_depth", "INTEGER DEFAULT 0"),
            ("recurrence_type", "TEXT"),
            ("recurrence_value", "TEXT"),
            ("recurrence_end_at", "TEXT"),
            ("deliver_to_channel", "TEXT"),
            ("timezone", "TEXT"),
        ];
        for (col, col_type) in &columns {
            let sql = format!("ALTER TABLE scheduled_tasks ADD COLUMN {col} {col_type}");
            // Ignore "duplicate column" errors - column already exists
            if let Err(e) = self.conn.execute(&sql, []) {
                let msg = e.to_string();
                if !msg.contains("duplicate column") {
                    return Err(Error::Database(format!("migration failed: {e}")));
                }
            }
        }

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
                "SELECT t.id, t.session_id, s.channel_id, t.user_id, t.execute_at, t.payload,
                        s.metadata, t.retry_count, t.max_retries, t.heartbeat_depth,
                        t.recurrence_type, t.recurrence_value, t.recurrence_end_at,
                        t.deliver_to_channel, t.timezone
                 FROM scheduled_tasks t
                 JOIN sessions s ON t.session_id = s.id
                 WHERE t.status = 'pending'
                   AND datetime(COALESCE(t.next_retry_at, t.execute_at)) <= datetime('now')
                 ORDER BY t.execute_at ASC
                 LIMIT 10",
            )
            .map_err(|e| Error::Database(format!("failed to prepare poll query: {e}")))?;

        let rows = stmt
            .query_map([], |row| {
                let execute_at_raw: String = row.get(4)?;
                let metadata_raw: String = row.get(6)?;
                let end_at_raw: Option<String> = row.get(12)?;
                Ok(ScheduledTask {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    channel_id: row.get(2)?,
                    user_id: row.get(3)?,
                    execute_at: parse_timestamp(&execute_at_raw),
                    payload: row.get(5)?,
                    session_metadata: serde_json::from_str(&metadata_raw)
                        .unwrap_or(serde_json::Value::Null),
                    retry_count: row.get::<_, Option<i32>>(7)?.unwrap_or(0),
                    max_retries: row.get::<_, Option<i32>>(8)?.unwrap_or(3),
                    heartbeat_depth: row.get::<_, Option<u8>>(9)?.unwrap_or(0),
                    recurrence_type: row.get(10)?,
                    recurrence_value: row.get(11)?,
                    recurrence_end_at: end_at_raw.map(|s| parse_timestamp(&s)),
                    deliver_to_channel: row.get(13)?,
                    timezone: row.get(14)?,
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
    /// Returns true if the task was still pending and is now completed,
    /// false if it was already cancelled or in another terminal state.
    pub fn complete_task(&self, task_id: &str) -> Result<bool> {
        let rows = self
            .conn
            .execute(
                "UPDATE scheduled_tasks SET status = 'completed' WHERE id = ?1 AND status = 'pending'",
                params![task_id],
            )
            .map_err(|e| Error::Database(format!("failed to complete task: {e}")))?;
        Ok(rows > 0)
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

    /// Retry a failed task with exponential backoff, or mark it as permanently failed.
    /// Backoff schedule: 30s, 60s, 120s, 240s (doubles each retry).
    pub fn retry_or_fail_task(&self, task_id: &str) -> Result<bool> {
        let (retry_count, max_retries): (i32, i32) = self
            .conn
            .query_row(
                "SELECT COALESCE(retry_count, 0), COALESCE(max_retries, 3) FROM scheduled_tasks WHERE id = ?1",
                params![task_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|e| Error::Database(format!("failed to read retry state: {e}")))?;

        let new_count = retry_count + 1;
        if new_count > max_retries {
            self.fail_task(task_id)?;
            return Ok(false);
        }

        let backoff_secs = 30i64 * (1 << retry_count.min(7));
        let next_retry = chrono::Utc::now() + chrono::Duration::seconds(backoff_secs);

        self.conn
            .execute(
                "UPDATE scheduled_tasks SET retry_count = ?1, next_retry_at = ?2 WHERE id = ?3",
                params![new_count, next_retry.to_rfc3339(), task_id],
            )
            .map_err(|e| Error::Database(format!("failed to set retry: {e}")))?;
        Ok(true)
    }

    /// Cancel a pending task, scoped to a session for safety.
    pub fn cancel_task(&self, task_id: &str, session_id: &str) -> Result<bool> {
        let rows = self
            .conn
            .execute(
                "UPDATE scheduled_tasks SET status = 'cancelled' WHERE id = ?1 AND session_id = ?2 AND status = 'pending'",
                params![task_id, session_id],
            )
            .map_err(|e| Error::Database(format!("failed to cancel task: {e}")))?;
        Ok(rows > 0)
    }

    /// List pending tasks for a session, ordered by execute_at.
    pub fn list_pending_tasks(&self, session_id: &str) -> Result<Vec<ScheduledTask>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT t.id, t.session_id, s.channel_id, t.user_id, t.execute_at, t.payload,
                        s.metadata, t.retry_count, t.max_retries, t.heartbeat_depth,
                        t.recurrence_type, t.recurrence_value, t.recurrence_end_at,
                        t.deliver_to_channel, t.timezone
                 FROM scheduled_tasks t
                 JOIN sessions s ON t.session_id = s.id
                 WHERE t.session_id = ?1 AND t.status = 'pending'
                 ORDER BY t.execute_at ASC",
            )
            .map_err(|e| Error::Database(format!("failed to prepare list query: {e}")))?;

        let rows = stmt
            .query_map(params![session_id], |row| {
                let execute_at_raw: String = row.get(4)?;
                let metadata_raw: String = row.get(6)?;
                let end_at_raw: Option<String> = row.get(12)?;
                Ok(ScheduledTask {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    channel_id: row.get(2)?,
                    user_id: row.get(3)?,
                    execute_at: parse_timestamp(&execute_at_raw),
                    payload: row.get(5)?,
                    session_metadata: serde_json::from_str(&metadata_raw)
                        .unwrap_or(serde_json::Value::Null),
                    retry_count: row.get::<_, Option<i32>>(7)?.unwrap_or(0),
                    max_retries: row.get::<_, Option<i32>>(8)?.unwrap_or(3),
                    heartbeat_depth: row.get::<_, Option<u8>>(9)?.unwrap_or(0),
                    recurrence_type: row.get(10)?,
                    recurrence_value: row.get(11)?,
                    recurrence_end_at: end_at_raw.map(|s| parse_timestamp(&s)),
                    deliver_to_channel: row.get(13)?,
                    timezone: row.get(14)?,
                })
            })
            .map_err(|e| Error::Database(format!("failed to list tasks: {e}")))?;

        let mut tasks = Vec::new();
        for row in rows {
            tasks.push(row.map_err(|e| Error::Database(format!("failed to read task row: {e}")))?);
        }
        Ok(tasks)
    }

    /// Delete sessions that have been inactive for more than `inactive_days` days,
    /// along with all their associated messages. Returns the number of sessions deleted.
    pub fn cleanup_stale_sessions(&self, inactive_days: i64) -> Result<usize> {
        let interval = format!("-{inactive_days} days");
        self.conn
            .execute(
                "DELETE FROM messages WHERE session_id IN (
                     SELECT id FROM sessions WHERE updated_at < datetime('now', ?1)
                 )",
                params![interval],
            )
            .map_err(|e| Error::Database(format!("failed to cleanup session messages: {e}")))?;
        let deleted = self
            .conn
            .execute(
                "DELETE FROM sessions WHERE updated_at < datetime('now', ?1)",
                params![interval],
            )
            .map_err(|e| Error::Database(format!("failed to cleanup stale sessions: {e}")))?;
        Ok(deleted)
    }

    /// Delete completed, failed, and cancelled tasks older than `older_than_days`.
    /// Returns the number of deleted rows.
    pub fn cleanup_completed_tasks(&self, older_than_days: i64) -> Result<usize> {
        let deleted = self
            .conn
            .execute(
                "DELETE FROM scheduled_tasks
                 WHERE status IN ('completed', 'failed', 'cancelled')
                   AND created_at < datetime('now', ?1)",
                params![format!("-{older_than_days} days")],
            )
            .map_err(|e| Error::Database(format!("failed to cleanup tasks: {e}")))?;
        Ok(deleted)
    }

    /// Schedule a task with full options (recurrence, heartbeat depth, cross-channel delivery).
    #[allow(clippy::too_many_arguments)]
    pub fn schedule_task_full(
        &self,
        session_id: &str,
        user_id: &str,
        execute_at: chrono::DateTime<chrono::Utc>,
        payload: &str,
        heartbeat_depth: u8,
        recurrence_type: Option<&str>,
        recurrence_value: Option<&str>,
        recurrence_end_at: Option<chrono::DateTime<chrono::Utc>>,
        deliver_to_channel: Option<&str>,
        timezone: Option<&str>,
    ) -> Result<String> {
        let task_id = uuid::Uuid::new_v4().to_string();
        let end_at_str = recurrence_end_at.map(|dt| dt.to_rfc3339());
        self.conn
            .execute(
                "INSERT INTO scheduled_tasks (id, session_id, user_id, execute_at, payload, status,
                    heartbeat_depth, recurrence_type, recurrence_value, recurrence_end_at,
                    deliver_to_channel, timezone)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    task_id,
                    session_id,
                    user_id,
                    execute_at.to_rfc3339(),
                    payload,
                    heartbeat_depth,
                    recurrence_type,
                    recurrence_value,
                    end_at_str,
                    deliver_to_channel,
                    timezone,
                ],
            )
            .map_err(|e| Error::Database(format!("failed to schedule task: {e}")))?;
        Ok(task_id)
    }

    /// Record token usage for a completed agent turn.
    pub fn record_usage(
        &self,
        session_id: &str,
        attribution: UsageAttribution<'_>,
        input_tokens: u32,
        output_tokens: u32,
    ) -> Result<()> {
        let id = uuid::Uuid::new_v4().to_string();
        self.conn
            .execute(
                "INSERT INTO usage_log
                     (id, session_id, user_id, channel_id, provider, model, input_tokens, output_tokens)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    id,
                    session_id,
                    attribution.user_id,
                    attribution.channel_id,
                    attribution.provider,
                    attribution.model,
                    input_tokens,
                    output_tokens
                ],
            )
            .map_err(|e| Error::Database(format!("failed to record usage: {e}")))?;
        Ok(())
    }

    /// Query aggregated token usage for a specific user.
    ///
    /// - `period`: `"today"`, `"week"`, `"month"`, or `None` (all time).
    pub fn query_usage_for_user(&self, user_id: &str, period: Option<&str>) -> Result<UsageRecord> {
        let date_filter = match period {
            Some("today") => " AND date(recorded_at) = date('now')",
            Some("week") => " AND recorded_at >= datetime('now', '-7 days')",
            Some("month") => " AND recorded_at >= datetime('now', '-30 days')",
            _ => "",
        };
        let sql = format!(
            "SELECT COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0), \
             COALESCE(SUM(input_tokens+output_tokens),0) \
             FROM usage_log WHERE user_id = ?1{date_filter}"
        );
        let row: (i64, i64, i64) = self
            .conn
            .query_row(&sql, params![user_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .map_err(|e| Error::Database(format!("failed to query user usage: {e}")))?;
        Ok(UsageRecord {
            input_tokens: row.0 as u64,
            output_tokens: row.1 as u64,
            total_tokens: row.2 as u64,
        })
    }

    /// Query aggregated token usage.
    ///
    /// - `session_id`: when `Some`, restrict to that session; otherwise aggregate all sessions.
    /// - `period`: one of `"today"`, `"week"`, `"month"`, or `None` (all time).
    pub fn query_usage(
        &self,
        session_id: Option<&str>,
        period: Option<&str>,
    ) -> Result<UsageRecord> {
        let date_filter = match period {
            Some("today") => " AND date(recorded_at) = date('now')",
            Some("week") => " AND recorded_at >= datetime('now', '-7 days')",
            Some("month") => " AND recorded_at >= datetime('now', '-30 days')",
            _ => "",
        };

        let (sql, params_vec): (String, Vec<String>) = if let Some(sid) = session_id {
            (
                format!(
                    "SELECT COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0), \
                     COALESCE(SUM(input_tokens+output_tokens),0) \
                     FROM usage_log WHERE session_id = ?1{date_filter}"
                ),
                vec![sid.to_string()],
            )
        } else {
            (
                format!(
                    "SELECT COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0), \
                     COALESCE(SUM(input_tokens+output_tokens),0) \
                     FROM usage_log WHERE 1=1{date_filter}"
                ),
                vec![],
            )
        };

        let row: (i64, i64, i64) = if params_vec.is_empty() {
            self.conn
                .query_row(&sql, [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                .map_err(|e| Error::Database(format!("failed to query usage: {e}")))?
        } else {
            self.conn
                .query_row(&sql, params![params_vec[0]], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })
                .map_err(|e| Error::Database(format!("failed to query usage: {e}")))?
        };

        Ok(UsageRecord {
            input_tokens: row.0 as u64,
            output_tokens: row.1 as u64,
            total_tokens: row.2 as u64,
        })
    }

    /// After completing a recurring task, schedule the next occurrence.
    /// Returns the new task ID if rescheduled, None if the chain is done.
    pub fn reschedule_recurring_task(&self, task: &ScheduledTask) -> Result<Option<String>> {
        let (rec_type, rec_value) = match (&task.recurrence_type, &task.recurrence_value) {
            (Some(t), Some(v)) => (t.as_str(), v.as_str()),
            _ => return Ok(None),
        };

        let now = chrono::Utc::now();

        let next_at = match rec_type {
            "interval" => {
                let secs: i64 = rec_value
                    .parse()
                    .map_err(|_| Error::Database("invalid interval value".to_string()))?;
                let interval = chrono::Duration::seconds(secs);
                // Anchor to previous execute_at to prevent drift; skip forward if behind
                let mut candidate = task.execute_at + interval;
                while candidate <= now {
                    candidate += interval;
                }
                candidate
            }
            "cron" => {
                use std::str::FromStr;
                let schedule = cron::Schedule::from_str(rec_value)
                    .map_err(|e| Error::Database(format!("invalid cron expression: {e}")))?;
                let tz: chrono_tz::Tz = task
                    .timezone
                    .as_deref()
                    .unwrap_or("UTC")
                    .parse()
                    .unwrap_or_else(|_| {
                        warn!(
                            "invalid timezone '{}' in task {}, using UTC",
                            task.timezone.as_deref().unwrap_or("?"),
                            task.id
                        );
                        chrono_tz::Tz::UTC
                    });
                match schedule.upcoming(tz).next() {
                    Some(dt) => dt.with_timezone(&chrono::Utc),
                    None => return Ok(None),
                }
            }
            _ => return Ok(None),
        };

        // Check if past end time
        if let Some(end_at) = task.recurrence_end_at
            && next_at > end_at
        {
            return Ok(None);
        }

        self.schedule_task_full(
            &task.session_id,
            &task.user_id,
            next_at,
            &task.payload,
            task.heartbeat_depth,
            task.recurrence_type.as_deref(),
            task.recurrence_value.as_deref(),
            task.recurrence_end_at,
            task.deliver_to_channel.as_deref(),
            task.timezone.as_deref(),
        )
        .map(Some)
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
    pub retry_count: i32,
    pub max_retries: i32,
    pub heartbeat_depth: u8,
    pub recurrence_type: Option<String>,
    pub recurrence_value: Option<String>,
    pub recurrence_end_at: Option<chrono::DateTime<chrono::Utc>>,
    pub deliver_to_channel: Option<String>,
    pub timezone: Option<String>,
}

fn parse_timestamp(value: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .unwrap_or_else(|e| {
            warn!(
                "failed to parse timestamp '{}': {e}, falling back to now",
                value
            );
            chrono::Utc::now()
        })
}

#[cfg(test)]
mod tests {
    use super::{ScheduledTask, SessionStore, UsageAttribution};
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

        let completed = store
            .complete_task(&task_id)
            .expect("complete should succeed");
        assert!(completed, "pending task should be marked completed");

        let due_after = store
            .poll_due_tasks()
            .expect("poll after complete should succeed");
        assert_eq!(due_after.len(), 0);

        // Completing again should return false (already completed)
        let re_completed = store.complete_task(&task_id).unwrap();
        assert!(!re_completed, "already-completed task should return false");
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

    #[test]
    fn retry_or_fail_task_retries_then_fails() {
        let store = SessionStore::in_memory().expect("in-memory store should open");
        store
            .upsert_session("s1", "web", "u1", &serde_json::json!({}))
            .unwrap();

        let due_time = chrono::Utc::now() - Duration::minutes(1);
        let task_id = store.schedule_task("s1", "u1", due_time, "flaky").unwrap();

        // First 3 retries should return true (task stays pending with future next_retry_at)
        for i in 0..3 {
            let retried = store.retry_or_fail_task(&task_id).unwrap();
            assert!(retried, "retry {} should succeed", i + 1);
        }

        // 4th attempt exceeds max_retries (default 3), should return false
        let retried = store.retry_or_fail_task(&task_id).unwrap();
        assert!(!retried, "should be permanently failed now");

        // Task should not appear in poll
        let due = store.poll_due_tasks().unwrap();
        assert!(due.is_empty());
    }

    #[test]
    fn cancel_task_scoped_to_session() {
        let store = SessionStore::in_memory().expect("in-memory store should open");
        store
            .upsert_session("s1", "web", "u1", &serde_json::json!({}))
            .unwrap();
        store
            .upsert_session("s2", "web", "u2", &serde_json::json!({}))
            .unwrap();

        let future = chrono::Utc::now() + Duration::minutes(10);
        let task_id = store
            .schedule_task("s1", "u1", future, "cancel me")
            .unwrap();

        // Cancel from wrong session should fail
        assert!(!store.cancel_task(&task_id, "s2").unwrap());
        assert_eq!(store.count_pending_tasks_for_session("s1").unwrap(), 1);

        // Cancel from correct session should succeed
        assert!(store.cancel_task(&task_id, "s1").unwrap());
        assert_eq!(store.count_pending_tasks_for_session("s1").unwrap(), 0);

        // Double cancel should return false (already cancelled)
        assert!(!store.cancel_task(&task_id, "s1").unwrap());
    }

    #[test]
    fn list_pending_tasks_returns_ordered() {
        let store = SessionStore::in_memory().expect("in-memory store should open");
        store
            .upsert_session("s1", "web", "u1", &serde_json::json!({}))
            .unwrap();

        let now = chrono::Utc::now();
        store
            .schedule_task("s1", "u1", now + Duration::minutes(5), "later")
            .unwrap();
        store
            .schedule_task("s1", "u1", now + Duration::minutes(1), "sooner")
            .unwrap();

        let tasks = store.list_pending_tasks("s1").unwrap();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].payload, "sooner");
        assert_eq!(tasks[1].payload, "later");
    }

    #[test]
    fn schedule_task_full_stores_all_fields() {
        let store = SessionStore::in_memory().expect("in-memory store should open");
        store
            .upsert_session("s1", "web", "u1", &serde_json::json!({}))
            .unwrap();

        let execute_at = chrono::Utc::now() + Duration::minutes(5);
        let end_at = chrono::Utc::now() + Duration::hours(24);

        let task_id = store
            .schedule_task_full(
                "s1",
                "u1",
                execute_at,
                "recurring check",
                2,
                Some("interval"),
                Some("300"),
                Some(end_at),
                Some("telegram"),
                Some("America/New_York"),
            )
            .unwrap();

        let tasks = store.list_pending_tasks("s1").unwrap();
        assert_eq!(tasks.len(), 1);
        let task = &tasks[0];
        assert_eq!(task.id, task_id);
        assert_eq!(task.heartbeat_depth, 2);
        assert_eq!(task.recurrence_type.as_deref(), Some("interval"));
        assert_eq!(task.recurrence_value.as_deref(), Some("300"));
        assert!(task.recurrence_end_at.is_some());
        assert_eq!(task.deliver_to_channel.as_deref(), Some("telegram"));
        assert_eq!(task.timezone.as_deref(), Some("America/New_York"));
    }

    #[test]
    fn reschedule_recurring_interval_task() {
        let store = SessionStore::in_memory().expect("in-memory store should open");
        store
            .upsert_session("s1", "web", "u1", &serde_json::json!({}))
            .unwrap();

        let past = chrono::Utc::now() - Duration::minutes(1);
        let task = ScheduledTask {
            id: "task-1".to_string(),
            session_id: "s1".to_string(),
            channel_id: "web".to_string(),
            user_id: "u1".to_string(),
            execute_at: past,
            payload: "interval task".to_string(),
            session_metadata: serde_json::json!({}),
            retry_count: 0,
            max_retries: 3,
            heartbeat_depth: 1,
            recurrence_type: Some("interval".to_string()),
            recurrence_value: Some("60".to_string()),
            recurrence_end_at: None,
            deliver_to_channel: None,
            timezone: None,
        };

        let new_id = store
            .reschedule_recurring_task(&task)
            .unwrap()
            .expect("should reschedule");

        let tasks = store.list_pending_tasks("s1").unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, new_id);
        assert_eq!(tasks[0].recurrence_type.as_deref(), Some("interval"));
        assert_eq!(tasks[0].recurrence_value.as_deref(), Some("60"));
        assert_eq!(tasks[0].heartbeat_depth, 1);
    }

    #[test]
    fn reschedule_non_recurring_returns_none() {
        let store = SessionStore::in_memory().expect("in-memory store should open");
        let task = ScheduledTask {
            id: "task-1".to_string(),
            session_id: "s1".to_string(),
            channel_id: "web".to_string(),
            user_id: "u1".to_string(),
            execute_at: chrono::Utc::now(),
            payload: "one-shot".to_string(),
            session_metadata: serde_json::json!({}),
            retry_count: 0,
            max_retries: 3,
            heartbeat_depth: 0,
            recurrence_type: None,
            recurrence_value: None,
            recurrence_end_at: None,
            deliver_to_channel: None,
            timezone: None,
        };

        assert!(store.reschedule_recurring_task(&task).unwrap().is_none());
    }

    #[test]
    fn reschedule_stops_after_end_at() {
        let store = SessionStore::in_memory().expect("in-memory store should open");
        store
            .upsert_session("s1", "web", "u1", &serde_json::json!({}))
            .unwrap();

        let task = ScheduledTask {
            id: "task-1".to_string(),
            session_id: "s1".to_string(),
            channel_id: "web".to_string(),
            user_id: "u1".to_string(),
            execute_at: chrono::Utc::now(),
            payload: "expiring".to_string(),
            session_metadata: serde_json::json!({}),
            retry_count: 0,
            max_retries: 3,
            heartbeat_depth: 0,
            recurrence_type: Some("interval".to_string()),
            recurrence_value: Some("3600".to_string()),
            // End time in the past - next occurrence would be past it
            recurrence_end_at: Some(chrono::Utc::now() - Duration::minutes(1)),
            deliver_to_channel: None,
            timezone: None,
        };

        assert!(store.reschedule_recurring_task(&task).unwrap().is_none());
    }

    #[test]
    fn poll_due_tasks_respects_next_retry_at() {
        let store = SessionStore::in_memory().expect("in-memory store should open");
        store
            .upsert_session("s1", "web", "u1", &serde_json::json!({}))
            .unwrap();

        let past = chrono::Utc::now() - Duration::minutes(1);
        let task_id = store.schedule_task("s1", "u1", past, "retry me").unwrap();

        // Initially due
        assert_eq!(store.poll_due_tasks().unwrap().len(), 1);

        // Set retry - next_retry_at in the future
        store.retry_or_fail_task(&task_id).unwrap();

        // Should NOT be due (next_retry_at is 30+ seconds in the future)
        assert_eq!(store.poll_due_tasks().unwrap().len(), 0);
    }

    #[test]
    fn cleanup_completed_tasks_deletes_old_rows() {
        let store = SessionStore::in_memory().expect("in-memory store should open");
        store
            .upsert_session("s1", "web", "u1", &serde_json::json!({}))
            .unwrap();

        let past = chrono::Utc::now() - Duration::minutes(1);
        let task_id = store.schedule_task("s1", "u1", past, "done").unwrap();
        store.complete_task(&task_id).unwrap();

        // Backdate the created_at to 8 days ago so it qualifies for 7-day cleanup
        store
            .connection()
            .execute(
                "UPDATE scheduled_tasks SET created_at = datetime('now', '-8 days') WHERE id = ?1",
                rusqlite::params![task_id],
            )
            .unwrap();

        let deleted = store.cleanup_completed_tasks(7).unwrap();
        assert_eq!(deleted, 1);

        // Pending tasks should not be affected
        let task_id2 = store
            .schedule_task(
                "s1",
                "u1",
                chrono::Utc::now() + Duration::hours(1),
                "pending",
            )
            .unwrap();
        store
            .connection()
            .execute(
                "UPDATE scheduled_tasks SET created_at = datetime('now', '-8 days') WHERE id = ?1",
                rusqlite::params![task_id2],
            )
            .unwrap();

        let deleted2 = store.cleanup_completed_tasks(7).unwrap();
        assert_eq!(deleted2, 0);
    }

    #[test]
    fn cleanup_stale_sessions_deletes_inactive_sessions() {
        let store = SessionStore::in_memory().expect("in-memory store should open");
        store
            .upsert_session("s1", "web", "u1", &serde_json::json!({}))
            .unwrap();
        store
            .upsert_session("s2", "web", "u2", &serde_json::json!({}))
            .unwrap();
        // Add a message to s1 to verify cascade delete
        store
            .append_message(
                "s1",
                "user",
                "hello",
                chrono::Utc::now(),
                &serde_json::json!({}),
            )
            .unwrap();

        // Backdate s1 to 91 days ago so it qualifies for 90-day cleanup
        store
            .connection()
            .execute(
                "UPDATE sessions SET updated_at = datetime('now', '-91 days') WHERE id = 's1'",
                [],
            )
            .unwrap();

        let deleted = store.cleanup_stale_sessions(90).unwrap();
        assert_eq!(deleted, 1); // s1 deleted, s2 kept

        // Messages belonging to s1 should also be gone
        let msg_count: i64 = store
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE session_id = 's1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(msg_count, 0);

        // s2 should still exist
        let session_count: i64 = store
            .connection()
            .query_row("SELECT COUNT(*) FROM sessions WHERE id = 's2'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(session_count, 1);
    }

    #[test]
    fn record_and_query_usage_all_time() {
        let store = SessionStore::in_memory().expect("in-memory store");
        store
            .record_usage(
                "s1",
                UsageAttribution {
                    user_id: "user1",
                    channel_id: "test",
                    provider: "anthropic",
                    model: "claude-sonnet",
                },
                100,
                50,
            )
            .expect("record_usage should succeed");
        store
            .record_usage(
                "s1",
                UsageAttribution {
                    user_id: "user1",
                    channel_id: "test",
                    provider: "anthropic",
                    model: "claude-sonnet",
                },
                200,
                80,
            )
            .expect("record_usage should succeed");
        store
            .record_usage(
                "s2",
                UsageAttribution {
                    user_id: "user2",
                    channel_id: "test",
                    provider: "openai",
                    model: "gpt-4o",
                },
                300,
                100,
            )
            .expect("record_usage should succeed");

        // Query all sessions
        let all = store.query_usage(None, None).expect("query_usage");
        assert_eq!(all.input_tokens, 600);
        assert_eq!(all.output_tokens, 230);
        assert_eq!(all.total_tokens, 830);

        // Query per session
        let s1 = store.query_usage(Some("s1"), None).expect("query_usage s1");
        assert_eq!(s1.input_tokens, 300);
        assert_eq!(s1.output_tokens, 130);

        let s2 = store.query_usage(Some("s2"), None).expect("query_usage s2");
        assert_eq!(s2.input_tokens, 300);
        assert_eq!(s2.output_tokens, 100);
    }

    #[test]
    fn query_usage_empty_returns_zeros() {
        let store = SessionStore::in_memory().expect("in-memory store");
        let result = store.query_usage(None, None).expect("query_usage");
        assert_eq!(result.input_tokens, 0);
        assert_eq!(result.output_tokens, 0);
        assert_eq!(result.total_tokens, 0);
    }

    #[test]
    fn record_usage_stores_user_and_channel() {
        let store = SessionStore::in_memory().expect("in-memory store");
        store
            .record_usage(
                "s1",
                UsageAttribution {
                    user_id: "alice",
                    channel_id: "telegram",
                    provider: "anthropic",
                    model: "claude",
                },
                100,
                50,
            )
            .expect("record_usage");
        store
            .record_usage(
                "s2",
                UsageAttribution {
                    user_id: "alice",
                    channel_id: "discord",
                    provider: "anthropic",
                    model: "claude",
                },
                200,
                100,
            )
            .expect("record_usage");
        store
            .record_usage(
                "s3",
                UsageAttribution {
                    user_id: "bob",
                    channel_id: "telegram",
                    provider: "anthropic",
                    model: "claude",
                },
                300,
                150,
            )
            .expect("record_usage");

        let alice = store
            .query_usage_for_user("alice", None)
            .expect("query alice");
        assert_eq!(alice.input_tokens, 300);
        assert_eq!(alice.output_tokens, 150);
        assert_eq!(alice.total_tokens, 450);

        let bob = store.query_usage_for_user("bob", None).expect("query bob");
        assert_eq!(bob.input_tokens, 300);
        assert_eq!(bob.total_tokens, 450);
    }

    #[test]
    fn query_usage_for_user_unknown_returns_zeros() {
        let store = SessionStore::in_memory().expect("in-memory store");
        let result = store
            .query_usage_for_user("nobody", None)
            .expect("query nobody");
        assert_eq!(result.total_tokens, 0);
    }
}
