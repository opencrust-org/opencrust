use opencrust_common::{Error, Result};
use rusqlite::{Connection, params};
use std::cmp::Ordering;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::migrations::{DOCUMENT_SCHEMA_V1, DOCUMENT_SCHEMA_V2};
use crate::vector_store::{ensure_sqlite_vec_registered, verify_vec_extension};

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

/// A chunk to insert into a document store.
#[derive(Debug, Clone, Copy)]
pub struct NewDocumentChunk<'a> {
    pub chunk_index: usize,
    pub text: &'a str,
    pub embedding: Option<&'a [f32]>,
    pub model: Option<&'a str>,
    pub dims: Option<usize>,
    pub token_count: Option<usize>,
}

/// Store for RAG document ingestion and vector-based retrieval.
///
/// Similarity search uses sqlite-vec KNN when available (fast, scales to
/// millions of chunks), falling back to in-Rust cosine similarity otherwise.
pub struct DocumentStore {
    conn: Mutex<Connection>,
    /// Whether sqlite-vec is loaded and functional on this connection.
    vec_enabled: bool,
}

impl DocumentStore {
    /// Open or create the document store at the given database path.
    pub fn open(db_path: &Path) -> Result<Self> {
        info!("opening document store at {}", db_path.display());
        let vec_enabled = ensure_sqlite_vec_registered();

        let conn = Connection::open(db_path)
            .map_err(|e| Error::Database(format!("failed to open document database: {e}")))?;

        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .map_err(|e| Error::Database(format!("failed to set pragmas: {e}")))?;

        let vec_enabled = if vec_enabled {
            verify_vec_extension(&conn)
        } else {
            false
        };

        let store = Self {
            conn: Mutex::new(conn),
            vec_enabled,
        };
        store.run_migrations()?;
        if store.vec_enabled {
            store.backfill_vec_index()?;
        }
        Ok(store)
    }

    /// Create an in-memory document store (useful for testing).
    pub fn in_memory() -> Result<Self> {
        let vec_enabled = ensure_sqlite_vec_registered();

        let conn = Connection::open_in_memory()
            .map_err(|e| Error::Database(format!("failed to open in-memory document db: {e}")))?;

        conn.execute_batch("PRAGMA foreign_keys=ON;")
            .map_err(|e| Error::Database(format!("failed to set pragmas: {e}")))?;

        let vec_enabled = if vec_enabled {
            verify_vec_extension(&conn)
        } else {
            false
        };

        let store = Self {
            conn: Mutex::new(conn),
            vec_enabled,
        };
        store.run_migrations()?;
        Ok(store)
    }

    /// Whether sqlite-vec KNN is active for this store.
    pub fn vec_enabled(&self) -> bool {
        self.vec_enabled
    }

    fn run_migrations(&self) -> Result<()> {
        let conn = self.connection()?;
        conn.execute_batch(DOCUMENT_SCHEMA_V1.sql)
            .map_err(|e| Error::Database(format!("document migration v1 failed: {e}")))?;
        conn.execute_batch(DOCUMENT_SCHEMA_V2.sql)
            .map_err(|e| Error::Database(format!("document migration v2 failed: {e}")))?;
        Ok(())
    }

