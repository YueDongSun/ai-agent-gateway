//! Async IPC adapters for launcher services. Blocking filesystem/process work
//! runs on Tauri's worker pool, never on the native UI thread.
use crate::services::mirror_sync::Mirror_sync_service;
use crate::services::provider::switching::Provider_switch_service;
use crate::services::session_authority::client_config::{self, ManagedClientContract};
use crate::services::tailnet_publication::Publication_service;
use crate::{database::Database, services::launcher};
use launcher::{
    ClaudeMirrorStatusDto, CodexCliStatusDto, Codex_mirror_session_audit, Managed_cli_status,
    VsCodeExtensionStatusDto,
};

async fn sync_instructions(
    db: &Database,
    service: &Mirror_sync_service,
    client: &'static str,
) -> Result<(), String> {
    if !crate::services::mirror_sync::instruction_sync_enabled(db).map_err(String::from)? {
        return Ok(());
    }
    let database = db.clone();
    let service = service.clone();
    run(move || service.reconcile(&database).for_client(client)).await
}

async fn run<T: Send + 'static>(
    operation: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(operation)
        .await
        .map_err(|_| "launcher_worker_failed".to_string())?
}

/// Capture listener/auth/model metadata from one accepting runtime. The
/// protected owner attestation runs on the blocking pool before any legacy
/// instruction sync, supplier rewrite or client-home mutation can occur.
async fn managed_codex_runtime(
    db: &Database,
    state: &tokio::sync::Mutex<Option<crate::commands::proxy::RunningProxy>>,
) -> Result<Option<(ManagedClientContract, Vec<String>)>, String> {
    if !client_config::managed_launch_required(db)? {
        return Ok(None);
    }
    let (port, key, models) = {
        let guard = state.lock().await;
        let running = guard
            .as_ref()
            .ok_or_else(|| "session_client_gateway_unavailable".to_string())?;
        let port = running
            .actual_port()
            .ok_or_else(|| "session_client_gateway_unavailable".to_string())?;
        let runtime = running
            .publication_state()
            .ok_or_else(|| "session_client_gateway_unavailable".to_string())?;
        let models = running
            .credentialless_models(crate::proxy::router::SURFACE_CODEX)
            .ok_or_else(|| "desktop_mirror_models_unavailable".to_string())?;
        (port, runtime.auth_token, models)
    };
    let database = db.clone();
    let contract = run(move || client_config::get_managed_contract(&database, port, key))
        .await?
        .ok_or_else(|| "session_client_authority_unavailable".to_string())?;
    Ok(Some((contract, models)))
}

#[tauri::command]
pub(crate) async fn managed_cli_status(app_type: String) -> Result<Managed_cli_status, String> {
    run(move || launcher::managed_cli_status(app_type)).await
}

#[tauri::command]
pub(crate) async fn managed_cli_open_terminal(
    db: tauri::State<'_, Database>,
    app_type: String,
    cwd: Option<String>,
) -> Result<serde_json::Value, String> {
    let database = db.inner().clone();
    run(move || launcher::managed_cli_open_terminal(&database, app_type, cwd)).await
}

#[tauri::command]
pub(crate) async fn codex_cli_status(
    db: tauri::State<'_, Database>,
) -> Result<CodexCliStatusDto, String> {
    let database = db.inner().clone();
    run(move || launcher::codex_cli_status(&database)).await
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Codex_reasoning_model {
    model: String,
    supported_efforts: Vec<String>,
    is_default: bool,
}

/// The same generated metadata used by the native Codex picker, with no
/// provider connection/authentication fields exposed to the form.
#[tauri::command]
pub(crate) async fn codex_reasoning_models(
    db: tauri::State<'_, Database>,
    provider_id: Option<String>,
) -> Result<Vec<Codex_reasoning_model>, String> {
    let database = db.inner().clone();
    run(move || {
        let provider = provider_id
            .as_deref()
            .map(|id| database.get_provider_by_id(id, "codex"))
            .transpose()
            .map_err(String::from)?
            .flatten();
        if provider_id.is_some() && provider.is_none() {
            return Err("codex_provider_not_found".into());
        }
        let (_, bytes) =
            launcher::codex_catalog::build_codex_model_catalog(&database, provider.as_ref(), None)
                .map_err(String::from)?;
        let catalog: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| "codex_catalog_invalid".to_string())?;
        Ok(catalog["models"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|entry| Codex_reasoning_model {
                model: entry["slug"].as_str().unwrap_or_default().into(),
                is_default: entry["priority"].as_u64() == Some(0),
                supported_efforts: entry["supported_reasoning_levels"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|level| level["effort"].as_str().map(str::to_owned))
                    .collect(),
            })
            .collect())
    })
    .await
}

