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

    /// cc-switch coexistence conflict: AAG would write `target_url`
    /// (an AAG proxy URL) into a live agent config, but cc-switch's own
    /// proxy is reachable at `cc_switch_port` — the two would overwrite
    /// each other's `ANTHROPIC_BASE_URL` / `OPENAI_BASE_URL`.
    ///
    /// Frontend matches on the literal "conflict with cc-switch" prefix to
    /// surface a structured dialog (see `ui/src/components/common/CcSwitchDialog.tsx`).
    #[error(
        "conflict with cc-switch (running at 127.0.0.1:{cc_switch_port}); \
         AAG would write {target_url} but cc-switch is already proxying — \
         please disable cc-switch's proxy or enable 'ignore cc-switch conflict' in settings"
    )]
    ConflictWithCcSwitch {
        cc_switch_port: u16,
        target_url: String,
    },

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