//! Read-only app/client configuration path inspection plus the scoped
//! direct-text edit surface (Kilo family, cc-switch style).
use crate::{database::Database, services::provider::client_config};

#[tauri::command]
pub(crate) async fn provider_config_status(
    db: tauri::State<'_, Database>,
    app_type: String,
) -> Result<client_config::Provider_config_status, String> {
    let database = db.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        client_config::provider_config_status(&database, &app_type)
    })
    .await
    .map_err(|_| "provider_config_worker_failed".to_string())?
    .map_err(String::from)
}

/// Read the live Kilo config file verbatim for in-app editing.
#[tauri::command]
pub(crate) async fn client_config_read(
    app_type: String,
) -> Result<crate::services::provider::config_editor::Config_edit_target, String> {
    tauri::async_runtime::spawn_blocking(move || {
        crate::services::provider::config_editor::client_config_read(&app_type)
    })
    .await
    .map_err(|_| "client_config_read_worker_failed".to_string())?
    .map_err(String::from)
}

/// Validate edited text (JSON/JSONC, object root) and write it back to the
/// live Kilo config file atomically, with a backup of the previous file.
#[tauri::command]
pub(crate) async fn client_config_save(
    app_type: String,
    contents: String,
) -> Result<crate::services::provider::LiveWriteResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        crate::services::provider::config_editor::client_config_save(&app_type, &contents)
    })
    .await
    .map_err(|_| "client_config_save_worker_failed".to_string())?
    .map_err(String::from)
}
