use opencrust_common::{Error, Result};
use rusqlite::Connection;
use std::path::Path;
use tracing::info;

/// Vector database for semantic search and memory embeddings.
/// Uses sqlite-vec for vector similarity operations.
pub struct VectorStore {
    conn: Connection,
}

impl VectorStore {
    pub fn open(db_path: &Path) -> Result<Self> {
        info!("opening vector store at {}", db_path.display());
        let conn = Connection::open(db_path)
            .map_err(|e| Error::Database(format!("failed to open vector database: {e}")))?;

        // TODO: Load sqlite-vec extension once available
        // unsafe { conn.load_extension("vec0", None) }

        let store = Self { conn };
        store.run_migrations()?;
        Ok(store)
    }

    fn run_migrations(&self) -> Result<()> {
        self.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS embeddings (
                    id TEXT PRIMARY KEY,
                    source TEXT NOT NULL,
                    content TEXT NOT NULL,
                    embedding BLOB,
                    metadata TEXT DEFAULT '{}',
                    created_at TEXT NOT NULL DEFAULT (datetime('now'))
                );",
            )
            .map_err(|e| Error::Database(format!("vector store migration failed: {e}")))?;

        Ok(())
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }
}
