//! Supplier import preview/apply IPC commands.
//!
//! Command registration is owned by the application integration layer;
//! add these handlers from `lib.rs::generate_handler!` alongside the
//! `supplier_*` commands after wiring the module.

use crate::database::Database;
use crate::services::import_cc_switch::{
    apply_import, preview_import, SupplierImportApplyResult, SupplierImportPreview,
};
use crate::services::provider::switching::Provider_switch_service;
use tauri::async_runtime::Mutex;
use tauri::State;

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SupplierImportSetupResult {
    #[serde(flatten)]
    result: SupplierImportApplyResult,
    setup_error: Option<String>,
    discovery_error: Option<String>,
}

/// Redacted, zero-write preview of what importing the cc-switch source
/// would create. `source_path` overrides the default cc-switch location
/// (used by tests and non-default installs).
#[tauri::command]
pub(crate) fn supplier_import_preview(
    db: State<'_, Database>,
    source_path: Option<String>,
) -> Result<SupplierImportPreview, String> {
    let path = source_path
        .map(std::path::PathBuf::from)
        .unwrap_or_else(crate::services::import_cc_switch::cc_switch_db_path);
    preview_import(&db, &path).map_err(supplier_ipc_error)
}

/// Apply a previewed plan in one transaction. The plan id binds the
/// source fingerprint and the target configuration generation; any drift
/// is refused without writing.
#[tauri::command]
pub(crate) async fn supplier_import_apply(
    db: State<'_, Database>,
    state: State<'_, Mutex<Option<crate::commands::proxy::RunningProxy>>>,
    publication: State<'_, crate::services::tailnet_publication::Publication_service>,
    mirror_writes: State<'_, Provider_switch_service>,
    plan_id: String,
    source_path: Option<String>,
) -> Result<SupplierImportSetupResult, String> {
    let path = source_path
        .map(std::path::PathBuf::from)
        .unwrap_or_else(crate::services::import_cc_switch::cc_switch_db_path);
    let preview = preview_import(&db, &path).map_err(supplier_ipc_error)?;
    let changed_ids: Vec<String> = preview
        .plans
        .iter()
        .filter(|entry| entry.change != "unchanged")
        .map(|entry| entry.supplier_id.clone())
        .collect();
    crate::services::import_cc_switch::screen_import(&db, &path, &plan_id, &[]).await?;
    let mut result = apply_import(&db, &path, &plan_id).map_err(supplier_ipc_error)?;
    let discovery_error = if result.already_applied {
        None
    } else {
        crate::services::suppliers::revalidate_suppliers(&db, &changed_ids)
            .await
            .err()
    };
    result.applied_generation = db
        .supplier_config_generation()
        .map_err(supplier_ipc_error)?;
    // Import commits independently. Surface setup failures without presenting
    // the already-persisted import as rolled back or inviting duplicate import.
    let setup_error =
        super::suppliers::reconcile_supplier_setup(&db, &state, &publication, &mirror_writes)
            .await
            .err();
    Ok(SupplierImportSetupResult {
        result,
        setup_error,
        discovery_error,
    })
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SupplierMaintenanceSetupResult {
    #[serde(flatten)]
    result: crate::database::dao::suppliers::SupplierMaintenanceResult,
    setup_error: Option<String>,
}

#[derive(Default)]
pub(crate) struct Supplier_maintenance_service {
    operation: tokio::sync::Mutex<Option<SupplierMaintenanceSetupResult>>,
}

impl Supplier_maintenance_service {
    pub(crate) fn with_initial(
        result: crate::database::dao::suppliers::SupplierMaintenanceResult,
    ) -> Self {
        let setup_error = if result.backup_created {
            Some("supplier_setup_pending".into())
        } else {
            None
        };
        Self::with_initial_setup(result, setup_error)
    }
    pub(crate) fn with_initial_setup(
        result: crate::database::dao::suppliers::SupplierMaintenanceResult,
        setup_error: Option<String>,
    ) -> Self {
        Self {
            operation: tokio::sync::Mutex::new(Some(SupplierMaintenanceSetupResult {
                result,
                setup_error,
            })),
        }
    }
}

#[tauri::command]
pub(crate) async fn supplier_import_maintain(
    db: State<'_, Database>,
    service: State<'_, Supplier_maintenance_service>,
    state: State<'_, Mutex<Option<crate::commands::proxy::RunningProxy>>>,
    publication: State<'_, crate::services::tailnet_publication::Publication_service>,
    mirror_writes: State<'_, Provider_switch_service>,
) -> Result<SupplierMaintenanceSetupResult, String> {
    // Admission spans both the transaction and asynchronous client/runtime reconciliation.
    let mut last = service.operation.lock().await;
    let result = crate::services::import_cc_switch::maintain_imported_suppliers(&db)
        .map_err(supplier_ipc_error)?;
    let pending = db
        .get_setting(crate::database::dao::suppliers::KEY_SUPPLIER_IMPORT_SETUP_PENDING)
        .map_err(supplier_ipc_error)?
        .as_deref()
        == Some("1");
    let current_conflicts = (
        result.merge_conflict_count,
        result.merge_conflict_codes.clone(),
    );
    let mut receipt = if !result.backup_created {
        last.as_ref()
            .filter(|previous| previous.result.generation == result.generation)
            .cloned()
            .unwrap_or(SupplierMaintenanceSetupResult {
                result,
                setup_error: None,
            })
    } else {
        SupplierMaintenanceSetupResult {
            result,
            setup_error: None,
        }
    };
    receipt.result.merge_conflict_count = current_conflicts.0;
    receipt.result.merge_conflict_codes = current_conflicts.1;
    if pending || receipt.setup_error.is_some() {
        receipt.setup_error =
            super::suppliers::reconcile_supplier_setup(&db, &state, &publication, &mirror_writes)
                .await
                .err();
        if receipt.setup_error.is_none() {
            if db
                .set_setting(
                    crate::database::dao::suppliers::KEY_SUPPLIER_IMPORT_SETUP_PENDING,
                    "",
                )
                .is_err()
            {
                receipt.setup_error = Some("supplier_setup_state_unavailable".into());
            }
        }
    }
    *last = Some(receipt.clone());
    Ok(receipt)
}

/// Stable IPC error mapping: codes like `supplier_import_*` pass through
/// verbatim; anything else becomes a generic message so internals (paths,
/// URLs, credentials) never cross the IPC boundary.
fn supplier_ipc_error(error: crate::error::AppError) -> String {
    match &error {
        crate::error::AppError::Message(detail) | crate::error::AppError::InvalidInput(detail)
            if detail.starts_with("supplier_")
                && detail.len() <= 96
                && detail.bytes().all(|byte| {
                    byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'
                }) =>
        {
            detail.clone()
        }
        _ => String::from("supplier_import_failed"),
    }
}
