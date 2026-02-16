use async_trait::async_trait;
use chrono::{DateTime, NaiveDateTime, Utc};
use opencrust_common::{Error, Result};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use tracing::info;
use uuid::Uuid;

use crate::migrations::MEMORY_SCHEMA_V1;

const DEFAULT_RECALL_LIMIT: usize = 20;
const MAX_RECALL_LIMIT: usize = 200;

/// Persisted memory entry used for retrieval and context assembly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: String,
    pub session_id: String,
    pub channel_id: Option<String>,
    pub user_id: Option<String>,
    /// Logical continuity bucket for shared memory across channels/sessions.
    pub continuity_key: Option<String>,
    pub role: MemoryRole,
    pub content: String,
    pub embedding: Option<Vec<f32>>,
    pub embedding_model: Option<String>,
    pub embedding_dimensions: Option<usize>,
    pub metadata: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

/// Insert shape for new memory records before persistence assigns ID/timestamps.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewMemoryEntry {
    pub session_id: String,
    pub channel_id: Option<String>,
    pub user_id: Option<String>,
    pub continuity_key: Option<String>,
    pub role: MemoryRole,
    pub content: String,
    pub embedding: Option<Vec<f32>>,
    pub embedding_model: Option<String>,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryRole {
    User,
    Assistant,
    System,
    Tool,
}

