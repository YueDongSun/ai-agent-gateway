//! Thin manager-only MCP IPC. Private definitions never cross the read surface.
use crate::{
    database::Database,
    services::mcp::{
        Mcp_client, Mcp_server_input, Mcp_server_summary, Mcp_service, Mcp_sync_preview,
        Mcp_sync_receipt,
    },
};
use tauri::Manager;

#[tauri::command(rename_all = "snake_case")]
pub(crate) fn mcp_discard_review(
    service: tauri::State<'_, Mcp_service>,
    review_token: String,
) -> Result<(), String> {
    service.discard_review(&review_token).map_err(String::from)
}

#[tauri::command(rename_all = "snake_case")]
pub(crate) fn mcp_list_servers(
    service: tauri::State<'_, Mcp_service>,
    db: tauri::State<'_, Database>,
) -> Result<Vec<Mcp_server_summary>, String> {
    service.list(db.inner()).map_err(String::from)
}

#[tauri::command(rename_all = "snake_case")]
pub(crate) fn mcp_save_server(
    service: tauri::State<'_, Mcp_service>,
    db: tauri::State<'_, Database>,
    input: Mcp_server_input,
) -> Result<Mcp_server_summary, String> {
    service.save(db.inner(), input).map_err(String::from)
}

#[tauri::command(rename_all = "snake_case")]
pub(crate) fn mcp_delete_server(
    service: tauri::State<'_, Mcp_service>,
    db: tauri::State<'_, Database>,
    id: String,
) -> Result<(), String> {
    service.delete(db.inner(), &id).map_err(String::from)
}

#[tauri::command(rename_all = "snake_case")]
pub(crate) fn mcp_set_enabled(
    service: tauri::State<'_, Mcp_service>,
    db: tauri::State<'_, Database>,
    id: String,
    client: Mcp_client,
    enabled: bool,
) -> Result<Mcp_server_summary, String> {
    service
        .set_enabled(db.inner(), &id, client, enabled)
        .map_err(String::from)
}

#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn mcp_preview_sync(
    app: tauri::AppHandle,
    db: tauri::State<'_, Database>,
    client: Mcp_client,
) -> Result<Mcp_sync_preview, String> {
    let database = db.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<Mcp_service>().preview(&database, client)
    })
    .await
    .map_err(|_| "mcp:preview_worker_failed".to_string())?
    .map_err(String::from)
}

#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn mcp_apply_sync(
    app: tauri::AppHandle,
    db: tauri::State<'_, Database>,
    review_token: String,
) -> Result<Mcp_sync_receipt, String> {
    let database = db.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        app.state::<Mcp_service>().apply(&database, &review_token)
    })
    .await
    .map_err(|_| "mcp:apply_worker_failed".to_string())?
    .map_err(String::from)
}
