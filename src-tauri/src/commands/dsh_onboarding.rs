//! Passive detection and review-bound auto setup for the trusted manager only.
use crate::{
    database::Database,
    services::dsh::{
        setup::{Dsh_setup_plan, Dsh_setup_request},
        setup_preferences, upstreams, Dsh_discovery, Dsh_service, Dsh_snapshot,
    },
};
use tokio::sync::Mutex;

#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn dsh_discover_runtime(
    service: tauri::State<'_, Dsh_service>,
) -> Result<Dsh_discovery, String> {
    let service = service.inner().clone();
    tauri::async_runtime::spawn_blocking(move || service.discover_runtime())
        .await
        .map_err(|_| "dsh:discovery_worker_failed".to_string())?
        .map_err(String::from)
}
#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn dsh_preview_setup(
    service: tauri::State<'_, Dsh_service>,
    db: tauri::State<'_, Database>,
    proxy: tauri::State<'_, Mutex<Option<super::proxy::RunningProxy>>>,
    request: Dsh_setup_request,
) -> Result<Dsh_setup_plan, String> {
    let gateway = super::proxy::dsh_gateway_snapshot(proxy.inner()).await?;
    let options = upstreams::list_upstreams(db.inner(), gateway.as_ref()).map_err(String::from)?;
    let (source, model) =
        setup_preferences::preferred_native(db.inner(), &options).map_err(String::from)?;
    let service = service.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        service.preview_setup(request, &options, source.as_deref(), model.as_deref())
    })
    .await
    .map_err(|_| "dsh:setup_worker_failed".to_string())?
    .map_err(String::from)
}
#[tauri::command(rename_all = "snake_case")]
pub(crate) async fn dsh_apply_setup(
    service: tauri::State<'_, Dsh_service>,
    db: tauri::State<'_, Database>,
    proxy: tauri::State<'_, Mutex<Option<super::proxy::RunningProxy>>>,
    plan_id: String,
) -> Result<Dsh_snapshot, String> {
    let gateway = super::proxy::dsh_gateway_snapshot(proxy.inner()).await?;
    let options = upstreams::list_upstreams(db.inner(), gateway.as_ref()).map_err(String::from)?;
    let service = service.inner().clone();
    tauri::async_runtime::spawn_blocking(move || service.apply_setup(plan_id, &options))
        .await
        .map_err(|_| "dsh:setup_worker_failed".to_string())?
        .map_err(String::from)
}
