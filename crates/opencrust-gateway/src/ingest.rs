//! Shared document ingestion pipeline used by both CLI and chat channels.

use opencrust_agents::EmbeddingProvider;
use opencrust_channels::ChannelResponse;
use opencrust_common::{Error, Result};
use opencrust_db::{DocumentStore, NewDocumentChunk};
use std::path::Path;
use tracing::{info, warn};

/// Result of a successful document ingestion.
#[derive(Debug)]
pub struct IngestResult {
    pub name: String,
    pub chunk_count: usize,
    pub has_embeddings: bool,
    pub replaced: bool,
}

/// Shared ingest handler invoked by all channel callbacks.
///
/// Opens the document store, resolves the embedding provider, runs
/// `ingest_from_bytes`, and returns a formatted [`ChannelResponse`].
/// Presence of the word `"replace"` (case-insensitive) in `text` triggers
/// an upsert instead of a duplicate-rejection.
pub async fn run_ingest(
    state: &crate::state::AppState,
    data_dir: &Path,
    text: &str,
    filename: &str,
    data: &[u8],
) -> std::result::Result<ChannelResponse, String> {
    let doc_store = DocumentStore::open(&data_dir.join("memory.db"))
        .map_err(|e| format!("failed to open document store: {e}"))?;
    let embed = state.agents.embedding_provider();
    let replace = text.to_lowercase().contains("replace");

    match ingest_from_bytes(filename, data, &doc_store, embed.as_deref(), replace).await {
        Ok(result) => {
            state.agents.notify_document_ingested();
            let action = if result.replaced {
                "Replaced"
            } else {
                "Ingested"
            };
            let embed_note = if result.has_embeddings {
                " with embeddings"
            } else {
                ""
            };
            Ok(ChannelResponse::Text(format!(
                "{action} {} ({} chunks{embed_note}). You can now ask me anything about this document.",
                result.name, result.chunk_count
            )))
        }
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("already ingested") {
                Ok(ChannelResponse::Text(format!(
                    "{filename} is already ingested. Use /ingest replace to update it."
                )))
            } else {
                Err(format!("Failed to ingest {filename}: {msg}"))
            }
        }
    }
}

/// Ingest a document from a file path.
pub async fn ingest_from_path(
    path: &Path,
    doc_store: &DocumentStore,
    embedding_provider: Option<&dyn EmbeddingProvider>,
    replace: bool,
) -> Result<IngestResult> {
    let text = opencrust_media::extract_text(path)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string());
    let mime = opencrust_media::detect_mime_type(path);

    ingest_text(
        &name,
        &text,
        Some(path.display().to_string()),
        mime,
        doc_store,
        embedding_provider,
        replace,
    )
    .await
}

/// Ingest a document from raw bytes (e.g. downloaded from a chat channel).
pub async fn ingest_from_bytes(
    filename: &str,
    data: &[u8],
    doc_store: &DocumentStore,
    embedding_provider: Option<&dyn EmbeddingProvider>,
    replace: bool,
) -> Result<IngestResult> {
    // Write to temp file for extract_text (it needs a path with extension)
    let ext = Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("txt");
    let temp_dir = std::env::temp_dir().join("opencrust_ingest");
    std::fs::create_dir_all(&temp_dir)
        .map_err(|e| Error::Media(format!("failed to create temp dir: {e}")))?;
    let temp_path = temp_dir.join(format!("ingest_{}.{}", uuid::Uuid::new_v4(), ext));

    std::fs::write(&temp_path, data)
        .map_err(|e| Error::Media(format!("failed to write temp file: {e}")))?;

    let text = opencrust_media::extract_text(&temp_path);
    let _ = std::fs::remove_file(&temp_path);
    let text = text?;

    let mime = opencrust_media::detect_mime_type(Path::new(filename));
    ingest_text(
        filename,
        &text,
        None,
        mime,
        doc_store,
        embedding_provider,
        replace,
    )
    .await
}

