//! Shared document ingestion pipeline used by both CLI and chat channels.

use opencrust_agents::EmbeddingProvider;
use opencrust_common::{Error, Result};
use opencrust_db::DocumentStore;
use std::path::Path;
use tracing::{info, warn};

/// Result of a successful document ingestion.
pub struct IngestResult {
    pub name: String,
    pub chunk_count: usize,
    pub has_embeddings: bool,
    pub replaced: bool,
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

        let model = embedding_provider.map(|p| p.model().to_string());
        let dims = embedding.as_ref().map(|e| e.len());

        doc_store.add_chunk(
            &doc_id,
            chunk.index,
            &chunk.text,
            embedding.as_deref(),
            model.as_deref(),
            dims,
            Some(chunk.token_count),
        )?;
    }

    doc_store.update_chunk_count(&doc_id, chunks.len())?;

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
