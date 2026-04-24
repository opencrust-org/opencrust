use chrono::Utc;
use opencrust_common::{Error, Result};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Mutex;
use uuid::Uuid;

/// A single recorded step within a turn's tool loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrajectoryEvent {
    pub id: String,
    pub session_id: String,
    pub turn_index: u32,
    pub event_type: TrajectoryEventType,
    pub tool_name: Option<String>,
    /// JSON-encoded input args (tool_call) or output text (tool_result / turn_end).
    pub payload: String,
    pub latency_ms: Option<u64>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TrajectoryEventType {
    ToolCall,
    ToolResult,
    TurnEnd,
}

impl TrajectoryEventType {
    fn as_str(&self) -> &'static str {
        match self {
            Self::ToolCall => "tool_call",
            Self::ToolResult => "tool_result",
            Self::TurnEnd => "turn_end",
        }
    }

    fn from_str(s: &str) -> Option<Self> {
        match s {
            "tool_call" => Some(Self::ToolCall),
            "tool_result" => Some(Self::ToolResult),
            "turn_end" => Some(Self::TurnEnd),
            _ => None,
        }
    }
}

/// A tool-call sequence that appears repeatedly across sessions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepeatedToolSequence {
    /// Human-readable fingerprint, e.g. "web_search → web_search → summarize".
    pub fingerprint: String,
    /// Ordered list of tool names in this sequence.
    pub tools: Vec<String>,
    /// Number of turns where this exact sequence was observed.
    pub occurrences: usize,
    /// One session_id where this sequence was seen (for context).
    pub example_session: String,
}

/// SQLite-backed store for per-turn trajectory events.
///
/// Each turn in the agent tool loop produces a sequence of `tool_call` /
/// `tool_result` pairs followed by a single `turn_end` event. These are
/// used for skill auto-suggestion, debug replay, and training-data export.
pub struct TrajectoryStore {
    conn: Mutex<Connection>,
}

