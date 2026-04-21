use async_trait::async_trait;
use opencrust_common::{Error, Result};
use opencrust_db::{MemoryRole, MemoryStore, NewMemoryEntry, RecallQuery};
use std::path::PathBuf;

use super::{Tool, ToolContext, ToolOutput};

const DEFAULT_RECALL_LIMIT: usize = 10;
const MAX_RECALL_LIMIT: usize = 50;
const MAX_CONTENT_BYTES: usize = 4096;

/// Explicitly save or recall durable notes that persist across sessions.
///
/// Unlike conversation history (which is stored automatically), entries saved
/// through this tool are tagged as explicit agent notes and survive session
/// compaction. Use it to remember facts, preferences, or decisions that should
/// be available in future conversations.
pub struct MemoryTool {
    db_path: PathBuf,
}

impl MemoryTool {
    pub fn new(db_path: PathBuf) -> Self {
        Self { db_path }
    }
}

#[async_trait]
impl Tool for MemoryTool {
    fn name(&self) -> &str {
        "memory"
    }

    fn description(&self) -> &str {
        "Save a durable note to persistent memory, or recall previously saved notes. \
         Saved notes survive across sessions and are not affected by conversation compaction. \
         Use action \"save\" to persist a fact/preference/decision, \
         and action \"recall\" to retrieve relevant notes by keyword."
    }

    fn system_hint(&self) -> Option<&str> {
        Some(
            "Use memory(action=\"save\") to persist important facts, user preferences, or \
             decisions that should be available in future sessions. \
             Use memory(action=\"recall\", query=\"...\") to retrieve relevant notes before \
             answering questions about past interactions or user-specific context.",
        )
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["save", "recall"],
                    "description": "\"save\" to persist a note, \"recall\" to search saved notes"
                },
                "content": {
                    "type": "string",
                    "description": "The note to save (required for action=\"save\")"
                },
                "query": {
                    "type": "string",
                    "description": "Keyword or phrase to search for (required for action=\"recall\")"
                },
                "limit": {
                    "type": "number",
                    "description": "Maximum number of notes to return for recall (1–50, default 10)"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, context: &ToolContext, input: serde_json::Value) -> Result<ToolOutput> {
        let action = input
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Agent("missing 'action' parameter".into()))?;

        match action {
            "save" => {
                let content = input
                    .get("content")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        Error::Agent("missing 'content' parameter for action=save".into())
                    })?;

                if content.trim().is_empty() {
                    return Ok(ToolOutput::error("content cannot be empty"));
                }

                if content.len() > MAX_CONTENT_BYTES {
                    return Ok(ToolOutput::error(format!(
                        "content too large: {} bytes (limit: {} bytes)",
                        content.len(),
                        MAX_CONTENT_BYTES
                    )));
                }

                let store = MemoryStore::open(&self.db_path)
                    .map_err(|e| Error::Agent(format!("failed to open memory store: {e}")))?;

                let entry = NewMemoryEntry {
                    session_id: context.session_id.clone(),
                    channel_id: None,
                    user_id: context.user_id.clone(),
                    continuity_key: context.user_id.clone(),
                    role: MemoryRole::System,
                    content: content.to_string(),
                    embedding: None,
                    embedding_model: None,
                    metadata: serde_json::json!({ "source": "explicit" }),
                };

                let id = store
                    .remember(entry)
                    .await
                    .map_err(|e| Error::Agent(format!("failed to save memory: {e}")))?;

                Ok(ToolOutput::success(format!("saved note (id: {id})")))
            }

            "recall" => {
                let query = input.get("query").and_then(|v| v.as_str()).ok_or_else(|| {
                    Error::Agent("missing 'query' parameter for action=recall".into())
                })?;

                let limit = input
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .map(|v| (v as usize).clamp(1, MAX_RECALL_LIMIT))
                    .unwrap_or(DEFAULT_RECALL_LIMIT);

                let store = MemoryStore::open(&self.db_path)
                    .map_err(|e| Error::Agent(format!("failed to open memory store: {e}")))?;

                let entries = store
                    .recall(RecallQuery {
                        query_text: Some(query.to_string()),
                        query_embedding: None,
                        session_id: None,
                        continuity_key: context.user_id.clone(),
                        limit,
                    })
                    .await
                    .map_err(|e| Error::Agent(format!("recall failed: {e}")))?;

                // Filter to only explicitly saved notes
                let notes: Vec<_> = entries
                    .iter()
                    .filter(|e| {
                        e.metadata.get("source").and_then(|s| s.as_str()) == Some("explicit")
                    })
                    .collect();

                if notes.is_empty() {
                    return Ok(ToolOutput::success(format!(
                        "No saved notes found matching {:?}.",
                        query
                    )));
                }

                let mut output = format!("{} note(s) found:\n\n", notes.len());
                for (i, note) in notes.iter().enumerate() {
                    output.push_str(&format!(
                        "[{}/{}] ({})\n{}\n\n",
                        i + 1,
                        notes.len(),
                        note.created_at.format("%Y-%m-%d %H:%M UTC"),
                        note.content,
                    ));
                }

                Ok(ToolOutput::success(output.trim_end()))
            }

            other => Ok(ToolOutput::error(format!(
                "unknown action {:?} — must be \"save\" or \"recall\"",
                other
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn ctx() -> ToolContext {
        ToolContext {
            session_id: "test-session".into(),
            user_id: Some("user-1".into()),
            heartbeat_depth: 0,
            allowed_tools: None,
        }
    }

    fn make_tool() -> (MemoryTool, NamedTempFile) {
        let tmp = NamedTempFile::new().unwrap();
        // Initialise schema
        MemoryStore::open(tmp.path()).unwrap();
        let tool = MemoryTool::new(tmp.path().to_path_buf());
        (tool, tmp)
    }

    #[tokio::test]
    async fn save_and_recall_round_trip() {
        let (tool, _tmp) = make_tool();

        let save = tool
            .execute(
                &ctx(),
                serde_json::json!({ "action": "save", "content": "user prefers dark mode" }),
            )
            .await
            .unwrap();
        assert!(!save.is_error, "{}", save.content);

        let recall = tool
            .execute(
                &ctx(),
                serde_json::json!({ "action": "recall", "query": "dark mode" }),
            )
            .await
            .unwrap();
        assert!(!recall.is_error, "{}", recall.content);
        assert!(recall.content.contains("dark mode"));
    }

    #[tokio::test]
    async fn recall_returns_no_results_when_empty() {
        let (tool, _tmp) = make_tool();

        let output = tool
            .execute(
                &ctx(),
                serde_json::json!({ "action": "recall", "query": "nothing here" }),
            )
            .await
            .unwrap();

        assert!(!output.is_error);
        assert!(output.content.contains("No saved notes"));
    }

    #[tokio::test]
    async fn save_rejects_empty_content() {
        let (tool, _tmp) = make_tool();

        let output = tool
            .execute(
                &ctx(),
                serde_json::json!({ "action": "save", "content": "   " }),
            )
            .await
            .unwrap();

        assert!(output.is_error);
    }

    #[tokio::test]
    async fn unknown_action_returns_error() {
        let (tool, _tmp) = make_tool();

        let output = tool
            .execute(&ctx(), serde_json::json!({ "action": "delete" }))
            .await
            .unwrap();

        assert!(output.is_error);
        assert!(output.content.contains("unknown action"));
    }

    #[tokio::test]
    async fn missing_action_returns_error() {
        let (tool, _tmp) = make_tool();
        let result = tool.execute(&ctx(), serde_json::json!({})).await;
        assert!(result.is_err());
    }
}
