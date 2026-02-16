/// Migration system for tracking and applying database schema changes.
///
/// Each migration has a version number and a SQL statement.
/// Migrations are applied in order and tracked in a `_migrations` table.
pub struct Migration {
    pub version: u32,
    pub name: &'static str,
    pub sql: &'static str,
}

pub const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    name: "initial_schema",
    sql: "-- Applied inline in session_store and vector_store",
}];