/// Core ingestion: chunk text, embed, store.
async fn ingest_text(
    name: &str,
    text: &str,
    source_path: Option<String>,
    mime: &str,
    doc_store: &DocumentStore,
    embedding_provider: Option<&dyn EmbeddingProvider>,
    replace: bool,
) -> Result<IngestResult> {
    if text.trim().is_empty() {
        return Err(Error::Media(format!("no text content found in {name}")));
    }

    // Handle duplicates
    let replaced = if doc_store.get_document_by_name(name)?.is_some() {
        if replace {
            doc_store.remove_document(name)?;
            true
        } else {
            return Err(Error::Media(format!("document '{name}' already ingested")));
        }
    } else {
        false
    };

    let chunks = opencrust_media::chunk_text(text, &opencrust_media::ChunkOptions::default());

    info!("ingesting '{name}' ({} chunks)", chunks.len());

    let doc_id = doc_store.add_document(name, source_path.as_deref(), mime)?;

    let has_embeddings = embedding_provider.is_some();

    let mut embeddings = Vec::with_capacity(chunks.len());
    for chunk in &chunks {
        let embedding = if let Some(provider) = embedding_provider {
            match provider
                .embed_documents(std::slice::from_ref(&chunk.text))
                .await
            {
                Ok(mut vecs) if !vecs.is_empty() => Some(vecs.remove(0)),
                Ok(_) => None,
                Err(e) => {
                    if chunk.index == 0 {
                        warn!("embedding failed for '{name}': {e}");
                    }
                    None
                }
            }
        } else {
            None
        };

        embeddings.push(embedding);
    }

    let model = embedding_provider.map(|p| p.model().to_string());
    let batch_chunks = chunks
        .iter()
        .zip(embeddings.iter())
        .map(|(chunk, embedding)| NewDocumentChunk {
            chunk_index: chunk.index,
            text: &chunk.text,
            embedding: embedding.as_deref(),
            model: model.as_deref(),
            dims: embedding.as_ref().map(|e| e.len()),
            token_count: Some(chunk.token_count),
        })
        .collect::<Vec<_>>();

    doc_store.add_chunks_batch(&doc_id, &batch_chunks)?;

    info!(
        "ingested '{name}': {} chunks{}",
        chunks.len(),
        if has_embeddings {
            " with embeddings"
        } else {
            ""
        }
    );

    Ok(IngestResult {
        name: name.to_string(),
        chunk_count: chunks.len(),
        has_embeddings,
        replaced,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use opencrust_db::DocumentStore;

    fn temp_doc_store() -> DocumentStore {
        let dir = tempfile::tempdir().expect("tempdir");
        DocumentStore::open(&dir.path().join("test.db")).expect("open doc store")
    }

    #[tokio::test]
    async fn ingest_from_bytes_happy_path() {
        let store = temp_doc_store();
        let data = b"Hello world. This is a test document with enough text to chunk.";
        let result = ingest_from_bytes("test.txt", data, &store, None, false)
            .await
            .expect("ingest should succeed");
        assert_eq!(result.name, "test.txt");
        assert!(result.chunk_count > 0);
        assert!(!result.has_embeddings);
        assert!(!result.replaced);
    }

    #[tokio::test]
    async fn ingest_from_bytes_rejects_duplicate() {
        let store = temp_doc_store();
        let data = b"Some content for duplicate test.";
        ingest_from_bytes("dup.txt", data, &store, None, false)
            .await
            .expect("first ingest should succeed");

        let err = ingest_from_bytes("dup.txt", data, &store, None, false)
            .await
            .expect_err("second ingest without replace should fail");
        assert!(err.to_string().contains("already ingested"));
    }

    #[tokio::test]
    async fn ingest_from_bytes_replace_overwrites() {
        let store = temp_doc_store();
        let data = b"Original content.";
        ingest_from_bytes("replace.txt", data, &store, None, false)
            .await
            .expect("first ingest");

        let result = ingest_from_bytes("replace.txt", data, &store, None, true)
            .await
            .expect("replace ingest should succeed");
        assert!(result.replaced);
    }

    #[tokio::test]
    async fn ingest_from_bytes_empty_content_returns_error() {
        let store = temp_doc_store();
        let err = ingest_from_bytes("empty.txt", b"   ", &store, None, false)
            .await
            .expect_err("empty content should fail");
        assert!(err.to_string().contains("no text content"));
    }

    #[tokio::test]
    async fn ingest_from_path_ingests_txt_file() {
        use std::io::Write;
        let store = temp_doc_store();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("sample.txt");
        let mut f = std::fs::File::create(&path).expect("create file");
        writeln!(f, "Sample text file for path ingestion test.").expect("write");

        let result = ingest_from_path(&path, &store, None, false)
            .await
            .expect("ingest from path should succeed");
        assert_eq!(result.name, "sample.txt");
        assert!(result.chunk_count > 0);
    }

    #[tokio::test]
    async fn run_ingest_returns_channel_response_text() {
        use crate::state::AppState;
        use opencrust_agents::AgentRuntime;
        use opencrust_channels::ChannelRegistry;
        use opencrust_config::AppConfig;
        use std::sync::Arc;

        let state = AppState::new(
            AppConfig::default(),
            Arc::new(AgentRuntime::new()),
            ChannelRegistry::new(),
        );
        let dir = tempfile::tempdir().expect("tempdir");
        let data = b"Content for run_ingest test.";

        let resp = run_ingest(&state, dir.path(), "ingest", "doc.txt", data)
            .await
            .expect("run_ingest should succeed");

        match resp {
            opencrust_channels::ChannelResponse::Text(msg) => {
                assert!(msg.contains("doc.txt"), "response should mention filename");
            }
            other => panic!("expected Text response, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn run_ingest_already_ingested_returns_hint() {
        use crate::state::AppState;
        use opencrust_agents::AgentRuntime;
        use opencrust_channels::ChannelRegistry;
        use opencrust_config::AppConfig;
        use std::sync::Arc;

        let state = AppState::new(
            AppConfig::default(),
            Arc::new(AgentRuntime::new()),
            ChannelRegistry::new(),
        );
        let dir = tempfile::tempdir().expect("tempdir");
        let data = b"Content for duplicate run_ingest test.";

        run_ingest(&state, dir.path(), "ingest", "dup.txt", data)
            .await
            .expect("first ingest");

        let resp = run_ingest(&state, dir.path(), "ingest", "dup.txt", data)
            .await
            .expect("second ingest should return Ok with hint message");

        match resp {
            opencrust_channels::ChannelResponse::Text(msg) => {
                assert!(
                    msg.contains("already ingested"),
                    "response should mention already ingested: {msg}"
                );
            }
            other => panic!("expected Text response, got {other:?}"),
        }
    }
}
