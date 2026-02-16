use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Error, Debug)]
pub enum Error {
    #[error("configuration error: {0}")]
    Config(String),

    #[error("channel error: {0}")]
    Channel(String),

    #[error("agent error: {0}")]
    Agent(String),

    #[error("database error: {0}")]
    Database(String),

    #[error("plugin error: {0}")]
    Plugin(String),

    #[error("security error: {0}")]
    Security(String),

    #[error("media error: {0}")]
    Media(String),

    #[error("gateway error: {0}")]
    Gateway(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("unauthorized: {0}")]
    Unauthorized(String),

    #[error("{0}")]
    Other(String),
}
