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
    db.get_setting(&key).map_err(String::from)
}

/// Write a device-level setting. An empty value deletes the key.
#[tauri::command]
pub(crate) fn settings_set(
    db: tauri::State<'_, Database>,
    key: String,
    value: String,
) -> Result<(), String> {
    db.set_setting(&key, &value).map_err(String::from)
}