impl MemoryRole {
    fn as_str(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::System => "system",
            Self::Tool => "tool",
        }
    }

    fn from_db(value: &str) -> Result<Self> {
        match value {
            "user" => Ok(Self::User),
            "assistant" => Ok(Self::Assistant),
            "system" => Ok(Self::System),
            "tool" => Ok(Self::Tool),
            other => Err(Error::Database(format!("unknown memory role: {other}"))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallQuery {
    pub query_text: Option<String>,
    pub query_embedding: Option<Vec<f32>>,
    pub session_id: Option<String>,
    pub continuity_key: Option<String>,
    pub limit: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionContext {
    pub session_id: String,
    pub entries: Vec<MemoryEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionReport {
    pub deleted_entries: usize,
    pub before: DateTime<Utc>,
}

#[async_trait]
pub trait MemoryProvider: Send + Sync {
    async fn remember(&self, entry: NewMemoryEntry) -> Result<String>;
    async fn recall(&self, query: RecallQuery) -> Result<Vec<MemoryEntry>>;
    async fn get_session_context(&self, session_id: &str, limit: usize) -> Result<SessionContext>;
    async fn get_continuity_context(
        &self,
        continuity_key: &str,
        limit: usize,
    ) -> Result<Vec<MemoryEntry>>;
    async fn compact(&self, before: DateTime<Utc>) -> Result<CompactionReport>;
    async fn delete_session_memory(&self, session_id: &str) -> Result<usize>;
}

/// Backing store for long-term and session-scoped memory data.
pub struct MemoryStore {
    conn: Mutex<Connection>,
}

impl MemoryStore {
    pub fn open(db_path: &Path) -> Result<Self> {
        info!("opening memory store at {}", db_path.display());
        let conn = Connection::open(db_path)
            .map_err(|e| Error::Database(format!("failed to open memory database: {e}")))?;

        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .map_err(|e| Error::Database(format!("failed to set pragmas: {e}")))?;

        let store = Self {
            conn: Mutex::new(conn),
        };
        store.run_migrations()?;
        Ok(store)
    }

    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()
            .map_err(|e| Error::Database(format!("failed to open in-memory database: {e}")))?;

        let store = Self {
            conn: Mutex::new(conn),
        };
        store.run_migrations()?;
        Ok(store)
    }

    fn run_migrations(&self) -> Result<()> {
        let conn = self.connection()?;
        conn.execute_batch(MEMORY_SCHEMA_V1.sql)
            .map_err(|e| Error::Database(format!("memory migration failed: {e}")))?;

        Ok(())
    }

    fn connection(&self) -> Result<MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| Error::Database("memory database lock poisoned".into()))
    }

    pub async fn remember(&self, entry: NewMemoryEntry) -> Result<String> {
        self.remember_sync(entry)
    }

    pub async fn recall(&self, query: RecallQuery) -> Result<Vec<MemoryEntry>> {
        self.recall_sync(query)
    }

    pub async fn get_session_context(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<SessionContext> {
        let entries = self.recent_entries_sync(Some(session_id), None, limit)?;
        Ok(SessionContext {
            session_id: session_id.to_string(),
            entries,
        })
    }

    pub async fn get_continuity_context(
        &self,
        continuity_key: &str,
        limit: usize,
    ) -> Result<Vec<MemoryEntry>> {
        self.recent_entries_sync(None, Some(continuity_key), limit)
    }

    pub async fn compact(&self, before: DateTime<Utc>) -> Result<CompactionReport> {
        let conn = self.connection()?;
        let deleted = conn
            .execute(
                "DELETE FROM memory_entries WHERE datetime(created_at) < datetime(?)",
                params![before.to_rfc3339()],
            )
            .map_err(|e| Error::Database(format!("failed to compact memory entries: {e}")))?;

        Ok(CompactionReport {
            deleted_entries: deleted,
            before,
        })
    }

    pub async fn delete_session_memory(&self, session_id: &str) -> Result<usize> {
        let conn = self.connection()?;
        conn.execute(
            "DELETE FROM memory_entries WHERE session_id = ?",
            params![session_id],
        )
        .map_err(|e| Error::Database(format!("failed to delete session memory: {e}")))
    }

    fn remember_sync(&self, entry: NewMemoryEntry) -> Result<String> {
        if entry.content.trim().is_empty() {
            return Err(Error::Database("memory content cannot be empty".into()));
        }

        let id = Uuid::new_v4().to_string();
        let created_at = Utc::now().to_rfc3339();
        let embedding_blob = entry.embedding.as_ref().map(|e| embedding_to_blob(e));
        let embedding_dimensions = entry.embedding.as_ref().map(|e| e.len() as i64);
        let metadata_json = serde_json::to_string(&entry.metadata)
            .map_err(|e| Error::Database(format!("failed to serialize memory metadata: {e}")))?;

        let conn = self.connection()?;
        conn.execute(
            "INSERT INTO memory_entries (
                id, session_id, channel_id, user_id, continuity_key, role, content,
                embedding, embedding_model, embedding_dimensions, metadata, created_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                id,
                entry.session_id,
                entry.channel_id,
                entry.user_id,
                entry.continuity_key,
                entry.role.as_str(),
                entry.content,
                embedding_blob,
                entry.embedding_model,
                embedding_dimensions,
                metadata_json,
                created_at,
            ],
        )
        .map_err(|e| Error::Database(format!("failed to insert memory entry: {e}")))?;

        Ok(id)
    }

    fn recall_sync(&self, query: RecallQuery) -> Result<Vec<MemoryEntry>> {
        let limit = clamp_limit(query.limit);
        let candidates = self.query_candidates_sync(
            query.session_id.as_deref(),
            query.continuity_key.as_deref(),
            query.query_text.as_deref(),
            limit.saturating_mul(4),
        )?;

        if candidates.is_empty() {
            return Ok(Vec::new());
        }

        let mut scored: Vec<(f32, MemoryEntry)> = candidates
            .into_iter()
            .map(|entry| {
                let semantic_score = match (&query.query_embedding, &entry.embedding) {
                    (Some(needle), Some(candidate)) => cosine_similarity(needle, candidate),
                    _ => 0.0,
                };
                let text_score = text_match_score(query.query_text.as_deref(), &entry.content);
                let recency_score = recency_score(entry.created_at);

                let score = if query.query_embedding.is_some() {
                    semantic_score * 0.7 + text_score * 0.2 + recency_score * 0.1
                } else {
                    text_score * 0.7 + recency_score * 0.3
                };

                (score, entry)
            })
            .collect();

        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(Ordering::Equal));

        Ok(scored
            .into_iter()
            .take(limit)
            .map(|(_, entry)| entry)
            .collect())
    }

    fn recent_entries_sync(
        &self,
        session_id: Option<&str>,
        continuity_key: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MemoryEntry>> {
        self.query_candidates_sync(session_id, continuity_key, None, limit)
    }

    fn query_candidates_sync(
        &self,
        session_id: Option<&str>,
        continuity_key: Option<&str>,
        query_text: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MemoryEntry>> {
        let query_limit = clamp_limit(limit).min(MAX_RECALL_LIMIT) as i64;
        let conn = self.connection()?;

        let mut stmt = conn
            .prepare(
                "SELECT id, session_id, channel_id, user_id, continuity_key, role, content,
                        embedding, embedding_model, embedding_dimensions, metadata, created_at
                 FROM memory_entries
                 WHERE (?1 IS NULL OR session_id = ?1)
                   AND (?2 IS NULL OR continuity_key = ?2)
                   AND (?3 IS NULL OR lower(content) LIKE '%' || lower(?3) || '%')
                 ORDER BY datetime(created_at) DESC
                 LIMIT ?4",
            )
            .map_err(|e| Error::Database(format!("failed to prepare recall query: {e}")))?;

        let rows = stmt
            .query_map(
                params![session_id, continuity_key, query_text, query_limit],
                row_to_entry,
            )
            .map_err(|e| Error::Database(format!("failed to execute recall query: {e}")))?;

        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| Error::Database(format!("failed to collect recall rows: {e}")))?
            .pipe(Ok)
    }
}

#[async_trait]
impl MemoryProvider for MemoryStore {
    async fn remember(&self, entry: NewMemoryEntry) -> Result<String> {
        self.remember(entry).await
    }

    async fn recall(&self, query: RecallQuery) -> Result<Vec<MemoryEntry>> {
        self.recall(query).await
    }

    async fn get_session_context(&self, session_id: &str, limit: usize) -> Result<SessionContext> {
        self.get_session_context(session_id, limit).await
    }

    async fn get_continuity_context(
        &self,
        continuity_key: &str,
        limit: usize,
    ) -> Result<Vec<MemoryEntry>> {
        self.get_continuity_context(continuity_key, limit).await
    }

    async fn compact(&self, before: DateTime<Utc>) -> Result<CompactionReport> {
        self.compact(before).await
    }

    async fn delete_session_memory(&self, session_id: &str) -> Result<usize> {
        self.delete_session_memory(session_id).await
    }
}

fn row_to_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemoryEntry> {
    let role_str: String = row.get(5)?;
    let role = MemoryRole::from_db(&role_str).map_err(|e| {
        rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::other(e.to_string())))
    })?;

    let embedding_blob: Option<Vec<u8>> = row.get(7)?;
    let embedding = embedding_blob
        .as_deref()
        .map(blob_to_embedding)
        .transpose()
        .map_err(|e| {
            rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::other(e.to_string())))
        })?;

    let metadata_str: String = row.get(10)?;
    let metadata = serde_json::from_str(&metadata_str).unwrap_or(serde_json::Value::Null);

    let created_at_str: String = row.get(11)?;
    let created_at = parse_timestamp(&created_at_str).map_err(|e| {
        rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::other(e.to_string())))
    })?;

    Ok(MemoryEntry {
        id: row.get(0)?,
        session_id: row.get(1)?,
        channel_id: row.get(2)?,
        user_id: row.get(3)?,
        continuity_key: row.get(4)?,
        role,
        content: row.get(6)?,
        embedding,
        embedding_model: row.get(8)?,
        embedding_dimensions: row.get::<_, Option<i64>>(9)?.map(|d| d as usize),
        metadata,
        created_at,
    })
}

