//! Async IPC adapters for launcher services. Blocking filesystem/process work
//! runs on Tauri's worker pool, never on the native UI thread.
use crate::{database::Database, services::launcher};
use launcher::{
    ClaudeMirrorStatusDto, CodexCliStatusDto, Codex_mirror_session_audit, VsCodeExtensionStatusDto,
};

async fn run<T: Send + 'static>(
    operation: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(operation)
        .await
        .map_err(|_| "launcher_worker_failed".to_string())?
}

#[tauri::command]
pub(crate) async fn codex_cli_status(
    db: tauri::State<'_, Database>,
) -> Result<CodexCliStatusDto, String> {
    let database = db.inner().clone();
    run(move || launcher::codex_cli_status(&database)).await
}

#[tauri::command]
pub(crate) async fn codex_open_cli_terminal(
    db: tauri::State<'_, Database>,
    cwd: Option<String>,
) -> Result<serde_json::Value, String> {
    let database = db.inner().clone();
    run(move || launcher::codex_open_cli_terminal(&database, cwd)).await
}

#[tauri::command]
pub(crate) async fn codex_open_desktop_window(
    db: tauri::State<'_, Database>,
) -> Result<serde_json::Value, String> {
    let database = db.inner().clone();
    run(move || launcher::codex_open_desktop_window(&database)).await
}

#[tauri::command]
pub(crate) async fn copilot_open_cli_terminal(
    db: tauri::State<'_, Database>,
    cwd: Option<String>,
) -> Result<serde_json::Value, String> {
    let database = db.inner().clone();
    run(move || launcher::copilot_open_cli_terminal(&database, cwd)).await
}

#[tauri::command]
pub(crate) async fn vscode_open_with_extension(
    db: tauri::State<'_, Database>,
    app_type: String,
) -> Result<serde_json::Value, String> {
    let database = db.inner().clone();
    run(move || launcher::vscode_open_with_extension(&database, app_type)).await
}

#[tauri::command]
pub(crate) async fn codex_mirror_session_reset(
    db: tauri::State<'_, Database>,
) -> Result<(), String> {
    let database = db.inner().clone();
    run(move || launcher::codex_mirror_session_reset(&database)).await
}

#[tauri::command]
pub(crate) async fn codex_mirror_session_audit(
    db: tauri::State<'_, Database>,
) -> Result<Codex_mirror_session_audit, String> {
    let database = db.inner().clone();
    run(move || launcher::codex_mirror_session_audit(&database)).await
}

#[tauri::command]
pub(crate) async fn vscode_extension_status() -> Result<VsCodeExtensionStatusDto, String> {
    run(launcher::vscode_extension_status).await
}

#[tauri::command]
pub(crate) async fn codex_mirror_set_mode(
    db: tauri::State<'_, Database>,
    mode: String,
) -> Result<serde_json::Value, String> {
    let database = db.inner().clone();
    run(move || launcher::codex_mirror_set_mode(&database, mode)).await
}

#[tauri::command]
pub(crate) async fn claude_mirror_set_mode(
    db: tauri::State<'_, Database>,
    mode: String,
) -> Result<serde_json::Value, String> {
    let database = db.inner().clone();
    run(move || launcher::claude_mirror_set_mode(&database, mode)).await
}

#[tauri::command]
pub(crate) async fn claude_mirror_status(
    db: tauri::State<'_, Database>,
) -> Result<ClaudeMirrorStatusDto, String> {
    let database = db.inner().clone();
    run(move || launcher::claude_mirror_status(&database)).await
}

#[tauri::command]
pub(crate) async fn claude_open_desktop(
    db: tauri::State<'_, Database>,
) -> Result<serde_json::Value, String> {
    let database = db.inner().clone();
    run(move || launcher::claude_open_desktop(&database)).await
}

#[tauri::command]
pub(crate) async fn claude_open_cli_terminal(
    db: tauri::State<'_, Database>,
    cwd: Option<String>,
) -> Result<serde_json::Value, String> {
    let database = db.inner().clone();
    run(move || launcher::claude_open_cli_terminal(&database, cwd)).await
}
