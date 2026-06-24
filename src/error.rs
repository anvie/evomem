use thiserror::Error;

#[derive(Debug, Error)]
pub enum EvoError {
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("invalid frontmatter in {path}: {message}")]
    Frontmatter { path: String, message: String },

    #[error("knowledge store is not initialized at {0}; run `evomem init` first")]
    NotInitialized(String),

    #[error("embedder mismatch: database was built with `{stored}`, current is `{current}`; reinitialize the knowledge store")]
    EmbedderMismatch { stored: String, current: String },

    #[error("page not found: {0}")]
    PageNotFound(String),

    #[error("server error: {0}")]
    Server(String),

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, EvoError>;
