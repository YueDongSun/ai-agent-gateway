//! Application error type.
//!
//! Modeled after cc-switch's `AppError` but trimmed to the variants AAG needs.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("config error: {0}")]
    Config(String),

    #[error("invalid input: {0}")]
    InvalidInput(String),

    #[error("io error at {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("database error: {0}")]
    Database(String),

    #[error("lock acquisition failed: {0}")]
    Lock(String),

    #[error("{0}")]
    Message(String),
}

impl From<rusqlite::Error> for AppError {
    fn from(e: rusqlite::Error) -> Self {
        AppError::Database(e.to_string())
    }
}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        AppError::Database(e.to_string())
    }
}

/// Tauri commands return `Result<T, String>`; this converts AppError to a String.
impl From<AppError> for String {
    fn from(e: AppError) -> String {
        e.to_string()
    }
}

/// Convenience: wrap an io::Error with a path context. Accepts `&str`,
/// `String`, `&Path`, `PathBuf`, etc.
impl AppError {
    pub fn io(path: impl AsRef<std::path::Path>, source: std::io::Error) -> Self {
        AppError::Io {
            path: path.as_ref().to_string_lossy().into_owned(),
            source,
        }
    }
}