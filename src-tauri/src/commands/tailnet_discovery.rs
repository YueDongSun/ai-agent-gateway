//! Read-only Tailnet AAG discovery. Only one shared scan is admitted at a time.
use crate::{
    database::Database,
    services::tailnet_discovery::{
        self, Discovery_report, Discovery_status, Tailnet_discovery_service,
    },
};
use tauri::State;

#[tauri::command]
pub(crate) async fn tailnet_discovery_scan(
    db: State<'_, Database>,
    service: State<'_, Tailnet_discovery_service>,
    request_id: String,
) -> Result<Discovery_report, String> {
    service.scan(&db, request_id).await
}

#[tauri::command]
pub(crate) fn tailnet_discovery_status(
    db: State<'_, Database>,
    service: State<'_, Tailnet_discovery_service>,
) -> Result<Discovery_status, String> {
    Ok(service.status(tailnet_discovery::discovery_port(&db)?))
}

#[tauri::command]
pub(crate) fn tailnet_discovery_cancel(
    service: State<'_, Tailnet_discovery_service>,
    request_id: String,
) -> bool {
    service.cancel(&request_id)
}
