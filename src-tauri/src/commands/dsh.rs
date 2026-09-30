//! Thin DSH IPC; resolve credentials only in Rust immediately before launch.
use crate::{
    database::Database,
    services::dsh::{
        upstreams, Dsh_operation_preview, Dsh_operation_request, Dsh_receipt, Dsh_service,
        Dsh_settings, Dsh_snapshot,
    },
};
use std::collections::BTreeMap;
use tokio::sync::Mutex;

#[tauri::command(rename_all = "snake_case")]
pub(crate) fn dsh_get_snapshot(
    service: tauri::State<'_, Dsh_service>,
) -> Result<Dsh_snapshot, String> {
    service.snapshot().map_err(String::from)
}
#[tauri::command(rename_all = "snake_case")]
pub(crate) fn dsh_save_settings(
    service: tauri::State<'_, Dsh_service>,
    settings: Dsh_settings,
    expected_digest: String,
) -> Result<Dsh_snapshot, String> {
    service
        .save_settings(settings, expected_digest)
        .map_err(String::from)
}
#[tauri::command(rename_all = "snake_case")]
pub(crate) fn dsh_preview_operation(
    service: tauri::State<'_, Dsh_service>,
    request: Dsh_operation_request,
) -> Result<Dsh_operation_preview, String> {
    service.preview(request).map_err(String::from)
}
#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn dsh_execute_operation(
    service: tauri::State<'_, Dsh_service>,
    preview_id: String,
) -> Result<Dsh_receipt, String> {
    let owned_service = service.inner().clone();
    tauri::async_runtime::spawn_blocking(move || owned_service.execute(preview_id))
        .await
        .map_err(|_| "dsh:operation_worker_failed".to_string())?
        .map_err(String::from)
}
#[tauri::command(rename_all = "snake_case")]
pub(crate) fn dsh_cancel_operation(service: tauri::State<'_, Dsh_service>) -> bool {
    service.cancel()
}
#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn dsh_list_upstreams(
    db: tauri::State<'_, Database>,
    proxy: tauri::State<'_, Mutex<Option<super::proxy::RunningProxy>>>,
) -> Result<Vec<upstreams::Upstream_option>, String> {
    let gateway = super::proxy::dsh_gateway_snapshot(proxy.inner()).await?;
    upstreams::list_upstreams(db.inner(), gateway.as_ref()).map_err(String::from)
}
#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn dsh_start(
    service: tauri::State<'_, Dsh_service>,
    db: tauri::State<'_, Database>,
    proxy: tauri::State<'_, Mutex<Option<super::proxy::RunningProxy>>>,
) -> Result<Dsh_snapshot, String> {
    let snapshot = service.snapshot().map_err(String::from)?;
    let settings = snapshot.settings;
    let (llm_settings, secret_env) = match (
        settings.upstream_id.as_deref(),
        settings.model_id.as_deref(),
    ) {
        (None, None) => (serde_json::json!({}), BTreeMap::new()),
        (Some(source_id), Some(model)) => {
            let gateway = super::proxy::dsh_gateway_snapshot(proxy.inner()).await?;
            let resolved =
                upstreams::resolve_upstream(db.inner(), source_id, model, gateway.as_ref())
                    .map_err(String::from)?;
            (
                resolved.llm_settings,
                resolved.secret_env.into_iter().collect(),
            )
        }
        _ => return Err("dsh:incomplete_upstream_selection".into()),
    };
    service
        .start(llm_settings, secret_env, snapshot.settings_digest)
        .await
        .map_err(String::from)
}
#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn dsh_stop(
    service: tauri::State<'_, Dsh_service>,
    app: tauri::AppHandle,
) -> Result<Dsh_snapshot, String> {
    use tauri::Manager;
    for (label, window) in app.webview_windows() {
        if label.starts_with("dsh_web_") {
            window.close().map_err(|_| "dsh:window_close_failed")?;
        }
    }
    let owned_service = service.inner().clone();
    tauri::async_runtime::spawn_blocking(move || owned_service.stop())
        .await
        .map_err(|_| "dsh:stop_worker_failed".to_string())?
        .map_err(String::from)
}
#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn dsh_open_web_ui(
    service: tauri::State<'_, Dsh_service>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let web_url = service.validated_web_url().map_err(String::from)?;
    crate::dsh_window::open(&app, &web_url)
}