    fn connection(&self) -> Result<MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| Error::Database("document store lock poisoned".into()))
    }

    // -----------------------------------------------------------------------
    // sqlite-vec helpers
    // -----------------------------------------------------------------------

    fn ensure_doc_vec_table_on_conn(conn: &Connection, dims: usize) -> Result<()> {
        let table = format!("vec_doc_chunks_{dims}");

        let exists: bool = conn
            .query_row(
                "SELECT count(*) > 0 FROM sqlite_master WHERE type='table' AND name=?",
                params![table],
                |row| row.get(0),
            )
            .map_err(|e| Error::Database(format!("failed to check vec table: {e}")))?;

        if !exists {
            // Use cosine distance so similarity = 1 - distance for unit vectors.
            let sql = format!(
                "CREATE VIRTUAL TABLE [{table}] \
                 USING vec0(embedding float[{dims}] distance_metric=cosine)"
            );
            conn.execute_batch(&sql)
                .map_err(|e| Error::Database(format!("failed to create vec table {table}: {e}")))?;
            info!("created vec0 table: {table} ({dims} dims)");
        }
        Ok(())
    }

    /// Insert a chunk embedding into the sqlite-vec index.
    /// Maps chunk UUID → integer rowid via `vec_doc_id_map`.
    fn insert_chunk_into_vec(&self, chunk_id: &str, embedding: &[f32], dims: usize) -> Result<()> {
        let conn = self.connection()?;
        Self::insert_chunk_into_vec_on_conn(&conn, chunk_id, embedding, dims)
    }

    fn insert_chunk_into_vec_on_conn(
        conn: &Connection,
        chunk_id: &str,
        embedding: &[f32],
        dims: usize,
    ) -> Result<()> {
        Self::ensure_doc_vec_table_on_conn(conn, dims)?;
        let table = format!("vec_doc_chunks_{dims}");
        let blob = embedding_to_blob(embedding);

        conn.execute(
            "INSERT OR IGNORE INTO vec_doc_id_map (chunk_id) VALUES (?)",
            params![chunk_id],
        )
        .map_err(|e| Error::Database(format!("failed to insert vec id mapping: {e}")))?;

        let rowid: i64 = conn
            .query_row(
                "SELECT rowid FROM vec_doc_id_map WHERE chunk_id = ?",
                params![chunk_id],
                |row| row.get(0),
            )
            .map_err(|e| Error::Database(format!("failed to get vec rowid: {e}")))?;

        conn.execute(
            &format!("INSERT OR REPLACE INTO [{table}] (rowid, embedding) VALUES (?, ?)"),
            params![rowid, blob],
        )
        .map_err(|e| Error::Database(format!("failed to insert into {table}: {e}")))?;

        Ok(())
    }

    /// Backfill the sqlite-vec index from existing `document_chunks` rows.
    /// Called once at `open()` to catch chunks ingested before vec was enabled.
    /// Skips chunks already present in `vec_doc_id_map`.
    fn backfill_vec_index(&self) -> Result<()> {
        // Collect chunks that have embeddings but are not yet in the vec index.
        let rows: Vec<(String, Vec<u8>, usize)> = {
            let conn = self.connection()?;
            let mut stmt = conn
                .prepare(
                    "SELECT c.id, c.embedding, c.embedding_dimensions
                     FROM document_chunks c
                     LEFT JOIN vec_doc_id_map m ON m.chunk_id = c.id
                     WHERE c.embedding IS NOT NULL
                       AND c.embedding_dimensions IS NOT NULL
                       AND m.rowid IS NULL",
                )
                .map_err(|e| Error::Database(format!("backfill prepare failed: {e}")))?;

            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Vec<u8>>(1)?,
                        row.get::<_, i64>(2)? as usize,
                    ))
                })
                .map_err(|e| Error::Database(format!("backfill query failed: {e}")))?;

            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| Error::Database(format!("backfill collect failed: {e}")))?
        };

        if rows.is_empty() {
            return Ok(());
        }

        info!("backfilling vec index for {} existing chunks", rows.len());
        let mut count = 0usize;
        for (chunk_id, blob, dims) in rows {
            match blob_to_embedding(&blob) {
                Ok(emb) => {
                    self.insert_chunk_into_vec(&chunk_id, &emb, dims)?;
                    count += 1;
                }
                Err(e) => {
                    warn!("skipping chunk {chunk_id} during backfill: {e}");
                }
            }
        }
        info!("vec backfill complete: {count} chunks indexed");
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Public API
    // -----------------------------------------------------------------------

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
    /// When `embedding` and `dims` are provided and sqlite-vec is enabled,
    /// the chunk is also inserted into the KNN index immediately.
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

        {
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
        }

        // Index into sqlite-vec immediately so searches reflect new chunks.
        if self.vec_enabled
            && let (Some(emb), Some(d)) = (embedding, dims)
        {
            self.insert_chunk_into_vec(&id, emb, d)?;
        }

        debug!(
            "added chunk {} for document {} (index {})",
            id, doc_id, chunk_index
        );
        Ok(id)
    }

    /// Add many chunks in a single SQLite transaction and update the parent
    /// document's cached chunk count. Returns generated chunk IDs in input order.
    pub fn add_chunks_batch(
        &self,
        doc_id: &str,
        chunks: &[NewDocumentChunk<'_>],
    ) -> Result<Vec<String>> {
        let ids = (0..chunks.len())
            .map(|_| Uuid::new_v4().to_string())
            .collect::<Vec<_>>();

        let mut conn = self.connection()?;
        let tx = conn
            .transaction()
            .map_err(|e| Error::Database(format!("failed to start chunk batch: {e}")))?;

        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO document_chunks (
                        id, document_id, chunk_index, text,
                        embedding, embedding_model, embedding_dimensions, token_count
                    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                )
                .map_err(|e| Error::Database(format!("failed to prepare chunk batch: {e}")))?;

            for (id, chunk) in ids.iter().zip(chunks.iter()) {
                let embedding_blob = chunk.embedding.map(embedding_to_blob);
                let dims_i64 = chunk.dims.map(|d| d as i64);
                let token_count_i64 = chunk.token_count.map(|t| t as i64);

                stmt.execute(params![
                    id,
                    doc_id,
                    chunk.chunk_index as i64,
                    chunk.text,
                    embedding_blob,
                    chunk.model,
                    dims_i64,
                    token_count_i64,
                ])
                .map_err(|e| Error::Database(format!("failed to insert document chunk: {e}")))?;
            }
        }

        if self.vec_enabled {
            for (id, chunk) in ids.iter().zip(chunks.iter()) {
                if let (Some(embedding), Some(dims)) = (chunk.embedding, chunk.dims) {
                    Self::insert_chunk_into_vec_on_conn(&tx, id, embedding, dims)?;
                }
            }
        }

        tx.execute(
            "UPDATE documents SET chunk_count = ? WHERE id = ?",
            params![chunks.len() as i64, doc_id],
        )
        .map_err(|e| Error::Database(format!("failed to update chunk count: {e}")))?;

        tx.commit()
            .map_err(|e| Error::Database(format!("failed to commit chunk batch: {e}")))?;

        debug!("added {} chunks for document {}", chunks.len(), doc_id);
        Ok(ids)
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

    /// Vector similarity search across document chunks.
    ///
    /// When sqlite-vec is enabled, uses KNN on the vec0 virtual table
    /// (O(log n)) instead of loading all embeddings into memory.
    /// Falls back to brute-force cosine similarity when sqlite-vec is
    /// unavailable (e.g. unsupported platform).
    pub fn search_chunks(
        &self,
        query_embedding: &[f32],
        limit: usize,
        min_similarity: f64,
    ) -> Result<Vec<DocumentChunk>> {
        if self.vec_enabled {
            self.search_chunks_knn(query_embedding, limit, min_similarity)
        } else {
            self.search_chunks_brute_force(query_embedding, limit, min_similarity)
        }
    }

    /// KNN search via sqlite-vec. Returns top-`limit` chunks with
    /// cosine similarity >= min_similarity.
    fn search_chunks_knn(
        &self,
        query_embedding: &[f32],
        limit: usize,
        min_similarity: f64,
    ) -> Result<Vec<DocumentChunk>> {
        let dims = query_embedding.len();
        let table = format!("vec_doc_chunks_{dims}");

        // If the vec table doesn't exist yet there are no indexed chunks.
        let table_exists: bool = {
            let conn = self.connection()?;
            conn.query_row(
                "SELECT count(*) > 0 FROM sqlite_master WHERE type='table' AND name=?",
                params![table],
                |row| row.get(0),
            )
            .map_err(|e| Error::Database(format!("failed to check vec table: {e}")))?
        };
        if !table_exists {
            return Ok(Vec::new());
        }

        let blob = embedding_to_blob(query_embedding);
        // Cosine distance = 1 - cosine_similarity for unit vectors.
        // min_similarity threshold → max_distance = 1 - min_similarity.
        // Fetch extra candidates to absorb any filtering loss.
        let fetch_k = (limit * 2).max(limit + 8);
        let max_distance = 1.0 - min_similarity;

        let knn_results: Vec<(String, f64)> = {
            let conn = self.connection()?;
            let mut stmt = conn
                .prepare(&format!(
                    "SELECT m.chunk_id, v.distance
                     FROM [{table}] v
                     JOIN vec_doc_id_map m ON m.rowid = v.rowid
                     WHERE v.embedding MATCH ? AND k = ?"
                ))
                .map_err(|e| Error::Database(format!("failed to prepare KNN query: {e}")))?;

            let rows = stmt
                .query_map(params![blob, fetch_k as i64], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
                })
                .map_err(|e| Error::Database(format!("KNN query failed: {e}")))?;

            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| Error::Database(format!("failed to collect KNN results: {e}")))?
        };

        // Apply similarity threshold and convert distance → score.
        let candidate_ids: Vec<(String, f64)> = knn_results
            .into_iter()
            .filter(|(_, dist)| *dist <= max_distance)
            .map(|(id, dist)| (id, 1.0 - dist))
            .take(limit)
            .collect();

        if candidate_ids.is_empty() {
            return Ok(Vec::new());
        }

        // Fetch text and metadata for the matched chunks.
        let placeholders: String = candidate_ids
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 1))
            .collect::<Vec<_>>()
            .join(", ");

        let mut chunks: Vec<DocumentChunk> = {
            let conn = self.connection()?;
            let sql = format!(
                "SELECT c.id, c.document_id, d.name, c.chunk_index, c.text
                 FROM document_chunks c
                 JOIN documents d ON d.id = c.document_id
                 WHERE c.id IN ({placeholders})"
            );
            let mut stmt = conn
                .prepare(&sql)
                .map_err(|e| Error::Database(format!("failed to prepare chunk fetch: {e}")))?;

            let id_values: Vec<&dyn rusqlite::ToSql> = candidate_ids
                .iter()
                .map(|(id, _)| id as &dyn rusqlite::ToSql)
                .collect();

            let rows = stmt
                .query_map(id_values.as_slice(), |row| {
                    Ok(DocumentChunk {
                        id: row.get(0)?,
                        document_id: row.get(1)?,
                        document_name: row.get(2)?,
                        chunk_index: row.get::<_, i64>(3)? as usize,
                        text: row.get(4)?,
                        score: 0.0,
                    })
                })
                .map_err(|e| Error::Database(format!("failed to fetch chunk text: {e}")))?;

            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| Error::Database(format!("failed to collect chunks: {e}")))?
        };

        // Attach scores from the KNN results and sort by descending similarity.
        for chunk in &mut chunks {
            if let Some((_, score)) = candidate_ids.iter().find(|(id, _)| id == &chunk.id) {
                chunk.score = *score;
            }
        }
        chunks.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(Ordering::Equal));

        Ok(chunks)
    }

    /// Brute-force cosine similarity search. Loads all embeddings into memory.
    /// Used as fallback when sqlite-vec is not available.
    fn search_chunks_brute_force(
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
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Vec<u8>>(5)?,
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

    /// Search document chunks by keyword with term-frequency ranking.
    ///
    /// Candidates are found via `LIKE` (broad net), then ranked by
    /// `text_match_score` so partial-term matches are ordered by relevance
    /// rather than all returning `score = 1.0`. Chunks with zero term overlap
    /// are excluded.
    pub fn keyword_search_chunks(&self, query: &str, limit: usize) -> Result<Vec<DocumentChunk>> {
        let conn = self.connection()?;

        // Broaden the LIKE pattern to any chunk that contains at least one
        // query word, then rank in Rust where we have full scoring logic.
        let terms: Vec<String> = query
            .split_whitespace()
            .map(|t| t.to_lowercase())
            .filter(|t| !t.is_empty())
            .collect();

        if terms.is_empty() {
            return Ok(Vec::new());
        }

        // Build OR-of-LIKE conditions: text LIKE '%term1%' OR text LIKE '%term2%' ...
        let conditions: String = terms
            .iter()
            .enumerate()
            .map(|(i, _)| format!("LOWER(c.text) LIKE ?{}", i + 1))
            .collect::<Vec<_>>()
            .join(" OR ");

        let sql = format!(
            "SELECT c.id, c.document_id, d.name, c.chunk_index, c.text
             FROM document_chunks c
             JOIN documents d ON d.id = c.document_id
             WHERE {conditions}
             LIMIT ?{}",
            terms.len() + 1
        );

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| Error::Database(format!("failed to prepare keyword search: {e}")))?;

        // Bind each term pattern then the limit.
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = terms
            .iter()
            .map(|t| {
                let p = format!("%{}%", t.replace('%', "\\%").replace('_', "\\_"));
                Box::new(p) as Box<dyn rusqlite::ToSql>
            })
            .collect();
        params.push(Box::new(limit as i64 * 4)); // fetch extra for post-ranking

        let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();

        let rows = stmt
            .query_map(param_refs.as_slice(), |row| {
                Ok(DocumentChunk {
                    id: row.get(0)?,
                    document_id: row.get(1)?,
                    document_name: row.get(2)?,
                    chunk_index: row.get::<_, i64>(3)? as usize,
                    text: row.get(4)?,
                    score: 0.0,
                })
            })
            .map_err(|e| Error::Database(format!("failed to execute keyword search: {e}")))?;

        let mut scored: Vec<DocumentChunk> = rows
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| Error::Database(format!("failed to read keyword search rows: {e}")))?
            .into_iter()
            .filter_map(|mut chunk| {
                let s = text_match_score(query, &chunk.text);
                if s > 0.0 {
                    chunk.score = s as f64;
                    Some(chunk)
                } else {
                    None
                }
            })
            .collect();

        scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(Ordering::Equal));
        scored.truncate(limit);
        Ok(scored)
    }

    /// Hybrid search: combine vector similarity with text match scoring.
    ///
    /// When an embedding is available, the final score is a weighted blend of
    /// cosine similarity (0.8) and term-frequency text match (0.2), improving
    /// results when the vector model underweights exact keyword occurrences.
    ///
    /// When no embedding is provided, falls back to `keyword_search_chunks`.
    pub fn hybrid_search_chunks(
        &self,
        query: &str,
        query_embedding: Option<&[f32]>,
        limit: usize,
        min_similarity: f64,
    ) -> Result<Vec<DocumentChunk>> {
        let Some(embedding) = query_embedding else {
            return self.keyword_search_chunks(query, limit);
        };

        // Get vector candidates (fetch extra to re-rank).
        let fetch_k = (limit * 2).max(limit + 8);
        let mut candidates = self.search_chunks(embedding, fetch_k, min_similarity * 0.8)?;

        // Re-score with hybrid: 0.8 * vector + 0.2 * text_match.
        for chunk in &mut candidates {
            let text_score = text_match_score(query, &chunk.text) as f64;
            chunk.score = chunk.score * 0.8 + text_score * 0.2;
        }

        candidates.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(Ordering::Equal));

        // Apply final similarity threshold and cap at limit.
        candidates.retain(|c| c.score >= min_similarity);
        candidates.truncate(limit);
        Ok(candidates)
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

    /// Remove a document by name, cascading to its chunks.
    /// Also cleans up the corresponding vec index mapping entries.
    /// Returns true if a document was actually deleted.
    pub fn remove_document(&self, name: &str) -> Result<bool> {
        // Collect chunk IDs before deletion so we can clean up the vec index.
        let chunk_ids: Vec<String> = {
            let conn = self.connection()?;
            let doc_id: Option<String> = conn
                .query_row(
                    "SELECT id FROM documents WHERE name = ?",
                    params![name],
                    |row| row.get(0),
                )
                .ok();

            if let Some(did) = doc_id {
                let mut stmt = conn
                    .prepare("SELECT id FROM document_chunks WHERE document_id = ?")
                    .map_err(|e| {
                        Error::Database(format!("failed to list chunks for removal: {e}"))
                    })?;
                let rows = stmt
                    .query_map(params![did], |row| row.get::<_, String>(0))
                    .map_err(|e| Error::Database(format!("failed to query chunks: {e}")))?;
                rows.collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(|e| Error::Database(format!("failed to collect chunk ids: {e}")))?
            } else {
                Vec::new()
            }
        };

        // Delete the document (chunks cascade automatically).
        let deleted = {
            let conn = self.connection()?;
            conn.execute("DELETE FROM documents WHERE name = ?", params![name])
                .map_err(|e| Error::Database(format!("failed to remove document: {e}")))?
        };

        // Clean up vec_doc_id_map entries (vec0 orphaned rows are harmless but
        // removing from the map keeps index entries from being returned by KNN).
        if deleted > 0 && !chunk_ids.is_empty() {
            let conn = self.connection()?;
            for chunk_id in &chunk_ids {
                let _ = conn.execute(
                    "DELETE FROM vec_doc_id_map WHERE chunk_id = ?",
                    params![chunk_id],
                );
            }
        }

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

    /// Search documents whose name contains `pattern` (case-insensitive, partial match).
    /// Returns matching documents ordered by `created_at DESC`.
    pub fn search_documents_by_name(&self, pattern: &str) -> Result<Vec<DocumentInfo>> {
        let conn = self.connection()?;
        let like_pattern = format!(
            "%{}%",
            pattern
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );

        let mut stmt = conn
            .prepare(
                "SELECT id, name, source_path, mime_type, chunk_count, created_at
                 FROM documents
                 WHERE LOWER(name) LIKE LOWER(?) ESCAPE '\\'
                 ORDER BY created_at DESC",
            )
            .map_err(|e| Error::Database(format!("failed to prepare name search: {e}")))?;

        let rows = stmt
            .query_map(params![like_pattern], |row| {
                Ok(DocumentInfo {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    source_path: row.get(2)?,
                    mime_type: row.get(3)?,
                    chunk_count: row.get::<_, i64>(4).map(|c| c as usize)?,
                    created_at: row.get(5)?,
                })
            })
            .map_err(|e| Error::Database(format!("failed to execute name search: {e}")))?;

        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| Error::Database(format!("failed to collect name search results: {e}")))
    }

    /// Return all chunks for a given document ID, ordered by `chunk_index`.
    /// Score is set to `1.0` (direct lookup, not a ranked search).
    pub fn get_chunks_by_document_id(&self, document_id: &str) -> Result<Vec<DocumentChunk>> {
        let conn = self.connection()?;

        let mut stmt = conn
            .prepare(
                "SELECT c.id, c.document_id, d.name, c.chunk_index, c.text
                 FROM document_chunks c
                 JOIN documents d ON d.id = c.document_id
                 WHERE c.document_id = ?
                 ORDER BY c.chunk_index ASC",
            )
            .map_err(|e| {
                Error::Database(format!("failed to prepare chunk fetch by doc id: {e}"))
            })?;

        let rows = stmt
            .query_map(params![document_id], |row| {
                Ok(DocumentChunk {
                    id: row.get(0)?,
                    document_id: row.get(1)?,
                    document_name: row.get(2)?,
                    chunk_index: row.get::<_, i64>(3)? as usize,
                    text: row.get(4)?,
                    score: 1.0,
                })
            })
            .map_err(|e| Error::Database(format!("failed to execute chunk fetch: {e}")))?;

        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| Error::Database(format!("failed to collect chunks by doc id: {e}")))
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

