//! Prompt-enhancement IPC.
//!
//! The pipeline itself lives in services::prompt; this module only bridges
//! Tauri state (the database and the running gateway) to it, so the domain
//! stays testable without a Tauri runtime.

use crate::commands::proxy::RunningProxy;
use crate::database::Database;
use crate::services::prompt;
use tokio::sync::Mutex;

/// Run ONE stage of the prompt-enhancement pipeline against a model from the
/// local AAG pool.
///
/// One stage per call is deliberate: the UI can render the Chinese brief while
/// the English stage is still running, and the user can edit the brief and
/// re-run only the second stage. Nothing is persisted on the backend.
#[tauri::command]
pub(crate) async fn prompt_enhance_run(
    db: tauri::State<'_, Database>,
    state: tauri::State<'_, Mutex<Option<RunningProxy>>>,
    request: prompt::Prompt_stage_request,
) -> Result<prompt::Prompt_stage_output, String> {
    // Scope the gateway lock: the pipeline awaits upstream I/O and must not
    // hold the proxy state mutex while it runs.
    let router = {
        let guard = state.lock().await;
        guard.as_ref().and_then(RunningProxy::live_router)
    };
    prompt::run_stage(db.inner(), router, request).await
}

/// Built-in stage templates, settings keys, and limits, so the frontend can
/// offer template overrides without duplicating prompt text.
#[tauri::command]
pub(crate) fn prompt_enhance_defaults() -> prompt::Prompt_defaults {
    prompt::defaults()
}