fn clamp_limit(limit: usize) -> usize {
    if limit == 0 {
        DEFAULT_RECALL_LIMIT
    } else {
        limit.min(MAX_RECALL_LIMIT)
    }
}

fn parse_timestamp(raw: &str) -> Result<DateTime<Utc>> {
    if let Ok(ts) = DateTime::parse_from_rfc3339(raw) {
        return Ok(ts.with_timezone(&Utc));
    }

    if let Ok(naive) = NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S") {
        return Ok(DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc));
    }

    Err(Error::Database(format!("invalid timestamp format: {raw}")))
}

fn embedding_to_blob(embedding: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(embedding.len() * 4);
    for v in embedding {
        bytes.extend(v.to_le_bytes());
    }
    bytes
}

fn blob_to_embedding(blob: &[u8]) -> Result<Vec<f32>> {
    if !blob.len().is_multiple_of(4) {
        return Err(Error::Database("invalid embedding blob length".into()));
    }

    let mut out = Vec::with_capacity(blob.len() / 4);
    for chunk in blob.chunks_exact(4) {
        out.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    Ok(out)
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }

    let mut dot = 0.0;
    let mut norm_a = 0.0;
    let mut norm_b = 0.0;

    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }

    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }

    dot / (norm_a.sqrt() * norm_b.sqrt())
}

fn text_match_score(query_text: Option<&str>, content: &str) -> f32 {
    let Some(query) = query_text else {
        return 0.0;
    };
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return 0.0;
    }

    let content_lc = content.to_lowercase();
    if content_lc.contains(&query) {
        return 1.0;
    }

    let terms: Vec<&str> = query.split_whitespace().filter(|s| !s.is_empty()).collect();
    if terms.is_empty() {
        return 0.0;
    }

    let matches = terms
        .iter()
        .filter(|term| content_lc.contains(**term))
        .count();
    matches as f32 / terms.len() as f32
}

fn recency_score(created_at: DateTime<Utc>) -> f32 {
    let age_secs = (Utc::now() - created_at).num_seconds().max(0) as f32;
    1.0 / (1.0 + age_secs / 3600.0)
}

trait Pipe: Sized {
    fn pipe<T>(self, f: impl FnOnce(Self) -> T) -> T {
        f(self)
    }
}
impl<T> Pipe for T {}

#[cfg(test)]
mod tests {
    use super::{MemoryRole, MemoryStore, NewMemoryEntry, RecallQuery};
    use chrono::{Duration, Utc};

