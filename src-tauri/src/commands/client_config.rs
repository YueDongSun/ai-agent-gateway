//! Read-only app/client configuration path inspection.
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
