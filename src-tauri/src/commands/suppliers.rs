//! Supplier CRUD and status IPC commands.
//!
//! Command registration is owned by the application integration layer; add
//! these handlers from `lib.rs::generate_handler!` after wiring the module.

use crate::database::Database;
use crate::services::provider::switching::Provider_switch_service;
use crate::services::suppliers::{
    self as supplier_service, SupplierDiscoverModelsInput, SupplierGetResult, SupplierListView,
    SupplierModelsUpdateInput, SupplierMutationResult, SupplierSaveInput, SupplierStatus,
};
use crate::services::tailnet_publication::Publication_service;
use tauri::async_runtime::Mutex;
use tauri::State;

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SupplierSaveResult {
    #[serde(flatten)]
    result: SupplierMutationResult,
    setup_error: Option<String>,
    discovery_error: Option<String>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SupplierDeleteResult {
    generation: u64,
    setup_error: Option<String>,
}

#[tauri::command]
pub(crate) async fn supplier_discover_models(
    db: State<'_, Database>,
    input: SupplierDiscoverModelsInput,
) -> Result<Vec<String>, String> {
    supplier_service::discover_supplier_models(&db, input)
        .await
        .map_err(supplier_service::ipc_error)
}

#[tauri::command]
pub(crate) fn supplier_list(db: State<'_, Database>) -> Result<SupplierListView, String> {
    supplier_service::list_suppliers(&db).map_err(supplier_service::ipc_error)
}

#[tauri::command]
pub(crate) fn supplier_get(
    db: State<'_, Database>,
    id: String,
) -> Result<SupplierGetResult, String> {
    supplier_service::get_supplier(&db, &id).map_err(supplier_service::ipc_error)
}

#[tauri::command]
pub(crate) async fn supplier_save(
    db: State<'_, Database>,
    state: State<'_, Mutex<Option<crate::commands::proxy::RunningProxy>>>,
    publication: State<'_, Publication_service>,
    mirror_writes: State<'_, Provider_switch_service>,
    input: SupplierSaveInput,
) -> Result<SupplierSaveResult, String> {
    save_and_setup_supplier(&db, &state, &publication, &mirror_writes, input).await
}

async fn save_and_setup_supplier(
    db: &Database,
    state: &Mutex<Option<crate::commands::proxy::RunningProxy>>,
    publication: &Publication_service,
    mirror_writes: &Provider_switch_service,
    input: SupplierSaveInput,
) -> Result<SupplierSaveResult, String> {
    let mut result =
        supplier_service::save_supplier(&db, input).map_err(supplier_service::ipc_error)?;
    let discovery_error = supplier_service::revalidate_suppliers(db, &[result.supplier_id.clone()])
        .await
        .err();
    result.generation = db
        .supplier_config_generation()
        .map_err(supplier_service::ipc_error)?;
    let setup = reconcile_supplier_setup(db, state, publication, mirror_writes).await;
    Ok(SupplierSaveResult {
        result,
        setup_error: setup.err(),
        discovery_error,
    })
}

/// Shared by every mutation so removing a selected model cannot leave clients stale.
pub(crate) async fn reconcile_supplier_setup(
    db: &Database,
    state: &Mutex<Option<crate::commands::proxy::RunningProxy>>,
    publication: &Publication_service,
    mirror_writes: &Provider_switch_service,
) -> Result<(), String> {
    let selected = crate::services::supplier_setup::selected_model(db).map_err(String::from)?;
    db.set_setting(
        crate::services::supplier_setup::KEY_UNIFIED_DEFAULT_MODEL,
        selected.as_deref().unwrap_or(""),
    )
    .map_err(String::from)?;
    if selected.is_some() {
        crate::commands::proxy::activate_supplier_runtime(db, state, publication).await?;
    } else {
        crate::commands::proxy::runtime_reload(state).await?;
    }
    let codex_models = state
        .lock()
        .await
        .as_ref()
        .and_then(|running| running.credentialless_models(crate::proxy::router::SURFACE_CODEX))
        .unwrap_or_default();
    let database = db.clone();
    let owned_mirror_writes = mirror_writes.clone();
    let client_result = tauri::async_runtime::spawn_blocking(move || {
        owned_mirror_writes.with_client_write(|| {
            crate::services::supplier_setup::configure_mirrors_for_runtime(
                &database,
                Some(&codex_models),
            )
        })
    })
    .await
    .map_err(|_| "supplier_client_setup_worker_failed".to_string())?
    .map_err(String::from);
    // Managed launcher selections are routing inputs too. Reconcile their
    // committed records before claiming the saved configuration is active.
    let runtime_result = crate::commands::proxy::runtime_reload(state).await;
    client_result?;
    runtime_result
}

#[tauri::command]
pub(crate) async fn supplier_delete(
    db: State<'_, Database>,
    state: State<'_, Mutex<Option<crate::commands::proxy::RunningProxy>>>,
    publication: State<'_, Publication_service>,
    mirror_writes: State<'_, Provider_switch_service>,
    id: String,
    expected_generation: u64,
) -> Result<SupplierDeleteResult, String> {
    let generation = supplier_service::delete_supplier(&db, &id, expected_generation)
        .map_err(supplier_service::ipc_error)?;
    let setup_error = reconcile_supplier_setup(&db, &state, &publication, &mirror_writes)
        .await
        .err();
    Ok(SupplierDeleteResult {
        generation,
        setup_error,
    })
}

#[tauri::command]
pub(crate) async fn supplier_models_update(
    db: State<'_, Database>,
    state: State<'_, Mutex<Option<crate::commands::proxy::RunningProxy>>>,
    publication: State<'_, Publication_service>,
    mirror_writes: State<'_, Provider_switch_service>,
    input: SupplierModelsUpdateInput,
) -> Result<SupplierSaveResult, String> {
    let result = supplier_service::update_supplier_models(&db, input)
        .map_err(supplier_service::ipc_error)?;
    let setup_error = reconcile_supplier_setup(&db, &state, &publication, &mirror_writes)
        .await
        .err();
    Ok(SupplierSaveResult {
        result,
        setup_error,
        discovery_error: None,
    })
}

#[tauri::command]
pub(crate) fn supplier_status(db: State<'_, Database>) -> Result<SupplierStatus, String> {
    supplier_service::supplier_status(&db).map_err(supplier_service::ipc_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{routing::post, Json, Router};
    use serde_json::{json, Value};

    #[tokio::test]
    async fn model_updates_and_deletion_reconcile_default_model_and_mirrors() {
        let home =
            crate::services::provider::test_support::isolated_home_async("supplier-reconcile")
                .await;
        let db = Database::memory().unwrap();
        let mut config = db.get_proxy_config().unwrap();
        config.port = 0;
        config.auth_token = "fixture_gateway_token".into();
        db.save_proxy_config(&config).unwrap();
        let state = Mutex::new(None);
        let publication = Publication_service::new();
        let mirror_writes = Provider_switch_service::default();
        let first = save_and_setup_supplier(&db, &state, &publication, &mirror_writes, serde_json::from_value(json!({
            "expectedGeneration":0,"name":"First","baseUrl":"https://example.invalid","apiKey":"fixture_key",
            "endpoints":[{"protocol":"openai-chat","models":[{"modelId":"first-model"}]}]
        })).unwrap()).await.unwrap();
        assert!(first.setup_error.is_none());
        let second = save_and_setup_supplier(&db, &state, &publication, &mirror_writes, serde_json::from_value(json!({
            "expectedGeneration":first.result.generation,"name":"Second","baseUrl":"https://example.invalid","apiKey":"fixture_key",
            "endpoints":[{"protocol":"openai-chat","models":[{"modelId":"second-model"}]}]
        })).unwrap()).await.unwrap();
        let update = supplier_service::update_supplier_models(&db, serde_json::from_value(json!({
            "expectedGeneration":second.result.generation,"supplierId":first.result.supplier_id,"protocol":"openai-chat",
            "models":[{"modelId":"replacement-model"}]
        })).unwrap()).unwrap();
        reconcile_supplier_setup(&db, &state, &publication, &mirror_writes)
            .await
            .unwrap();
        let selected = db
            .get_setting(crate::services::supplier_setup::KEY_UNIFIED_DEFAULT_MODEL)
            .unwrap()
            .unwrap();
        assert_ne!(selected, "first-model");
        let config_text =
            std::fs::read_to_string(home.root.join(".codex-cli/config.toml")).unwrap();
        assert!(config_text.contains(&selected));
        supplier_service::delete_supplier(&db, &first.result.supplier_id, update.generation)
            .unwrap();
        reconcile_supplier_setup(&db, &state, &publication, &mirror_writes)
            .await
            .unwrap();
        assert_eq!(
            db.get_setting(crate::services::supplier_setup::KEY_UNIFIED_DEFAULT_MODEL)
                .unwrap()
                .as_deref(),
            Some("second-model")
        );
        let claude: Value = serde_json::from_slice(
            &std::fs::read(home.root.join(".claude-cli/settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(claude["env"]["ANTHROPIC_MODEL"], "second-model");
        let running = state.lock().await.take().unwrap();
        let mut shutdown = running.request_shutdown();
        shutdown.wait().await.unwrap();
    }

    #[tokio::test]
    async fn one_save_starts_gateway_and_routes_selected_model_without_manual_routes() {
        let _home =
            crate::services::provider::test_support::isolated_home_async("supplier-save").await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let upstream = tokio::spawn(async move {
            axum::serve(listener, Router::new().route("/v1/chat/completions", post(|Json(body): Json<Value>| async move {
                assert_eq!(body["model"], "selected-model");
                Json(json!({"id":"fixture", "object":"chat.completion", "created":1, "model":"selected-model", "choices":[{"index":0,"message":{"role":"assistant","content":"routed"},"finish_reason":"stop"}], "usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}))
            }))).await.unwrap();
        });
        let db = Database::memory().unwrap();
        let mut config = db.get_proxy_config().unwrap();
        config.port = 0;
        config.auth_token = "fixture_gateway_token".into();
        db.save_proxy_config(&config).unwrap();
        let state = Mutex::new(None);
        let input = serde_json::from_value(json!({
            "expectedGeneration":0,"name":"One provider","baseUrl":base_url,"apiKey":"fixture_upstream_key",
            "endpoints":[{"protocol":"openai-chat","models":[{"modelId":"selected-model"}]}]
        })).unwrap();
        let result = save_and_setup_supplier(
            &db,
            &state,
            &Publication_service::new(),
            &Provider_switch_service::default(),
            input,
        )
        .await
        .unwrap();
        assert!(result.setup_error.is_none(), "{:?}", result.setup_error);
        let config = db.get_proxy_config().unwrap();
        assert!(
            config.enabled,
            "automatic setup must survive application restart"
        );
        assert!(db.get_all_routes().unwrap().is_empty());
        let response = reqwest::Client::new()
            .post(format!(
                "http://127.0.0.1:{}/v1/chat/completions",
                config.port
            ))
            .bearer_auth("fixture_gateway_token")
            .json(&json!({"model":"selected-model","messages":[{"role":"user","content":"ping"}]}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["choices"][0]["message"]["content"], "routed");
        let running = state.lock().await.take().unwrap();
        let mut shutdown = running.request_shutdown();
        shutdown.wait().await.unwrap();
        upstream.abort();
        let _ = upstream.await;
    }
}
