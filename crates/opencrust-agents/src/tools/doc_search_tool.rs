use async_trait::async_trait;
use opencrust_common::{Error, Result};
use opencrust_db::DocumentStore;
use std::path::PathBuf;
use std::sync::Arc;

use super::{Tool, ToolContext, ToolOutput};

const DEFAULT_LIMIT: usize = 5;
const MAX_LIMIT: usize = 20;
const DEFAULT_MIN_SIMILARITY: f64 = 0.3;

/// Returns true when the query looks like a filename (e.g. "CLAUDE.md", "report.pdf").
/// Requires an extension of 2–10 ASCII alphanumeric chars to avoid false positives
/// like "version 2.0" (ext "0", length 1) or "3.14 is pi" (ext "14" but prefix is digits).
fn looks_like_filename(query: &str) -> bool {
    let trimmed = query.trim();
    if let Some(dot_pos) = trimmed.rfind('.') {
        let ext = &trimmed[dot_pos + 1..];
        let prefix = trimmed[..dot_pos].trim();
        ext.len() >= 2
            && ext.len() <= 10
            && ext.chars().all(|c| c.is_ascii_alphanumeric())
            && !prefix.is_empty()
            && !prefix.chars().all(|c| c.is_ascii_digit())
    } else {
        false
    }
}

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
            "Use this FIRST for any question about documents, data, regulations, properties, \
             or reference material the user has shared. Also use when the user asks about a \
             specific file by name (e.g. 'what is in CLAUDE.md?') — pass the filename as the \
             query and the tool will find it by name if semantic search returns nothing. \
             If document context was already provided above but seems incomplete or does not \
             fully answer the question, call this tool with a more specific query to retrieve \
             additional chunks. Do NOT use file_read for ingested documents.",
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

        let query_embedding = if let Some(embed_fn) = &self.embed_fn {
            Some(
                embed_fn(query)
                    .await
                    .map_err(|e| Error::Agent(format!("failed to embed query: {e}")))?,
            )
        } else {
            None
        };

        let mut chunks = store
            .hybrid_search_chunks(
                query,
                query_embedding.as_deref(),
                limit,
                DEFAULT_MIN_SIMILARITY,
            )
            .map_err(|e| Error::Agent(format!("document search failed: {e}")))?;

        // Filename fallback: when the query looks like a filename, or when semantic/keyword
        // search returned nothing, also look up documents by name directly so the user can
        // ask "what is in CLAUDE.md?" and get an answer even if the content isn't semantically
        // close to the filename string.
        if chunks.is_empty() || looks_like_filename(query) {
            let matched_docs = match store
                .get_document_by_name(query)
                .map_err(|e| Error::Agent(format!("name lookup failed: {e}")))?
            {
                Some(doc) => vec![doc],
                None => store
                    .search_documents_by_name(query)
                    .map_err(|e| Error::Agent(format!("name search failed: {e}")))?,
            };

            let existing_ids: std::collections::HashSet<String> =
                chunks.iter().map(|c| c.id.clone()).collect();

            'outer: for doc in matched_docs {
                let doc_chunks = store
                    .get_chunks_by_document_id(&doc.id)
                    .map_err(|e| Error::Agent(format!("chunk fetch failed: {e}")))?;
                for chunk in doc_chunks {
                    if !existing_ids.contains(&chunk.id) {
                        chunks.push(chunk);
                        if chunks.len() >= limit {
                            break 'outer;
                        }
                    }
                }
            }
        }

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

    #[test]
    fn looks_like_filename_detection() {
        assert!(looks_like_filename("CLAUDE.md"));
        assert!(looks_like_filename("report.pdf"));
        assert!(looks_like_filename("data.json"));
        assert!(looks_like_filename("  CLAUDE.md  "));
        assert!(!looks_like_filename("what is in the document"));
        assert!(!looks_like_filename("3.14 is pi"));
        assert!(!looks_like_filename("version 2.0"));
        assert!(!looks_like_filename("no extension here"));
        assert!(!looks_like_filename(".hidden"));
    }

    #[test]
    fn finds_document_by_filename_when_semantic_returns_empty() {
        let tmp = NamedTempFile::new().unwrap();
        {
            let store = DocumentStore::open(tmp.path()).unwrap();
            let doc_id = store
                .add_document("CLAUDE.md", Some("/tmp/CLAUDE.md"), "text/markdown")
                .unwrap();
            store
                .add_chunk(
                    &doc_id,
                    0,
                    "This is the CLAUDE.md content",
                    None,
                    None,
                    None,
                    None,
                )
                .unwrap();
            store.update_chunk_count(&doc_id, 1).unwrap();
        }

        let tool = make_tool_with_path(tmp.path().to_path_buf());
        let ctx = ToolContext {
            session_id: "test".into(),
            user_id: None,
            heartbeat_depth: 0,
            allowed_tools: None,
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        let output = rt
            .block_on(tool.execute(&ctx, serde_json::json!({"query": "CLAUDE.md"})))
            .unwrap();
        assert!(!output.is_error);
        assert!(output.content.contains("CLAUDE.md"));
        assert!(output.content.contains("CLAUDE.md content"));
    }
}
