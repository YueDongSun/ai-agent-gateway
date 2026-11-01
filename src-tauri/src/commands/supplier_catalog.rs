//! Public model-catalog IPC commands.
//!
//! Thin wrappers over `services::model_catalog`; validation, bounded fetch,
//! TTL/staleness and cache persistence all live in the service. Errors cross
//! IPC as stable `supplier_catalog_*` codes only — never URLs, bodies, or
//! raw fetch errors.

use crate::database::Database;
use crate::services::model_catalog::{
    catalog_status, model_metadata, CatalogRefreshInput, CatalogSourceView, CatalogStatusView,
    Catalog_refresh_service, ModelCatalogEntry,
};
use crate::services::suppliers::ipc_error;
use tauri::async_runtime::Mutex;
use tauri::State;

/// Cache state of both fixed public sources (models.dev metadata, EvalPlus
/// scores) in fixed order; missing or expired caches report `stale: true`.
#[tauri::command]
pub(crate) fn supplier_catalog_status(
    db: State<'_, Database>,
) -> Result<CatalogStatusView, String> {
    catalog_status(&db).map_err(ipc_error)
}

/// Refresh one source with a bounded credential-free fetch. Success replaces
/// the cache and clears the error; failure keeps the previous document,
/// records a stable error code, and returns the stale view. A successful
/// refresh moves the routing fingerprint, so hot-apply to the running
/// gateway the same way a supplier save does.
#[tauri::command]
pub(crate) async fn supplier_catalog_refresh(
    db: State<'_, Database>,
    refresh: State<'_, Catalog_refresh_service>,
    state: State<'_, Mutex<Option<crate::commands::proxy::RunningProxy>>>,
    input: CatalogRefreshInput,
    only_if_stale: Option<bool>,
) -> Result<CatalogSourceView, String> {
    let view = if only_if_stale.unwrap_or(false) {
        refresh.refresh_if_stale(&db, &input).await
    } else {
        refresh.refresh(&db, &input).await
    }
    .map_err(ipc_error)?;
    crate::commands::proxy::runtime_reload(&state).await?;
    Ok(view)
}

/// Model metadata for one selector (`precise:<id>`, `alias:<name>`, or a
/// bare name): tier assessment with source/condition/confidence, factual
/// capabilities and costs, and the offered/catalog-only split.
#[tauri::command]
pub(crate) fn model_catalog_model(
    db: State<'_, Database>,
    selector: String,
) -> Result<ModelCatalogEntry, String> {
    model_metadata(&db, &selector).map_err(ipc_error)
}

/// Cached public coordinates plus a pure strategy preview; never probes a model.
#[tauri::command]
pub(crate) async fn supplier_arena_snapshot(
    db: State<'_, Database>,
    state: State<'_, Mutex<Option<crate::commands::proxy::RunningProxy>>>,
    selector: Option<String>,
    needs_tools: Option<bool>,
    needs_images: Option<bool>,
    surface: Option<String>,
) -> Result<crate::services::model_catalog::arena_visualization::Arena_snapshot, String> {
    let surface = surface.as_deref().unwrap_or("claude");
    if !["claude", "codex", "chat", "gemini"].contains(&surface) {
        return Err("arena_surface_invalid".into());
    }
    let runtime = state
        .lock()
        .await
        .as_ref()
        .and_then(|running| running.preview_snapshot());
    let mut view = crate::services::model_catalog::arena_visualization::snapshot(
        &db,
        selector,
        needs_tools.unwrap_or(false),
        needs_images.unwrap_or(false),
    )
    .map_err(ipc_error)?;
    if let Some((router, pin)) = runtime {
        crate::services::model_catalog::arena_visualization::apply_runtime_preview(
            &mut view,
            &router,
            pin.as_deref(),
            surface,
            crate::proxy::planner::Request_features {
                needs_tools: needs_tools.unwrap_or(false),
                needs_images: needs_images.unwrap_or(false),
                ..Default::default()
            },
        );
    }
    Ok(view)
}
