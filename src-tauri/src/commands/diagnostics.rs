//! Tauri commands for local product diagnostics.

use crate::database::Database;
use crate::diagnostics::{self, Diagnostics_runtime, Diagnostics_state, Request_event};

#[tauri::command]
pub(crate) fn diagnostics_get(
    app: tauri::AppHandle,
    runtime: tauri::State<'_, Diagnostics_runtime>,
) -> Diagnostics_state {
    diagnostics::state(&app, runtime.inner())
}

#[tauri::command]
pub(crate) fn diagnostics_enable_debug(
    app: tauri::AppHandle,
    db: tauri::State<'_, Database>,
    runtime: tauri::State<'_, Diagnostics_runtime>,
) -> Result<Diagnostics_state, String> {
    diagnostics::enable_debug(&app, db.inner(), runtime.inner())
}

#[tauri::command]
pub(crate) fn diagnostics_disable_debug(
    app: tauri::AppHandle,
    db: tauri::State<'_, Database>,
    runtime: tauri::State<'_, Diagnostics_runtime>,
) -> Result<Diagnostics_state, String> {
    diagnostics::disable_debug(&app, db.inner(), runtime.inner())
}

#[tauri::command]
pub(crate) fn diagnostics_open_log_dir(app: tauri::AppHandle) -> Result<(), String> {
    diagnostics::open_log_directory(&app)
}

#[tauri::command]
pub(crate) fn diagnostics_get_summary(
    app: tauri::AppHandle,
    runtime: tauri::State<'_, Diagnostics_runtime>,
) -> String {
    diagnostics::summary(&app, runtime.inner())
}

#[tauri::command]
pub(crate) fn diagnostics_record_frontend_error(
    kind: String,
    message_chars: usize,
    stack_chars: usize,
    correlation_id: String,
) {
    diagnostics::record_frontend_error(&kind, message_chars, stack_chars, &correlation_id);
}

#[tauri::command]
pub(crate) fn diagnostics_request_timeline() -> Result<Vec<Request_event>, String> {
    Ok(diagnostics::request_timeline())
}

#[tauri::command]
pub(crate) fn diagnostics_clear_request_timeline() -> Result<(), String> {
    diagnostics::clear_request_timeline();
    Ok(())
}