/// Term-frequency text match score in [0, 1].
///
/// Returns 1.0 if the full query appears verbatim (case-insensitive),
/// otherwise returns the fraction of query terms found in `content`.
fn text_match_score(query: &str, content: &str) -> f32 {
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
    let matches = terms.iter().filter(|t| content_lc.contains(**t)).count();
    matches as f32 / terms.len() as f32
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

        let map_exists: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='vec_doc_id_map'",
                [],
                |row| row.get(0),
            )
            .expect("failed to query sqlite_master for vec_doc_id_map");
        assert_eq!(map_exists, 1);
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
    fn add_chunks_batch_inserts_chunks_and_updates_count() {
        let store = DocumentStore::in_memory().expect("store");
        let doc_id = store
            .add_document("batch-notes.txt", None, "text/plain")
            .expect("add_document");

        store
            .add_chunks_batch(
                &doc_id,
                &[
                    NewDocumentChunk {
                        chunk_index: 0,
                        text: "first chunk",
                        embedding: None,
                        model: None,
                        dims: None,
                        token_count: Some(2),
                    },
                    NewDocumentChunk {
                        chunk_index: 1,
                        text: "second chunk",
                        embedding: None,
                        model: None,
                        dims: None,
                        token_count: Some(2),
                    },
                ],
            )
            .expect("add_chunks_batch");

        let doc = store
            .get_document_by_name("batch-notes.txt")
            .expect("get_document_by_name")
            .expect("document should exist");
        assert_eq!(doc.chunk_count, 2);

        let chunks = store
            .get_chunks_by_document_id(&doc_id)
            .expect("get_chunks_by_document_id");
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].chunk_index, 0);
        assert_eq!(chunks[0].text, "first chunk");
        assert_eq!(chunks[1].chunk_index, 1);
        assert_eq!(chunks[1].text, "second chunk");
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

        let doc = store
            .get_document_by_name("delete-me.md")
            .expect("get_document_by_name");
        assert!(doc.is_none());

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

    #[test]
    fn remove_document_cleans_vec_index() {
        let store = DocumentStore::in_memory().expect("store");
        let doc_id = store
            .add_document("cleanup.txt", None, "text/plain")
            .expect("add_document");

        store
            .add_chunk(
                &doc_id,
                0,
                "some text",
                Some(&[1.0, 0.0, 0.0]),
                Some("test"),
                Some(3),
                None,
            )
            .expect("chunk");

        let removed = store.remove_document("cleanup.txt").expect("remove");
        assert!(removed);

        // vec_doc_id_map should be empty after removal
        let conn = store.connection().expect("lock");
        let count: i64 = conn
            .query_row("SELECT count(*) FROM vec_doc_id_map", [], |row| row.get(0))
            .expect("count");
        assert_eq!(count, 0);
    }

    #[test]
    fn get_chunks_by_document_id_returns_all_chunks_ordered() {
        let store = DocumentStore::in_memory().expect("store");
        let doc_id = store
            .add_document("CLAUDE.md", None, "text/markdown")
            .expect("add_document");
        store
            .add_chunk(&doc_id, 0, "first chunk", None, None, None, None)
            .expect("chunk 0");
        store
            .add_chunk(&doc_id, 1, "second chunk", None, None, None, None)
            .expect("chunk 1");
        store
            .add_chunk(&doc_id, 2, "third chunk", None, None, None, None)
            .expect("chunk 2");

        let chunks = store
            .get_chunks_by_document_id(&doc_id)
            .expect("get chunks");
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].chunk_index, 0);
        assert_eq!(chunks[1].chunk_index, 1);
        assert_eq!(chunks[2].chunk_index, 2);
        assert_eq!(chunks[0].document_name, "CLAUDE.md");
        assert!((chunks[0].score - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn get_chunks_by_document_id_returns_empty_for_unknown_id() {
        let store = DocumentStore::in_memory().expect("store");
        let chunks = store
            .get_chunks_by_document_id("no-such-id")
            .expect("get chunks");
        assert!(chunks.is_empty());
    }

    #[test]
    fn search_documents_by_name_partial_and_case_insensitive() {
        let store = DocumentStore::in_memory().expect("store");
        store
            .add_document("CLAUDE.md", None, "text/markdown")
            .expect("doc 1");
        store
            .add_document("readme.md", None, "text/markdown")
            .expect("doc 2");
        store
            .add_document("report.pdf", None, "application/pdf")
            .expect("doc 3");

        let md_docs = store.search_documents_by_name(".md").expect("search");
        assert_eq!(md_docs.len(), 2);

        let claude_docs = store.search_documents_by_name("claude").expect("search");
        assert_eq!(claude_docs.len(), 1);
        assert_eq!(claude_docs[0].name, "CLAUDE.md");

        let none = store
            .search_documents_by_name("nonexistent")
            .expect("search");
        assert!(none.is_empty());
    }
}
