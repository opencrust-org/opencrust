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
