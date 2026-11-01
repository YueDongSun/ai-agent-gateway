use crate::{
    database::Database,
    services::provider_admission::review::{self, Screening_overview},
};
use serde::Serialize;
use std::future::Future;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Screening_recheck_receipt {
    #[serde(flatten)]
    pub outcome: review::Recheck_outcome,
    pub runtime_reconciled: Option<bool>,
    pub runtime_error: Option<String>,
    pub overview: Option<Screening_overview>,
    pub overview_error: Option<String>,
}

#[tauri::command]
pub(crate) fn provider_screening_overview(
    db: tauri::State<'_, Database>,
) -> Result<Screening_overview, String> {
    review::overview(&db)
}

#[tauri::command]
pub(crate) async fn provider_screening_recheck(
    db: tauri::State<'_, Database>,
    proxy: tauri::State<'_, tokio::sync::Mutex<Option<super::proxy::RunningProxy>>>,
    coordinator: tauri::State<'_, review::Review_coordinator>,
    provider_id: String,
) -> Result<Screening_recheck_receipt, String> {
    Ok(run_recheck(&db, &coordinator, &provider_id, super::proxy::runtime_reload_after_recheck(proxy.inner())).await)
}

pub(crate) async fn run_recheck<F>(
    db: &Database,
    coordinator: &review::Review_coordinator,
    provider_id: &str,
    reconcile: F,
) -> Screening_recheck_receipt
where F: Future<Output = Result<(), String>> {
    let _permit = match coordinator.try_begin(provider_id) {
        Ok(permit) => permit,
        Err(error) => return complete_recheck(db, review::rejected_recheck(provider_id, &error), reconcile).await,
    };
    let outcome = review::recheck_outcome(db, provider_id).await;
    complete_recheck(db, outcome, reconcile).await
}

/// Reconcile a started check even after a later screening failure, then read
/// persisted truth independently of runtime apply success.
pub(crate) async fn complete_recheck<F>(
    db: &Database,
    outcome: review::Recheck_outcome,
    reconcile: F,
) -> Screening_recheck_receipt
where F: Future<Output = Result<(), String>> {
    let runtime_reconciled = if outcome.check_started { Some(reconcile.await.is_ok()) } else { None };
    let runtime_error = (runtime_reconciled == Some(false))
        .then(|| "provider_screening_runtime_reconcile_failed".into());
    let (overview, overview_error) = match review::overview(db) {
        Ok(overview) => (Some(overview), None),
        Err(error) => (None, Some(review::safe_recheck_error(&error))),
    };
    Screening_recheck_receipt { outcome, runtime_reconciled, runtime_error, overview, overview_error }
}
