use opencrust_common::{Error, Result};
use rusqlite::{Connection, params};
use std::cmp::Ordering;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::migrations::DOCUMENT_SCHEMA_V1;

/// Metadata about an ingested document.
#[derive(Debug, Clone)]
pub struct DocumentInfo {
    pub id: String,
    pub name: String,
    pub source_path: Option<String>,
    pub mime_type: String,
    pub chunk_count: usize,
    pub created_at: String,
}

/// A single document chunk returned from vector similarity search.
#[derive(Debug, Clone)]
pub struct DocumentChunk {
    pub id: String,
    pub document_id: String,
    pub document_name: String,
    pub chunk_index: usize,
    pub text: String,
    pub score: f64,
}

/// Store for RAG document ingestion and vector-based retrieval.
///
/// Documents are split into chunks, each optionally carrying an embedding
/// vector. Similarity search loads all embedded chunks and ranks them
/// using cosine similarity in Rust (no dependency on sqlite-vec).
pub struct DocumentStore {
    conn: Mutex<Connection>,
}

impl DocumentStore {
    /// Open or create the document store at the given database path.
    pub fn open(db_path: &Path) -> Result<Self> {
        info!("opening document store at {}", db_path.display());
        let conn = Connection::open(db_path)
            .map_err(|e| Error::Database(format!("failed to open document database: {e}")))?;

        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .map_err(|e| Error::Database(format!("failed to set pragmas: {e}")))?;

        let store = Self {
            conn: Mutex::new(conn),
        };
        store.run_migrations()?;
        Ok(store)
    }

    /// Create an in-memory document store (useful for testing).
    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()
            .map_err(|e| Error::Database(format!("failed to open in-memory document db: {e}")))?;

        conn.execute_batch("PRAGMA foreign_keys=ON;")
            .map_err(|e| Error::Database(format!("failed to set pragmas: {e}")))?;

