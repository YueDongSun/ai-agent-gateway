//! Model auto-discovery IPC commands.

use serde::Serialize;
use tauri::State;

use crate::database::Database;
use crate::services::model_discovery::{self, RefreshReport, Trigger};

/// Discovery panel snapshot: last stored report, whether changed routing
/// state has reached the live snapshot, and the periodic schedule.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DiscoveryStatus {
    pub report: Option<RefreshReport>,
    /// True when changed routing state has not reached a live request
    /// snapshot yet. Kept as `pendingRestart` for UI compatibility; a
    /// successful hot reload clears it without restarting the listener.
    pub pending_restart: bool,
    pub interval_hours: i64,
    pub last_run_epoch: i64,
}

#[tauri::command]
pub(crate) fn model_discovery_status(db: State<'_, Database>) -> Result<DiscoveryStatus, String> {
    Ok(DiscoveryStatus {
        report: model_discovery::stored_report(&db),
        pending_restart: model_discovery::pending_restart(&db),
        interval_hours: model_discovery::refresh_interval_hours(&db),
        last_run_epoch: model_discovery::last_run_epoch(&db),
    })
}

/// Manual refresh: one provider when `provider_id` is given, all otherwise.
#[tauri::command]
pub(crate) async fn proxy_refresh_models(
    provider_id: Option<String>,
    db: State<'_, Database>,
    proxy: State<'_, tokio::sync::Mutex<Option<crate::commands::proxy::RunningProxy>>>,
) -> Result<RefreshReport, String> {
    let running = proxy.lock().await.is_some();
    let report = model_discovery::run_refresh(
        db.inner(),
        Trigger::Manual,
        running,
        provider_id.as_deref(),
    )
    .await
    .map_err(String::from)?;
    if report.has_changes {
        crate::commands::proxy::runtime_reload(&proxy).await?;
        if running {
            crate::services::model_discovery::clear_pending_restart(&db);
        }
    }
    Ok(report)
}

#[tauri::command]
pub(crate) fn proxy_set_auto_track(
    provider_id: String,
    enabled: bool,
    db: State<'_, Database>,
    proxy: State<'_, tokio::sync::Mutex<Option<crate::commands::proxy::RunningProxy>>>,
) -> Result<(), String> {
    let mut pp = db
        .get_proxy_provider(&provider_id)
        .map_err(String::from)?
        .ok_or_else(|| "proxy_provider_not_found".to_string())?;
    pp.auto_track_models = enabled;
    db.save_proxy_provider(&pp).map_err(String::from)?;
    let _ = tauri::async_runtime::block_on(crate::commands::proxy::runtime_reload(&proxy));
    Ok(())
}
