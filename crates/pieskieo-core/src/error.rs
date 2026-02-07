use thiserror::Error;

#[derive(Debug, Error)]
pub enum PieskieoError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    Serde(#[from] bincode::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("not found")]
    NotFound,
    #[error("wrong shard")]
    WrongShard,
    #[error("validation error: {0}")]
    Validation(String),
    #[error("unique constraint violation on field '{0}'")]
    UniqueViolation(String),
    #[error("internal error: {0}")]
    Internal(String),
}

pub type Result<T> = std::result::Result<T, PieskieoError>;