        let store = Self {
            conn: Mutex::new(conn),
        };
        store.run_migrations()?;
        Ok(store)
    }

    fn run_migrations(&self) -> Result<()> {
        let conn = self.connection()?;
        conn.execute_batch(DOCUMENT_SCHEMA_V1.sql)
            .map_err(|e| Error::Database(format!("document migration failed: {e}")))?;
        Ok(())
    }

    fn connection(&self) -> Result<MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| Error::Database("document store lock poisoned".into()))
    }

    /// Register a new document and return its generated ID.
    pub fn add_document(
        &self,
        name: &str,
        source_path: Option<&str>,
        mime_type: &str,
    ) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let conn = self.connection()?;

        conn.execute(
            "INSERT INTO documents (id, name, source_path, mime_type) VALUES (?, ?, ?, ?)",
            params![id, name, source_path, mime_type],
        )
        .map_err(|e| Error::Database(format!("failed to insert document: {e}")))?;

        info!("added document '{}' with id {}", name, id);
        Ok(id)
    }

    /// Add a chunk belonging to a document. Returns the generated chunk ID.
    #[allow(clippy::too_many_arguments)]
    pub fn add_chunk(
        &self,
        doc_id: &str,
        chunk_index: usize,
        text: &str,
        embedding: Option<&[f32]>,
        model: Option<&str>,
        dims: Option<usize>,
        token_count: Option<usize>,
    ) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let embedding_blob = embedding.map(embedding_to_blob);
        let dims_i64 = dims.map(|d| d as i64);
        let token_count_i64 = token_count.map(|t| t as i64);

        let conn = self.connection()?;
        conn.execute(
            "INSERT INTO document_chunks (
                id, document_id, chunk_index, text,
                embedding, embedding_model, embedding_dimensions, token_count
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                id,
                doc_id,
                chunk_index as i64,
                text,
                embedding_blob,
                model,
                dims_i64,
                token_count_i64,
            ],
        )
        .map_err(|e| Error::Database(format!("failed to insert document chunk: {e}")))?;

        debug!(
            "added chunk {} for document {} (index {})",
            id, doc_id, chunk_index
        );
        Ok(id)
    }

    /// Update the cached chunk count on the parent document row.
    pub fn update_chunk_count(&self, doc_id: &str, count: usize) -> Result<()> {
        let conn = self.connection()?;
        conn.execute(
            "UPDATE documents SET chunk_count = ? WHERE id = ?",
            params![count as i64, doc_id],
        )
        .map_err(|e| Error::Database(format!("failed to update chunk count: {e}")))?;
        Ok(())
    }

    /// Vector similarity search across all document chunks that have embeddings.
    ///
    /// Loads candidate chunks, deserializes their embeddings, computes cosine
    /// similarity against `query_embedding`, filters by `min_similarity`, and
    /// returns the top `limit` results sorted by descending score.
    pub fn search_chunks(
        &self,
        query_embedding: &[f32],
        limit: usize,
        min_similarity: f64,
    ) -> Result<Vec<DocumentChunk>> {
        let conn = self.connection()?;

        let mut stmt = conn
            .prepare(
                "SELECT c.id, c.document_id, d.name, c.chunk_index, c.text, c.embedding
                 FROM document_chunks c
                 JOIN documents d ON d.id = c.document_id
                 WHERE c.embedding IS NOT NULL",
            )
            .map_err(|e| Error::Database(format!("failed to prepare chunk search query: {e}")))?;

        let rows = stmt
            .query_map([], |row| {
                let id: String = row.get(0)?;
                let document_id: String = row.get(1)?;
                let document_name: String = row.get(2)?;
                let chunk_index: i64 = row.get(3)?;
                let text: String = row.get(4)?;
                let embedding_blob: Vec<u8> = row.get(5)?;
                Ok((
                    id,
                    document_id,
                    document_name,
                    chunk_index,
                    text,
                    embedding_blob,
                ))
            })
            .map_err(|e| Error::Database(format!("failed to execute chunk search: {e}")))?;

        let mut scored: Vec<(f64, DocumentChunk)> = Vec::new();

        for row_result in rows {
            let (id, document_id, document_name, chunk_index, text, embedding_blob) = row_result
                .map_err(|e| Error::Database(format!("failed to read chunk row: {e}")))?;

            let embedding = match blob_to_embedding(&embedding_blob) {
                Ok(e) => e,
                Err(e) => {
                    warn!("skipping chunk {} with invalid embedding: {}", id, e);
                    continue;
                }
            };

            let score = cosine_similarity(query_embedding, &embedding) as f64;

            if score < min_similarity {
                continue;
            }

            scored.push((
                score,
                DocumentChunk {
                    id,
                    document_id,
                    document_name,
                    chunk_index: chunk_index as usize,
                    text,
                    score,
                },
            ));
        }

        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(Ordering::Equal));
        scored.truncate(limit);

        Ok(scored.into_iter().map(|(_, chunk)| chunk).collect())
    }

    /// Search document chunks by keyword (case-insensitive LIKE match).
    ///
    /// Used as a fallback when no embedding provider is configured.
    pub fn keyword_search_chunks(&self, query: &str, limit: usize) -> Result<Vec<DocumentChunk>> {
        let conn = self.connection()?;
        let pattern = format!("%{}%", query.replace('%', "\\%").replace('_', "\\_"));
        let mut stmt = conn
            .prepare(
                "SELECT c.id, c.document_id, d.name, c.chunk_index, c.text
                 FROM document_chunks c
                 JOIN documents d ON d.id = c.document_id
                 WHERE c.text LIKE ?1 ESCAPE '\\'
                 LIMIT ?2",
            )
            .map_err(|e| Error::Database(format!("failed to prepare keyword search: {e}")))?;

        let rows = stmt
            .query_map(rusqlite::params![pattern, limit as i64], |row| {
                Ok(DocumentChunk {
                    id: row.get(0)?,
                    document_id: row.get(1)?,
                    document_name: row.get(2)?,
                    chunk_index: row.get::<_, i64>(3)? as usize,
                    text: row.get(4)?,
                    score: 1.0,
                })
            })
            .map_err(|e| Error::Database(format!("failed to execute keyword search: {e}")))?;

        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| Error::Database(format!("failed to read keyword search rows: {e}")))
    }

    /// List all documents with their metadata.
    pub fn list_documents(&self) -> Result<Vec<DocumentInfo>> {
        let conn = self.connection()?;

        let mut stmt = conn
            .prepare(
                "SELECT id, name, source_path, mime_type, chunk_count, created_at
                 FROM documents
                 ORDER BY created_at DESC",
            )
            .map_err(|e| Error::Database(format!("failed to prepare list documents query: {e}")))?;

        let rows = stmt
            .query_map([], |row| {
                Ok(DocumentInfo {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    source_path: row.get(2)?,
                    mime_type: row.get(3)?,
                    chunk_count: row.get::<_, i64>(4).map(|c| c as usize)?,
                    created_at: row.get(5)?,
                })
            })
            .map_err(|e| Error::Database(format!("failed to list documents: {e}")))?;

        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| Error::Database(format!("failed to collect documents: {e}")))
    }

    /// Remove a document by name, cascading to its chunks. Returns true if
    /// a document was actually deleted.
    pub fn remove_document(&self, name: &str) -> Result<bool> {
        let conn = self.connection()?;
        let deleted = conn
            .execute("DELETE FROM documents WHERE name = ?", params![name])
            .map_err(|e| Error::Database(format!("failed to remove document: {e}")))?;

        if deleted > 0 {
            info!("removed document '{}'", name);
        }
        Ok(deleted > 0)
    }

    /// Look up a single document by its unique name.
    pub fn get_document_by_name(&self, name: &str) -> Result<Option<DocumentInfo>> {
        let conn = self.connection()?;

        let result = conn.query_row(
            "SELECT id, name, source_path, mime_type, chunk_count, created_at
             FROM documents
             WHERE name = ?",
            params![name],
            |row| {
                Ok(DocumentInfo {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    source_path: row.get(2)?,
                    mime_type: row.get(3)?,
                    chunk_count: row.get::<_, i64>(4).map(|c| c as usize)?,
                    created_at: row.get(5)?,
                })
            },
        );

        match result {
            Ok(info) => Ok(Some(info)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(Error::Database(format!(
                "failed to get document by name: {e}"
            ))),
        }
    }
}