impl TrajectoryStore {
    pub fn open(db_path: &Path) -> Result<Self> {
        let conn = Connection::open(db_path)
            .map_err(|e| Error::Database(format!("failed to open trajectory db: {e}")))?;
        let store = Self {
            conn: Mutex::new(conn),
        };
        store.run_migrations()?;
        Ok(store)
    }

    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()
            .map_err(|e| Error::Database(format!("failed to open in-memory trajectory db: {e}")))?;
        let store = Self {
            conn: Mutex::new(conn),
        };
        store.run_migrations()?;
        Ok(store)
    }

    fn run_migrations(&self) -> Result<()> {
        let conn = self.connection()?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS trajectory_events (
                id          TEXT PRIMARY KEY,
                session_id  TEXT NOT NULL,
                turn_index  INTEGER NOT NULL,
                event_type  TEXT NOT NULL,
                tool_name   TEXT,
                payload     TEXT NOT NULL,
                latency_ms  INTEGER,
                created_at  TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_traj_session
                ON trajectory_events(session_id, turn_index, created_at);

            CREATE TABLE IF NOT EXISTS skill_usage_events (
                id          TEXT PRIMARY KEY,
                skill_name  TEXT NOT NULL,
                session_id  TEXT NOT NULL,
                created_at  INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_skill_usage_name
                ON skill_usage_events(skill_name, created_at);",
        )
        .map_err(|e| Error::Database(format!("trajectory migration failed: {e}")))?;
        Ok(())
    }

    fn connection(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| Error::Database("trajectory store lock poisoned".into()))
    }

    fn insert(&self, event: &TrajectoryEvent) -> Result<()> {
        let conn = self.connection()?;
        conn.execute(
            "INSERT INTO trajectory_events
             (id, session_id, turn_index, event_type, tool_name, payload, latency_ms, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                event.id,
                event.session_id,
                event.turn_index,
                event.event_type.as_str(),
                event.tool_name,
                event.payload,
                event.latency_ms.map(|v| v as i64),
                event.created_at,
            ],
        )
        .map_err(|e| Error::Database(format!("trajectory insert failed: {e}")))?;
        Ok(())
    }

    /// Record that a tool was about to be called.
    pub fn log_tool_call(
        &self,
        session_id: &str,
        turn_index: u32,
        tool_name: &str,
        input_json: &str,
    ) -> Result<()> {
        self.insert(&TrajectoryEvent {
            id: Uuid::new_v4().to_string(),
            session_id: session_id.to_string(),
            turn_index,
            event_type: TrajectoryEventType::ToolCall,
            tool_name: Some(tool_name.to_string()),
            payload: input_json.to_string(),
            latency_ms: None,
            created_at: Utc::now().to_rfc3339(),
        })
    }

    /// Record the result of a tool call.
    pub fn log_tool_result(
        &self,
        session_id: &str,
        turn_index: u32,
        tool_name: &str,
        output_text: &str,
        latency_ms: u64,
    ) -> Result<()> {
        self.insert(&TrajectoryEvent {
            id: Uuid::new_v4().to_string(),
            session_id: session_id.to_string(),
            turn_index,
            event_type: TrajectoryEventType::ToolResult,
            tool_name: Some(tool_name.to_string()),
            payload: output_text.to_string(),
            latency_ms: Some(latency_ms),
            created_at: Utc::now().to_rfc3339(),
        })
    }

    /// Record the final assistant output at the end of a turn.
    pub fn log_turn_end(
        &self,
        session_id: &str,
        turn_index: u32,
        final_output: &str,
        total_tokens: u32,
    ) -> Result<()> {
        let payload = serde_json::json!({
            "output": final_output,
            "total_tokens": total_tokens,
        })
        .to_string();
        self.insert(&TrajectoryEvent {
            id: Uuid::new_v4().to_string(),
            session_id: session_id.to_string(),
            turn_index,
            event_type: TrajectoryEventType::TurnEnd,
            tool_name: None,
            payload,
            latency_ms: None,
            created_at: Utc::now().to_rfc3339(),
        })
    }

    /// Record that a skill was injected into the system prompt for a session.
    pub fn log_skill_usage(&self, session_id: &str, skill_name: &str) -> Result<()> {
        let conn = self.connection()?;
        conn.execute(
            "INSERT INTO skill_usage_events (id, skill_name, session_id, created_at)
             VALUES (?1, ?2, ?3, unixepoch())",
            params![Uuid::new_v4().to_string(), skill_name, session_id],
        )
        .map_err(|e| Error::Database(format!("skill_usage insert failed: {e}")))?;
        Ok(())
    }

    /// Return the unix timestamp of the most recent usage of `skill_name`,
    /// or `None` if the skill has never been logged as used.
    pub fn skill_last_used_at(&self, skill_name: &str) -> Result<Option<i64>> {
        let conn = self.connection()?;
        let mut stmt = conn
            .prepare("SELECT MAX(created_at) FROM skill_usage_events WHERE skill_name = ?1")
            .map_err(|e| Error::Database(format!("skill_last_used_at prepare failed: {e}")))?;
        let ts: Option<i64> = stmt
            .query_row(params![skill_name], |row| row.get(0))
            .map_err(|e| Error::Database(format!("skill_last_used_at query failed: {e}")))?;
        Ok(ts)
    }

    /// Return the names from `skill_names` that have not been used since
    /// `cutoff_unix` (seconds since epoch). Skills with no usage record at all
    /// are considered unused.
    pub fn skills_unused_since(
        &self,
        skill_names: &[&str],
        cutoff_unix: i64,
    ) -> Result<Vec<String>> {
        if skill_names.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.connection()?;
        let mut unused = Vec::new();
        for name in skill_names {
            let mut stmt = conn
                .prepare("SELECT MAX(created_at) FROM skill_usage_events WHERE skill_name = ?1")
                .map_err(|e| Error::Database(format!("skills_unused_since prepare failed: {e}")))?;
            let ts: Option<i64> = stmt
                .query_row(params![name], |row| row.get(0))
                .map_err(|e| Error::Database(format!("skills_unused_since query failed: {e}")))?;
            match ts {
                Some(last) if last >= cutoff_unix => {}
                _ => unused.push(name.to_string()),
            }
        }
        Ok(unused)
    }

    /// Return all trajectory events for a session ordered by turn and time.
    pub fn export_session(&self, session_id: &str) -> Result<Vec<TrajectoryEvent>> {
        let conn = self.connection()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, session_id, turn_index, event_type, tool_name, payload, latency_ms, created_at
                 FROM trajectory_events
                 WHERE session_id = ?1
                 ORDER BY turn_index, created_at",
            )
            .map_err(|e| Error::Database(format!("trajectory export prepare failed: {e}")))?;

        let rows = stmt
            .query_map(params![session_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, u32>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                    row.get::<_, String>(7)?,
                ))
            })
            .map_err(|e| Error::Database(format!("trajectory export query failed: {e}")))?;

        rows.map(|r| {
            r.map_err(|e| Error::Database(format!("trajectory row error: {e}")))
                .and_then(|(id, sid, ti, et, tn, payload, lat, ca)| {
                    let event_type = TrajectoryEventType::from_str(&et)
                        .ok_or_else(|| Error::Database(format!("unknown event_type: {et}")))?;
                    Ok(TrajectoryEvent {
                        id,
                        session_id: sid,
                        turn_index: ti,
                        event_type,
                        tool_name: tn,
                        payload,
                        latency_ms: lat.map(|v| v as u64),
                        created_at: ca,
                    })
                })
        })
        .collect()
    }

    /// Return tool sequences (ordered list of tool names per turn) that appear at least
    /// `min_occurrences` times across all sessions.
    ///
    /// A "sequence" is the ordered list of tool names called within one turn (one
    /// user→assistant exchange). Sequences with a single tool call are excluded
    /// because single-tool patterns are too coarse for skill suggestions.
    pub fn find_repeated_tool_sequences(
        &self,
        min_occurrences: usize,
    ) -> Result<Vec<RepeatedToolSequence>> {
        let conn = self.connection()?;

        // Fetch all tool_call events ordered so we can group by (session, turn).
        let mut stmt = conn
            .prepare(
                "SELECT session_id, turn_index, tool_name
                 FROM trajectory_events
                 WHERE event_type = 'tool_call' AND tool_name IS NOT NULL
                 ORDER BY session_id, turn_index, created_at",
            )
            .map_err(|e| Error::Database(format!("trajectory sequence prepare failed: {e}")))?;

        // Collect (session_id, turn_index) → [tool_name, ...].
        let mut turn_tools: std::collections::HashMap<(String, u32), Vec<String>> =
            std::collections::HashMap::new();
        let mut example_sessions: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();

        let rows = stmt
            .query_map(rusqlite::params![], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, u32>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|e| Error::Database(format!("trajectory sequence query failed: {e}")))?;

        for row in rows {
            let (session_id, turn_index, tool_name) =
                row.map_err(|e| Error::Database(format!("trajectory row error: {e}")))?;
            turn_tools
                .entry((session_id.clone(), turn_index))
                .or_default()
                .push(tool_name);
            example_sessions
                .entry(session_id.clone() + &turn_index.to_string())
                .or_insert(session_id);
        }

        // Count how often each tool sequence fingerprint appears.
        let mut counts: std::collections::HashMap<String, (usize, Vec<String>, String)> =
            std::collections::HashMap::new();

        for ((session_id, turn_index), tools) in &turn_tools {
            if tools.len() < 2 {
                continue; // single-tool turns are too coarse
            }
            let fingerprint = tools.join(" → ");
            let key = session_id.clone() + &turn_index.to_string();
            let example = example_sessions.get(&key).cloned().unwrap_or_default();
            let entry = counts
                .entry(fingerprint)
                .or_insert((0, tools.clone(), example));
            entry.0 += 1;
        }

        let mut results: Vec<RepeatedToolSequence> = counts
            .into_iter()
            .filter(|(_, (count, _, _))| *count >= min_occurrences)
            .map(
                |(fingerprint, (occurrences, tools, example_session))| RepeatedToolSequence {
                    fingerprint,
                    tools,
                    occurrences,
                    example_session,
                },
            )
            .collect();

        results.sort_by_key(|b| std::cmp::Reverse(b.occurrences));
        Ok(results)
    }

    /// Count total stored events for a session.
    pub fn count_session_events(&self, session_id: &str) -> Result<usize> {
        let conn = self.connection()?;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM trajectory_events WHERE session_id = ?1",
                params![session_id],
                |row| row.get(0),
            )
            .map_err(|e| Error::Database(format!("trajectory count failed: {e}")))?;
        Ok(count as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_store() -> TrajectoryStore {
        TrajectoryStore::in_memory().expect("in-memory store should open")
    }

    #[test]
    fn log_and_export_round_trip() {
        let store = make_store();
        store
            .log_tool_call("s1", 0, "web_search", r#"{"query":"X"}"#)
            .unwrap();
        store
            .log_tool_result("s1", 0, "web_search", "results...", 320)
            .unwrap();
        store.log_turn_end("s1", 0, "final answer", 500).unwrap();

        let events = store.export_session("s1").unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].event_type, TrajectoryEventType::ToolCall);
        assert_eq!(events[1].event_type, TrajectoryEventType::ToolResult);
        assert_eq!(events[1].latency_ms, Some(320));
        assert_eq!(events[2].event_type, TrajectoryEventType::TurnEnd);
    }

    #[test]
    fn export_scoped_to_session() {
        let store = make_store();
        store.log_tool_call("s1", 0, "tool_a", "{}").unwrap();
        store.log_tool_call("s2", 0, "tool_b", "{}").unwrap();

        let s1 = store.export_session("s1").unwrap();
        let s2 = store.export_session("s2").unwrap();
        assert_eq!(s1.len(), 1);
        assert_eq!(s2.len(), 1);
        assert_eq!(s1[0].tool_name.as_deref(), Some("tool_a"));
        assert_eq!(s2[0].tool_name.as_deref(), Some("tool_b"));
    }

    #[test]
    fn count_session_events_correct() {
        let store = make_store();
        store.log_tool_call("s1", 0, "t", "{}").unwrap();
        store.log_tool_result("s1", 0, "t", "out", 10).unwrap();
        store.log_turn_end("s1", 0, "done", 100).unwrap();
        assert_eq!(store.count_session_events("s1").unwrap(), 3);
        assert_eq!(store.count_session_events("s2").unwrap(), 0);
    }

    #[test]
    fn events_ordered_by_turn_then_time() {
        let store = make_store();
        store.log_turn_end("s1", 1, "turn1", 100).unwrap();
        store.log_tool_call("s1", 0, "tool", "{}").unwrap();
        store.log_turn_end("s1", 0, "turn0", 50).unwrap();

        let events = store.export_session("s1").unwrap();
        assert_eq!(events[0].turn_index, 0);
        assert_eq!(events[0].event_type, TrajectoryEventType::ToolCall);
        assert_eq!(events[2].turn_index, 1);
    }

    fn log_sequence(store: &TrajectoryStore, session: &str, turn: u32, tools: &[&str]) {
        for tool in tools {
            store.log_tool_call(session, turn, tool, "{}").unwrap();
            store
                .log_tool_result(session, turn, tool, "out", 10)
                .unwrap();
        }
        store.log_turn_end(session, turn, "done", 0).unwrap();
    }

    #[test]
    fn repeated_sequence_detected() {
        let store = make_store();
        // Same sequence in 3 different turns across 2 sessions
        log_sequence(&store, "s1", 0, &["web_search", "summarize"]);
        log_sequence(&store, "s1", 1, &["web_search", "summarize"]);
        log_sequence(&store, "s2", 0, &["web_search", "summarize"]);

        let results = store.find_repeated_tool_sequences(2).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].fingerprint, "web_search → summarize");
        assert_eq!(results[0].occurrences, 3);
    }

    #[test]
    fn below_min_occurrences_excluded() {
        let store = make_store();
        log_sequence(&store, "s1", 0, &["web_search", "summarize"]);
        log_sequence(&store, "s1", 1, &["doc_search", "summarize"]);

        // Both sequences appear only once → neither meets min_occurrences=2
        let results = store.find_repeated_tool_sequences(2).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn single_tool_turns_excluded() {
        let store = make_store();
        // Single-tool turns should never be suggested
        log_sequence(&store, "s1", 0, &["web_search"]);
        log_sequence(&store, "s1", 1, &["web_search"]);
        log_sequence(&store, "s2", 0, &["web_search"]);

        let results = store.find_repeated_tool_sequences(2).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn results_sorted_by_occurrences_desc() {
        let store = make_store();
        log_sequence(&store, "s1", 0, &["web_search", "summarize"]);
        log_sequence(&store, "s1", 1, &["web_search", "summarize"]);
        log_sequence(&store, "s2", 0, &["web_search", "summarize"]);

        log_sequence(&store, "s3", 0, &["doc_search", "web_search"]);
        log_sequence(&store, "s3", 1, &["doc_search", "web_search"]);

        let results = store.find_repeated_tool_sequences(2).unwrap();
        assert_eq!(results.len(), 2);
        assert!(results[0].occurrences >= results[1].occurrences);
    }

    #[test]
    fn skill_usage_log_and_last_used() {
        let store = make_store();
        assert!(store.skill_last_used_at("my-skill").unwrap().is_none());

        store.log_skill_usage("s1", "my-skill").unwrap();
        store.log_skill_usage("s2", "my-skill").unwrap();
        store.log_skill_usage("s1", "other-skill").unwrap();

        let ts = store.skill_last_used_at("my-skill").unwrap();
        assert!(ts.is_some(), "should have a usage timestamp");
        assert!(ts.unwrap() > 0);

        let ts2 = store.skill_last_used_at("other-skill").unwrap();
        assert!(ts2.is_some());
    }

    #[test]
    fn skills_unused_since_returns_correct_subset() {
        let store = make_store();
        store.log_skill_usage("s1", "active-skill").unwrap();

        // cutoff = far future → everything counts as unused
        let far_future = chrono::Utc::now().timestamp() + 9999;
        let unused = store
            .skills_unused_since(&["active-skill", "never-used"], far_future)
            .unwrap();
        assert_eq!(unused, vec!["active-skill", "never-used"]);

        // cutoff = past → recently used skill is NOT unused
        let past = chrono::Utc::now().timestamp() - 9999;
        let unused2 = store
            .skills_unused_since(&["active-skill", "never-used"], past)
            .unwrap();
        assert_eq!(unused2, vec!["never-used"]);
    }

    #[test]
    fn skills_unused_since_empty_input_returns_empty() {
        let store = make_store();
        let result = store.skills_unused_since(&[], 0).unwrap();
        assert!(result.is_empty());
    }
}