#[tauri::command]
pub(crate) async fn codex_open_cli_terminal(
    db: tauri::State<'_, Database>,
    sync: tauri::State<'_, Mirror_sync_service>,
    state: tauri::State<'_, tokio::sync::Mutex<Option<crate::commands::proxy::RunningProxy>>>,
    cwd: Option<String>,
) -> Result<serde_json::Value, String> {
    if let Some((contract, models)) = managed_codex_runtime(&db, &state).await? {
        let database = db.inner().clone();
        return run(move || {
            launcher::codex_open_cli_terminal(&database, cwd, Some(&contract), Some(&models))
        })
        .await;
    }
    sync_instructions(&db, &sync, "codex").await?;
    let database = db.inner().clone();
    run(move || launcher::codex_open_cli_terminal(&database, cwd, None, None)).await
}

#[tauri::command]
pub(crate) async fn codex_open_desktop_window(
    db: tauri::State<'_, Database>,
    sync: tauri::State<'_, Mirror_sync_service>,
    state: tauri::State<'_, tokio::sync::Mutex<Option<crate::commands::proxy::RunningProxy>>>,
    publication: tauri::State<'_, Publication_service>,
    mirror_writes: tauri::State<'_, Provider_switch_service>,
) -> Result<serde_json::Value, String> {
    if let Some((contract, models)) = managed_codex_runtime(&db, &state).await? {
        let database = db.inner().clone();
        return run(move || {
            launcher::codex_open_desktop_window(&database, Some(&models), Some(&contract))
        })
        .await;
    }
    sync_instructions(&db, &sync, "codex").await?;
    prepare_desktop_runtime(&db, &state, &publication, &mirror_writes).await?;
    let models = state
        .lock()
        .await
        .as_ref()
        .and_then(|running| running.credentialless_models(crate::proxy::router::SURFACE_CODEX))
        .ok_or_else(|| "desktop_mirror_models_unavailable".to_string())?;
    let database = db.inner().clone();
    let owned_mirror_writes = mirror_writes.inner().clone();
    run(move || {
        owned_mirror_writes.with_client_write(|| {
            launcher::codex_open_desktop_window(&database, Some(&models), None)
        })
    })
    .await
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
pub(crate) async fn gemini_open_cli_terminal(
    db: tauri::State<'_, Database>,
    cwd: Option<String>,
) -> Result<serde_json::Value, String> {
    let database = db.inner().clone();
    run(move || launcher::gemini_open_cli_terminal(&database, cwd)).await
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
    mirror_writes: tauri::State<'_, Provider_switch_service>,
    mode: String,
) -> Result<serde_json::Value, String> {
    let database = db.inner().clone();
    let owned_mirror_writes = mirror_writes.inner().clone();
    run(move || {
        owned_mirror_writes.with_client_write(|| launcher::codex_mirror_set_mode(&database, mode))
    })
    .await
}

#[tauri::command]
pub(crate) async fn claude_mirror_set_mode(
    db: tauri::State<'_, Database>,
    mirror_writes: tauri::State<'_, Provider_switch_service>,
    mode: String,
) -> Result<serde_json::Value, String> {
    let database = db.inner().clone();
    let owned_mirror_writes = mirror_writes.inner().clone();
    run(move || {
        owned_mirror_writes.with_client_write(|| launcher::claude_mirror_set_mode(&database, mode))
    })
    .await
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
    sync: tauri::State<'_, Mirror_sync_service>,
    state: tauri::State<'_, tokio::sync::Mutex<Option<crate::commands::proxy::RunningProxy>>>,
    publication: tauri::State<'_, Publication_service>,
    mirror_writes: tauri::State<'_, Provider_switch_service>,
) -> Result<serde_json::Value, String> {
    sync_instructions(&db, &sync, "claude").await?;
    prepare_desktop_runtime(&db, &state, &publication, &mirror_writes).await?;
    let pin = db
        .get_setting(crate::database::dao::settings::KEY_PROXY_PINNED_PROVIDER)
        .map_err(String::from)?;
    if state
        .lock()
        .await
        .as_ref()
        .and_then(|running| running.live_router())
        .is_none_or(|router| router.desktop_models(pin.as_deref()).is_empty())
    {
        return Err("desktop_mirror_models_unavailable".into());
    }
    let database = db.inner().clone();
    let owned_mirror_writes = mirror_writes.inner().clone();
    run(move || owned_mirror_writes.with_client_write(|| launcher::claude_open_desktop(&database)))
        .await
}

#[tauri::command]
pub(crate) async fn claude_open_cli_terminal(
    db: tauri::State<'_, Database>,
    sync: tauri::State<'_, Mirror_sync_service>,
    cwd: Option<String>,
) -> Result<serde_json::Value, String> {
    sync_instructions(&db, &sync, "claude").await?;
    let database = db.inner().clone();
    run(move || launcher::claude_open_cli_terminal(&database, cwd)).await
}

/// Bring up the listener before serializing its effective endpoint, then
/// reconcile current supplier/model selection on every explicit desktop open.
async fn prepare_desktop_runtime(
    db: &Database,
    state: &tokio::sync::Mutex<Option<crate::commands::proxy::RunningProxy>>,
    publication: &Publication_service,
    mirror_writes: &Provider_switch_service,
) -> Result<(), String> {
    crate::commands::proxy::activate_supplier_runtime(db, state, publication).await?;
    super::suppliers::reconcile_supplier_setup(db, state, publication, mirror_writes).await
}