// ---------------------------------------------------------------------------
// Embedding serialization helpers (same format as memory_store.rs)
// ---------------------------------------------------------------------------

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_memory_creates_document_tables() {
        let store = DocumentStore::in_memory().expect("failed to create in-memory document store");
        let conn = store.connection().expect("lock should not be poisoned");

        let doc_exists: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='documents'",
                [],
                |row| row.get(0),
            )
            .expect("failed to query sqlite_master for documents");
        assert_eq!(doc_exists, 1);

        let chunk_exists: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='document_chunks'",
                [],
                |row| row.get(0),
            )
            .expect("failed to query sqlite_master for document_chunks");
        assert_eq!(chunk_exists, 1);
    }

    #[test]
    fn add_document_and_list() {
        let store = DocumentStore::in_memory().expect("store");
        let id = store
            .add_document("test.pdf", Some("/tmp/test.pdf"), "application/pdf")
            .expect("add_document");
        assert!(!id.is_empty());

        let docs = store.list_documents().expect("list_documents");
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].name, "test.pdf");
        assert_eq!(docs[0].source_path.as_deref(), Some("/tmp/test.pdf"));
        assert_eq!(docs[0].mime_type, "application/pdf");
        assert_eq!(docs[0].chunk_count, 0);
    }

    #[test]
    fn add_chunks_and_update_count() {
        let store = DocumentStore::in_memory().expect("store");
        let doc_id = store
            .add_document("notes.txt", None, "text/plain")
            .expect("add_document");

        store
            .add_chunk(&doc_id, 0, "first chunk", None, None, None, None)
            .expect("add_chunk 0");
        store
            .add_chunk(&doc_id, 1, "second chunk", None, None, None, None)
            .expect("add_chunk 1");

        store.update_chunk_count(&doc_id, 2).expect("update count");

        let doc = store
            .get_document_by_name("notes.txt")
            .expect("get_document_by_name")
            .expect("document should exist");
        assert_eq!(doc.chunk_count, 2);
    }

    #[test]
    fn remove_document_cascades_chunks() {
        let store = DocumentStore::in_memory().expect("store");
        let doc_id = store
            .add_document("delete-me.md", None, "text/markdown")
            .expect("add_document");

        store
            .add_chunk(&doc_id, 0, "chunk text", None, None, None, None)
            .expect("add_chunk");

        let removed = store.remove_document("delete-me.md").expect("remove");
        assert!(removed);

        // Verify the document is gone
        let doc = store
            .get_document_by_name("delete-me.md")
            .expect("get_document_by_name");
        assert!(doc.is_none());

        // Verify chunks are gone (cascade)
        let conn = store.connection().expect("lock");
        let count: i64 = conn
            .query_row(
                "SELECT count(*) FROM document_chunks WHERE document_id = ?",
                params![doc_id],
                |row| row.get(0),
            )
            .expect("count query");
        assert_eq!(count, 0);
    }

    #[test]
    fn remove_nonexistent_document_returns_false() {
        let store = DocumentStore::in_memory().expect("store");
        let removed = store.remove_document("nope.txt").expect("remove");
        assert!(!removed);
    }

    #[test]
    fn get_document_by_name_returns_none_for_missing() {
        let store = DocumentStore::in_memory().expect("store");
        let doc = store
            .get_document_by_name("missing.pdf")
            .expect("get_document_by_name");
        assert!(doc.is_none());
    }

    #[test]
    fn search_chunks_by_embedding_similarity() {
        let store = DocumentStore::in_memory().expect("store");
        let doc_id = store
            .add_document("vectors.txt", None, "text/plain")
            .expect("add_document");

        // Chunk 0: embedding pointing in X direction
        store
            .add_chunk(
                &doc_id,
                0,
                "about cats",
                Some(&[1.0, 0.0, 0.0]),
                Some("test"),
                Some(3),
                Some(2),
            )
            .expect("chunk 0");

        // Chunk 1: embedding pointing in Y direction
        store
            .add_chunk(
                &doc_id,
                1,
                "about dogs",
                Some(&[0.0, 1.0, 0.0]),
                Some("test"),
                Some(3),
                Some(2),
            )
            .expect("chunk 1");

        // Chunk 2: no embedding (should be skipped)
        store
            .add_chunk(&doc_id, 2, "no embedding", None, None, None, None)
            .expect("chunk 2");

        store.update_chunk_count(&doc_id, 3).expect("update count");

        // Query close to X direction - should rank "about cats" first
        let results = store
            .search_chunks(&[0.95, 0.05, 0.0], 10, 0.0)
            .expect("search");
        assert_eq!(results.len(), 2); // chunk 2 excluded (no embedding)
        assert_eq!(results[0].text, "about cats");
        assert!(results[0].score > results[1].score);
        assert_eq!(results[0].document_name, "vectors.txt");

        // With a high min_similarity threshold, only the close match survives
        let filtered = store
            .search_chunks(&[0.95, 0.05, 0.0], 10, 0.9)
            .expect("search filtered");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].text, "about cats");
    }
}
