//! Prompt workspace IPC. File content remains inside the backend/model request.
use crate::{database::Database, services::prompt::workspaces::{self, Workspace_context_preview, Workspace_context_selection, Workspace_registry, Workspace_save_input}};

#[tauri::command]
pub(crate) fn prompt_workspace_list(db: tauri::State<'_, Database>) -> Result<Workspace_registry, String> { workspaces::list(&db) }

#[tauri::command]
pub(crate) async fn prompt_workspace_save(db: tauri::State<'_, Database>, input: Workspace_save_input) -> Result<Workspace_registry, String> {
    let database = db.inner().clone();
    tauri::async_runtime::spawn_blocking(move || workspaces::save(&database, input)).await.map_err(|_| "prompt_workspace_worker_failed".to_string())?
}

#[tauri::command]
pub(crate) fn prompt_workspace_select(db: tauri::State<'_, Database>, id: Option<String>, expected_revision: u64) -> Result<Workspace_registry, String> { workspaces::select(&db, id, expected_revision) }

#[tauri::command]
pub(crate) fn prompt_workspace_remove(db: tauri::State<'_, Database>, id: String, expected_revision: u64) -> Result<Workspace_registry, String> { workspaces::remove(&db, &id, expected_revision) }

#[tauri::command]
pub(crate) async fn prompt_workspace_preview(db: tauri::State<'_, Database>, selection: Workspace_context_selection) -> Result<Workspace_context_preview, String> {
    let database = db.inner().clone();
    tauri::async_runtime::spawn_blocking(move || workspaces::preview(&database, selection)).await.map_err(|_| "prompt_workspace_worker_failed".to_string())?
}
