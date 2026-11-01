//! Explicit reset and rebuild from static CC Switch supply. No native account,
//! client-file, listener, or process operation occurs in this transaction.
use super::{preview, supplier_import, SupplierImportPreview};
use crate::{database::Database, error::AppError};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    path::Path,
};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SupplierResetImportResult {
    pub plan_id: String,
    pub generation: u64,
    pub removed_rows: BTreeMap<String, u64>,
    pub created_suppliers: usize,
    pub created_endpoints: usize,
    pub excluded_oauth_count: usize,
    pub problem_count: usize,
    pub backup_created: bool,
    pub runtime_reload_required: bool,
}

fn reset_plan_id(derived: &preview::DerivedPlan, revision: &str) -> String {
    let mut digest = Sha256::new();
    for part in [
        "aag-static-reset-v1",
        derived.source_fingerprint.as_str(),
        revision,
        &preview::derived_digest12(&derived.entries, &derived.pending),
    ] {
        digest.update((part.len() as u64).to_be_bytes());
        digest.update(part.as_bytes());
    }
    format!("reset_v1:{}", hex::encode(digest.finalize()))
}

fn prepare(
    db: &Database,
    source: &Path,
) -> Result<(preview::DerivedPlan, String, u64, String), AppError> {
    let revision = db.provider_reset_revision()?;
    let generation = db.supplier_config_generation()?;
    let rows = preview::read_source_rows(source)?;
    // Empty inventory means no old supplier identity, manual override or
    // retained alternate can silently survive an ALL-provider rebuild.
    // Actual listener settings still reject source endpoints pointing at AAG.
    let derived = preview::derive_plan(&rows, &[], db)?;
    if derived.entries.is_empty() || !derived.pending.is_empty() {
        return Err(AppError::InvalidInput(
            "supplier_reset_import_no_valid_static_supply".into(),
        ));
    }
    if db.provider_reset_revision()? != revision {
        return Err(AppError::Message("provider_reset_concurrent_change".into()));
    }
    let plan_id = reset_plan_id(&derived, &revision);
    Ok((derived, revision, generation, plan_id))
}

pub fn preview_reset_import<P: AsRef<Path>>(
    db: &Database,
    source: P,
) -> Result<SupplierImportPreview, AppError> {
    let (derived, _, generation, plan_id) = prepare(db, source.as_ref())?;
    // to_preview defaults to create; no ordinary-import preservation labels
    // apply to this explicitly destructive operation.
    Ok(preview::to_preview(
        derived,
        plan_id,
        generation,
        chrono::Utc::now().timestamp(),
    ))
}

pub fn apply_reset_import<P: AsRef<Path>>(
    db: &Database,
    source: P,
    plan_id: &str,
    backup_path: &Path,
) -> Result<SupplierResetImportResult, AppError> {
    let source = source.as_ref();
    let (mut derived, revision, _, current_plan) = prepare(db, source)?;
    if !plan_id.starts_with("reset_v1:") || plan_id != current_plan {
        return Err(AppError::Message("supplier_reset_import_stale_plan".into()));
    }
    super::screening::filter_admitted(db, &mut derived, &[])?;
    if derived.entries.is_empty() {
        return Err(AppError::InvalidInput("supplier_reset_import_no_admitted_supply".into()));
    }
    let (reset, ()) =
        db.reset_provider_configuration_with(backup_path, Some(&revision), |transaction| {
            // Recheck source after the potentially slow backup. Refusal or any
            // insertion error rolls back the provider deletion and settings reset.
            let rows = preview::read_source_rows(source)?;
            if preview::source_fingerprint(&rows) != derived.source_fingerprint {
                return Err(AppError::Message("supplier_import_source_changed".into()));
            }
            supplier_import::apply_entries_on_transaction(
                transaction,
                &derived,
                &derived.entries,
                &[],
            )?;
            // Native startup owns the subsequent managed-client rebuild and
            // backed-up file writes, then clears this durable pending marker.
            transaction.execute("INSERT INTO app_meta(key,value) VALUES('supplier_import_setup_pending','1') ON CONFLICT(key) DO UPDATE SET value='1'",[])?;
            Ok(())
        })?;
    Ok(SupplierResetImportResult {
        plan_id: current_plan,
        generation: reset.generation,
        removed_rows: reset.removed_rows,
        created_suppliers: derived
            .entries
            .iter()
            .map(|entry| entry.supplier_id.as_str())
            .collect::<HashSet<_>>()
            .len(),
        created_endpoints: derived.entries.len(),
        excluded_oauth_count: derived.excluded_oauth_count,
        problem_count: derived.problems.len(),
        backup_created: true,
        runtime_reload_required: true,
    })
}
