use async_trait::async_trait;
use opencrust_common::{Error, Result};
use opencrust_db::DocumentStore;
use std::path::PathBuf;
use std::sync::Arc;

use super::{Tool, ToolContext, ToolOutput};

const DEFAULT_LIMIT: usize = 5;
const MAX_LIMIT: usize = 20;
const DEFAULT_MIN_SIMILARITY: f64 = 0.3;

/// Async embedding function type.
pub type EmbedFn =
    Arc<dyn Fn(&str) -> futures::future::BoxFuture<'_, Result<Vec<f32>>> + Send + Sync>;

/// Search ingested documents for relevant content.
///
/// Opens the document store fresh on each call so documents ingested after
/// startup are immediately visible without restarting the gateway.
/// Uses vector similarity when an embedding function is provided, otherwise
/// falls back to keyword (LIKE) search.
pub struct DocSearchTool {
    db_path: PathBuf,
    embed_fn: Option<EmbedFn>,
}

impl DocSearchTool {
    pub fn new(db_path: PathBuf, embed_fn: Option<EmbedFn>) -> Self {
        Self { db_path, embed_fn }
    }
}

#[async_trait]
impl Tool for DocSearchTool {
    fn name(&self) -> &str {
        "doc_search"
    }

    fn description(&self) -> &str {
        "Search ingested documents for content relevant to a query. Returns the most similar text chunks with source attribution."
    }

    fn system_hint(&self) -> Option<&str> {
        Some(
            "Use this FIRST for any question about documents, data, regulations, properties, or reference material the user has shared. Do NOT use file_read for this.",
        )
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query to find relevant document content"
                },
                "limit": {
                    "type": "number",
                    "description": "Maximum number of chunks to return (1-20, default 5)"
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(
        &self,
        _context: &ToolContext,
        input: serde_json::Value,
    ) -> Result<ToolOutput> {
        let query = input
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Agent("missing 'query' parameter".into()))?;

        let limit = input
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|v| (v as usize).clamp(1, MAX_LIMIT))
            .unwrap_or(DEFAULT_LIMIT);

        let store = DocumentStore::open(&self.db_path)
            .map_err(|e| Error::Agent(format!("failed to open document store: {e}")))?;

        let chunks = if let Some(embed_fn) = &self.embed_fn {
            let query_embedding = embed_fn(query)
                .await
                .map_err(|e| Error::Agent(format!("failed to embed query: {e}")))?;
            store
                .search_chunks(&query_embedding, limit, DEFAULT_MIN_SIMILARITY)
                .map_err(|e| Error::Agent(format!("document search failed: {e}")))?
        } else {
            store
                .keyword_search_chunks(query, limit)
                .map_err(|e| Error::Agent(format!("keyword search failed: {e}")))?
        };

        if chunks.is_empty() {
            return Ok(ToolOutput::success(
                "No relevant document content found for this query.",
            ));
        }

        let mut output = format!("Found {} relevant chunk(s):\n\n", chunks.len());
        for (i, chunk) in chunks.iter().enumerate() {
            output.push_str(&format!(
                "--- [{}/{}] {} (chunk {}, score: {:.2}) ---\n{}\n\n",
                i + 1,
                chunks.len(),
                chunk.document_name,
                chunk.chunk_index,
                chunk.score,
                chunk.text,
            ));
        }

        Ok(ToolOutput::success(output.trim_end()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn make_tool_with_path(path: PathBuf) -> DocSearchTool {
        DocSearchTool::new(path, None)
    }

    #[test]
    fn returns_error_on_missing_query() {
        let tmp = NamedTempFile::new().unwrap();
        // Initialise the schema by opening once.
        DocumentStore::open(tmp.path()).unwrap();
        let tool = make_tool_with_path(tmp.path().to_path_buf());
        let ctx = ToolContext {
            session_id: "test".into(),
            user_id: None,
            heartbeat_depth: 0,
            allowed_tools: None,
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(tool.execute(&ctx, serde_json::json!({})));
        assert!(result.is_err());
    }

    #[test]
    fn returns_no_results_on_empty_store() {
        let tmp = NamedTempFile::new().unwrap();
        DocumentStore::open(tmp.path()).unwrap();
        let tool = make_tool_with_path(tmp.path().to_path_buf());
        let ctx = ToolContext {
            session_id: "test".into(),
            user_id: None,
            heartbeat_depth: 0,
            allowed_tools: None,
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt
            .block_on(tool.execute(&ctx, serde_json::json!({"query": "test"})))
            .unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("No relevant document content"));
    }
}
