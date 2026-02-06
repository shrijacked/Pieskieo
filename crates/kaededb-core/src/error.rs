use thiserror::Error;

#[derive(Debug, Error)]
pub enum KaedeDbError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    Serde(#[from] bincode::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("not found")]
    NotFound,
}

pub type Result<T> = std::result::Result<T, KaedeDbError>;
