//! Device-level settings commands (key/value store).
//!
//! Extracted from `lib.rs` in the file-structure refactor - the command
//! bodies are moved verbatim; only module paths in `use` statements and
//! `pub(crate)` visibility changed.

use crate::database::Database;

// ----- App settings (Codex CLI isolation etc.) -----

/// Read a device-level setting (app_meta table).
#[tauri::command]
pub(crate) fn settings_get(
    db: tauri::State<'_, Database>,
    key: String,
) -> Result<Option<String>, String> {
    if crate::database::dao::provider_admission::is_private_admission_setting(&key) {
        return Err("provider_admission_private_setting".into());
    }
    db.get_setting(&key).map_err(String::from)
}

/// Write a device-level setting. An empty value deletes the key.
#[tauri::command]
pub(crate) fn settings_set(
    db: tauri::State<'_, Database>,
    key: String,
    value: String,
) -> Result<(), String> {
    if crate::database::dao::provider_admission::is_private_admission_setting(&key) {
        return Err("provider_admission_private_setting".into());
    }
    if key == crate::services::provider_screening::SCREENING_CONFIG_KEY && !value.trim().is_empty() {
        crate::services::provider_screening::ScreeningConfig::parse(&value)?;
    }
    if key == crate::services::model_catalog::utility::POLICY_SETTING && !value.trim().is_empty() {
        crate::services::model_catalog::utility::Utility_policy::parse(&value).map_err(str::to_string)?;
    }
    db.set_setting(&key, &value).map_err(String::from)
}
