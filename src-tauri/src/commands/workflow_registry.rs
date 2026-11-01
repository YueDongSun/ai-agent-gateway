//! Manager IPC for capability inventory and bounded explicit observations.
use crate::{
    database::Database,
    services::{
        mcp::Mcp_service,
        workflow_registry::{
            types::{Capability_list, Health_snapshot, Route_preview},
            Workflow_service,
        },
    },
};

#[tauri::command(rename_all = "snake_case")]
pub(crate) fn workflow_list_capabilities(
    service: tauri::State<'_, Workflow_service>,
    db: tauri::State<'_, Database>,
    mcp: tauri::State<'_, Mcp_service>,
) -> Result<Capability_list, String> {
    service.list_capabilities(db.inner(), mcp.inner())
}

#[tauri::command(rename_all = "snake_case")]
pub(crate) fn workflow_get_health(
    service: tauri::State<'_, Workflow_service>,
    db: tauri::State<'_, Database>,
    mcp: tauri::State<'_, Mcp_service>,
) -> Result<Health_snapshot, String> {
    service.health(db.inner(), mcp.inner())
}

#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn workflow_route_preview(
    service: tauri::State<'_, Workflow_service>,
    task: String,
) -> Result<Route_preview, String> {
    let config_root = service.config_root().to_path_buf();
    tauri::async_runtime::spawn_blocking(move || {
        Workflow_service::new(config_root).route_preview(&task)
    })
    .await
    .map_err(|_| "workflow:route_worker_failed".to_string())
}

#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn workflow_probe_capability(
    app: tauri::AppHandle,
    capability_id: String,
) -> Result<Health_snapshot, String> {
    use tauri::Manager;
    tauri::async_runtime::spawn_blocking(move || {
        let service = app.state::<Workflow_service>();
        let db = app.state::<Database>();
        let mcp = app.state::<Mcp_service>();
        service.probe_capability(db.inner(), mcp.inner(), &capability_id)
    })
    .await
    .map_err(|_| "workflow:probe_worker_failed".to_string())?
}

#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn workflow_evaluate_route(
    service: tauri::State<'_, Workflow_service>,
    task: String,
    external_advice_confirmed: Option<bool>,
) -> Result<Route_preview, String> {
    Ok(service.evaluate_route(&task, external_advice_confirmed.unwrap_or(false)).await)
}