    fn entry(
        session_id: &str,
        continuity_key: Option<&str>,
        content: &str,
        role: MemoryRole,
        embedding: Option<Vec<f32>>,
    ) -> NewMemoryEntry {
        NewMemoryEntry {
            session_id: session_id.to_string(),
            channel_id: None,
            user_id: Some("user-1".to_string()),
            continuity_key: continuity_key.map(|s| s.to_string()),
            role,
            content: content.to_string(),
            embedding,
            embedding_model: Some("unit-test".to_string()),
            metadata: serde_json::json!({}),
        }
    }

    #[test]
    fn in_memory_creates_memory_entries_table() {
        let store = MemoryStore::in_memory().expect("failed to create in-memory memory store");
        let conn = store.connection().expect("lock should not be poisoned");
        let exists: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='memory_entries'",
                [],
                |row| row.get(0),
            )
            .expect("failed to query sqlite_master");

        assert_eq!(exists, 1);
    }

    #[test]
    fn schema_has_embedding_and_continuity_columns() {
        let store = MemoryStore::in_memory().expect("failed to create in-memory memory store");
        let conn = store.connection().expect("lock should not be poisoned");
        let mut stmt = conn
            .prepare("PRAGMA table_info(memory_entries)")
            .expect("failed to prepare pragma statement");

        let columns: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .expect("failed to read table info")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("failed to collect columns");

        assert!(columns.iter().any(|c| c == "embedding"));
        assert!(columns.iter().any(|c| c == "embedding_model"));
        assert!(columns.iter().any(|c| c == "embedding_dimensions"));
        assert!(columns.iter().any(|c| c == "continuity_key"));
    }

    #[tokio::test]
    async fn remember_and_get_session_context_round_trip() {
        let store = MemoryStore::in_memory().expect("failed to create in-memory memory store");
        store
            .remember(entry(
                "session-a",
                Some("continuity-1"),
                "hello from telegram",
                MemoryRole::User,
                None,
            ))
            .await
            .expect("remember should succeed");

        let context = store
            .get_session_context("session-a", 10)
            .await
            .expect("session context should load");

        assert_eq!(context.session_id, "session-a");
        assert_eq!(context.entries.len(), 1);
        assert_eq!(context.entries[0].content, "hello from telegram");
    }

    #[tokio::test]
    async fn continuity_context_spans_multiple_sessions() {
        let store = MemoryStore::in_memory().expect("failed to create in-memory memory store");
        store
            .remember(entry(
                "session-telegram",
                Some("user-42"),
                "telegram memory",
                MemoryRole::User,
                None,
            ))
            .await
            .expect("first remember should succeed");
        store
            .remember(entry(
                "session-tui",
                Some("user-42"),
                "tui memory",
                MemoryRole::Assistant,
                None,
            ))
            .await
            .expect("second remember should succeed");

        let shared = store
            .get_continuity_context("user-42", 10)
            .await
            .expect("continuity context should load");

        assert_eq!(shared.len(), 2);
        assert!(shared.iter().any(|m| m.content == "telegram memory"));
        assert!(shared.iter().any(|m| m.content == "tui memory"));
    }

    #[tokio::test]
    async fn recall_prefers_embedding_similarity() {
        let store = MemoryStore::in_memory().expect("failed to create in-memory memory store");
        store
            .remember(entry(
                "session-a",
                Some("continuity-1"),
                "first",
                MemoryRole::User,
                Some(vec![1.0, 0.0, 0.0]),
            ))
            .await
            .expect("first remember should succeed");
        store
            .remember(entry(
                "session-a",
                Some("continuity-1"),
                "second",
                MemoryRole::User,
                Some(vec![0.0, 1.0, 0.0]),
            ))
            .await
            .expect("second remember should succeed");

        let recalled = store
            .recall(RecallQuery {
                query_text: None,
                query_embedding: Some(vec![0.95, 0.05, 0.0]),
                session_id: Some("session-a".to_string()),
                continuity_key: None,
                limit: 1,
            })
            .await
            .expect("recall should succeed");

        assert_eq!(recalled.len(), 1);
        assert_eq!(recalled[0].content, "first");
    }

    #[tokio::test]
    async fn compact_and_delete_session_memory_work() {
        let store = MemoryStore::in_memory().expect("failed to create in-memory memory store");
        store
            .remember(entry(
                "session-a",
                Some("continuity-1"),
                "transient",
                MemoryRole::User,
                None,
            ))
            .await
            .expect("remember should succeed");

        let report = store
            .compact(Utc::now() + Duration::seconds(5))
            .await
            .expect("compact should succeed");
        assert_eq!(report.deleted_entries, 1);

        store
            .remember(entry(
                "session-a",
                Some("continuity-1"),
                "persist-me",
                MemoryRole::Assistant,
                None,
            ))
            .await
            .expect("remember should succeed");

        let deleted = store
            .delete_session_memory("session-a")
            .await
            .expect("delete should succeed");
        assert_eq!(deleted, 1);
    }
}
