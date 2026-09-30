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
        e.external_message()
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

    /// Stable IPC-facing error text. Variant payloads are retained only for
    /// internal control flow and Debug inspection; they may contain paths,
    /// URLs, provider responses, database details, or credentials.
    pub fn external_message(&self) -> String {
        match self {
            AppError::Config(detail) => stable_detail(detail)
                .unwrap_or("configuration_error")
                .to_string(),
            AppError::InvalidInput(detail) => {
                stable_detail(detail).unwrap_or("invalid_input").to_string()
            }
            AppError::Io { .. } => "io_error".to_string(),
            AppError::Json(_) => "json_error".to_string(),
            AppError::Database(_) => "database_error".to_string(),
            AppError::Lock(_) => "lock_error".to_string(),
            AppError::ConflictWithCcSwitch { .. } => "cc_switch_conflict".to_string(),
            AppError::Message(detail) => stable_detail(detail)
                .unwrap_or("operation_failed")
                .to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipc_conversion_never_exposes_app_error_details() {
        let private_path = r"C:\Users\private-user\secret.txt";
        let errors = [
            AppError::Config("https://private.example.invalid?token=secret".into()),
            AppError::InvalidInput("private.person@example.invalid".into()),
            AppError::io(private_path, std::io::Error::other("sk-private-value")),
            AppError::Database("100.64.12.34".into()),
            AppError::Lock("private-node.tail123.ts.net".into()),
            AppError::Message("CANARY_RESPONSE_UNIQUE_92".into()),
            AppError::Message("secret".into()),
        ];
        for error in errors {
            let external = String::from(error);
            assert!(!external.contains("private"));
            assert!(!external.contains("CANARY_RESPONSE_UNIQUE_92"));
            assert!(!external.contains("100.64.12.34"));
            assert!(!external.contains("sk-"));
            assert!(!external.contains("C:\\Users"));
            assert_ne!(external, "secret");
        }
    }
}

fn stable_detail(value: &str) -> Option<&str> {
    if value.is_empty() || value.len() > 96 {
        return None;
    }
    if ["DSH_", "MCP_", "AAG_"]
        .iter()
        .any(|prefix| value.starts_with(prefix))
        && value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Some(value);
    }
    let components: Vec<_> = value.split(':').collect();
    let valid = components.iter().all(|component| {
        !component.is_empty()
            && component.len() <= 48
            && component
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    });
    (valid
        && (2..=3).contains(&components.len())
        && ["dsh", "dsh_market", "mcp", "private_storage"].contains(&components[0]))
    .then_some(value)
}
