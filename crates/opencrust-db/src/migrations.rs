/// Migration system for tracking and applying database schema changes.
///
/// Each migration has a version number and a SQL statement.
/// Migrations are applied in order and tracked in a `_migrations` table.
pub struct Migration {
    pub version: u32,
    pub name: &'static str,
    pub sql: &'static str,
}

pub const MEMORY_SCHEMA_V1_SQL: &str = "
CREATE TABLE IF NOT EXISTS memory_entries (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    channel_id TEXT,
    user_id TEXT,
    continuity_key TEXT,
    role TEXT NOT NULL,
    content TEXT NOT NULL,
    embedding BLOB,
    embedding_model TEXT,
    embedding_dimensions INTEGER,
    metadata TEXT DEFAULT '{}',
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_memory_session_created_at
    ON memory_entries(session_id, created_at);

CREATE INDEX IF NOT EXISTS idx_memory_continuity_created_at
    ON memory_entries(continuity_key, created_at);

CREATE INDEX IF NOT EXISTS idx_memory_role
    ON memory_entries(role, created_at);
";

pub const MEMORY_SCHEMA_V1: Migration = Migration {
    version: 1,
    name: "memory_schema_v1",
    sql: MEMORY_SCHEMA_V1_SQL,
};

pub const DOCUMENT_SCHEMA_V1_SQL: &str = "
CREATE TABLE IF NOT EXISTS documents (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    source_path TEXT,
    mime_type TEXT,
    chunk_count INTEGER DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS document_chunks (
    id TEXT PRIMARY KEY,
    document_id TEXT NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
    chunk_index INTEGER NOT NULL,
    text TEXT NOT NULL,
    embedding BLOB,
    embedding_model TEXT,
    embedding_dimensions INTEGER,
    token_count INTEGER,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_doc_chunks_document ON document_chunks(document_id, chunk_index);
";

pub const DOCUMENT_SCHEMA_V1: Migration = Migration {
    version: 2,
    name: "document_schema_v1",
    sql: DOCUMENT_SCHEMA_V1_SQL,
};

pub const USAGE_SCHEMA_V1_SQL: &str = "
CREATE TABLE IF NOT EXISTS usage_log (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    provider TEXT NOT NULL,
    model TEXT NOT NULL,
    input_tokens INTEGER NOT NULL DEFAULT 0,
    output_tokens INTEGER NOT NULL DEFAULT 0,
    recorded_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_usage_session ON usage_log(session_id, recorded_at);
CREATE INDEX IF NOT EXISTS idx_usage_recorded_at ON usage_log(recorded_at);
";

pub const USAGE_SCHEMA_V1: Migration = Migration {
    version: 3,
    name: "usage_schema_v1",
    sql: USAGE_SCHEMA_V1_SQL,
};

/// Idempotent column additions to usage_log for per-user budget queries.
pub const USAGE_SCHEMA_V2_COLUMNS: &[(&str, &str)] = &[("user_id", "TEXT"), ("channel_id", "TEXT")];

pub const USAGE_SCHEMA_V2_INDEX_SQL: &str = "
CREATE INDEX IF NOT EXISTS idx_usage_user_recorded_at
    ON usage_log(user_id, recorded_at);
";
