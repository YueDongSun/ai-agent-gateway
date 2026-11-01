//! Provider CRUD commands.
//!
//! Live configuration switching is owned by the provider service so that
//! filesystem failures cannot mark an unapplied provider as current.

use crate::database::Database;
use crate::provider::Provider;

// ----- Provider commands (Phase 1) -----

#[tauri::command]
pub(crate) fn provider_list(
    db: tauri::State<'_, Database>,
    app_type: String,
) -> Result<Vec<Provider>, String> {
    db.get_all_providers(&app_type)
        .map(|m| m.into_iter().map(|(_, v)| v).collect())
        .map_err(String::from)
}
#[tauri::command]
pub(crate) fn provider_current(
    db: tauri::State<'_, Database>,
    app_type: String,
) -> Result<Option<String>, String> {
    db.get_current_provider(&app_type).map_err(String::from)
}
#[tauri::command]
pub(crate) fn provider_get(
    db: tauri::State<'_, Database>,
    id: String,
    app_type: String,
) -> Result<Option<Provider>, String> {
    db.get_provider_by_id(&id, &app_type).map_err(String::from)
}
#[tauri::command]
pub(crate) async fn provider_save(
    db: tauri::State<'_, Database>,
    proxy: tauri::State<'_, tokio::sync::Mutex<Option<crate::commands::proxy::RunningProxy>>>,
    app_type: String,
    provider: Provider,
) -> Result<(), String> {
    // upsert (not save_provider): UI edits to an existing row must persist;
    // save_provider stays insert-only for importer idempotency.
    db.upsert_provider(&app_type, &provider)
        .map_err(String::from)?;
    // A saved row is a routing input (queue membership, model list): apply
    // it to the live snapshot instead of waiting for the next proxy_start.
    crate::commands::proxy::runtime_reload(proxy.inner()).await
}

#[tauri::command]
pub(crate) async fn provider_delete(
    db: tauri::State<'_, Database>,
    proxy: tauri::State<'_, tokio::sync::Mutex<Option<crate::commands::proxy::RunningProxy>>>,
    id: String,
    app_type: String,
) -> Result<(), String> {
    db.delete_provider(&id, &app_type)
        .map_err(String::from)?;
    // A deleted row leaves the failover queue and catalog inputs: apply the
    // removal to the live snapshot instead of waiting for the next
    // proxy_start.
    crate::commands::proxy::runtime_reload(proxy.inner()).await
}
#[tauri::command]
/// Apply the app's live configuration, then mark the provider as current.
/// Claude and Codex use their isolated AAG mirror homes; other supported
/// clients resolve their platform-specific configuration paths in the service.
pub(crate) async fn provider_switch(
    db: tauri::State<'_, Database>,
    service: tauri::State<'_, crate::services::provider::switching::Provider_switch_service>,
    proxy: tauri::State<'_, tokio::sync::Mutex<Option<crate::commands::proxy::RunningProxy>>>,
    id: String,
    app_type: String,
) -> Result<crate::services::provider::LiveWriteResult, String> {
    // Live-file writes must stay serialized off the IPC thread.
    let owned_db = db.inner().clone();
    let owned_service = service.inner().clone();
    let switch_id = id.clone();
    let switch_app_type = app_type.clone();
    let live = tauri::async_runtime::spawn_blocking(move || {
        owned_service.switch_provider(&owned_db, &switch_id, &switch_app_type)
    })
    .await
    .map_err(|_| "provider:switch_worker_failed".to_string())?
    .map_err(String::from)?;

    // The is_current marker is a routing input (codex model-catalog
    // auto-routes key off it), so hot-apply the switch to new requests
    // instead of leaving the running proxy on stale routes.
    crate::commands::proxy::runtime_reload(proxy.inner()).await?;

    Ok(live)
}

/// Per-app outcome of [`provider_switch_all`].
#[allow(non_camel_case_types)]
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct Provider_switch_all_entry {
    pub app_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    /// `applied`, `no_current`, or `error`.
    pub outcome: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// The clients [`provider_switch_all`] applies, in the UI's app-page order.
/// Detect-only clients (antigravity, openclaw, hermes, grok_build) are
/// excluded because switching rejects them.
const SWITCH_ALL_APP_TYPES: &[&str] = &[
    "claude_code",
    "claude_desktop",
    "codex",
    "copilot",
    "pi",
    "gemini",
    "opencode",
    "kilo_cli",
    "cline_cli",
    "claude_vscode",
    "kilo_vscode",
];

/// Batch core of [`provider_switch_all`]: one outcome entry per app in
/// [`SWITCH_ALL_APP_TYPES`], serialized app by app off the IPC thread. A
/// per-app failure lands in its entry and never aborts the batch.
async fn switch_all_apps(
    db: Database,
    service: crate::services::provider::switching::Provider_switch_service,
) -> Result<Vec<Provider_switch_all_entry>, String> {
    let mut entries = Vec::new();
    for app_type in SWITCH_ALL_APP_TYPES {
        let owned_db = db.clone();
        let owned_service = service.clone();
        let owned_app_type = (*app_type).to_string();
        // Each app's live-file writes stay serialized off the IPC thread,
        // one app at a time: a later app's write must not interleave with
        // an earlier one's backup/replace sequence.
        let entry = tauri::async_runtime::spawn_blocking(move || {
            match owned_db.get_current_provider(&owned_app_type) {
                Ok(Some(id)) => match owned_service
                    .switch_provider(&owned_db, &id, &owned_app_type)
                {
                    Ok(live) => Provider_switch_all_entry {
                        app_type: owned_app_type,
                        provider_id: Some(id),
                        outcome: "applied".into(),
                        detail: Some(format!("{} written", live.written.len())),
                    },
                    Err(error) => Provider_switch_all_entry {
                        app_type: owned_app_type,
                        provider_id: Some(id),
                        outcome: "error".into(),
                        detail: Some(String::from(error)),
                    },
                },
                Ok(None) => Provider_switch_all_entry {
                    app_type: owned_app_type,
                    provider_id: None,
                    outcome: "no_current".into(),
                    detail: None,
                },
                Err(error) => Provider_switch_all_entry {
                    app_type: owned_app_type,
                    provider_id: None,
                    outcome: "error".into(),
                    detail: Some(String::from(error)),
                },
            }
        })
        .await
        .map_err(|_| "provider:switch_all_worker_failed".to_string())?;
        entries.push(entry);
    }
    Ok(entries)
}

#[tauri::command]
/// One-click fill: apply every supported client's current provider to its
/// live configuration in one action. Apps without a current row report
/// `no_current`; an app whose write fails reports `error` with the reason
/// without aborting the batch.
pub(crate) async fn provider_switch_all(
    db: tauri::State<'_, Database>,
    service: tauri::State<'_, crate::services::provider::switching::Provider_switch_service>,
    proxy: tauri::State<'_, tokio::sync::Mutex<Option<crate::commands::proxy::RunningProxy>>>,
) -> Result<Vec<Provider_switch_all_entry>, String> {
    let entries = switch_all_apps(db.inner().clone(), service.inner().clone()).await?;
    // The batch may have moved several is_current markers: hot-apply the
    // routing inputs once for the whole batch.
    crate::commands::proxy::runtime_reload(proxy.inner()).await?;
    Ok(entries)
}


#[tauri::command]
pub(crate) async fn provider_reorder(
    db: tauri::State<'_, Database>,
    proxy: tauri::State<'_, tokio::sync::Mutex<Option<crate::commands::proxy::RunningProxy>>>,
    app_type: String,
    ordered_ids: Vec<String>,
) -> Result<(), String> {
    db.reorder_providers(&app_type, &ordered_ids)
        .map_err(String::from)?;
    // Queue order is a routing input: apply it to the live snapshot instead
    // of waiting for the next proxy_start.
    crate::commands::proxy::runtime_reload(proxy.inner()).await
}

/// Toggle a provider's membership in its app's failover chain (the per-card
/// "join the chain" switch). Rows with the bit off are excluded when the
/// proxy builds the per-app queue; a running proxy hot-applies the change.
#[tauri::command]
pub(crate) async fn provider_set_failover_queue(
    db: tauri::State<'_, Database>,
    proxy: tauri::State<'_, tokio::sync::Mutex<Option<crate::commands::proxy::RunningProxy>>>,
    id: String,
    app_type: String,
    in_queue: bool,
) -> Result<(), String> {
    db.set_in_failover_queue(&id, &app_type, in_queue)
        .map_err(String::from)?;
    crate::commands::proxy::runtime_reload(proxy.inner()).await
}

#[cfg(test)]
mod switch_all_tests {
    use super::*;
    use crate::services::provider::test_support::isolated_home_async;
    use crate::services::provider::switching::Provider_switch_service;
    use serde_json::json;

    #[tokio::test]
    async fn switch_all_reports_no_current_for_every_supported_app() {
        let entries = switch_all_apps(Database::memory().unwrap(), Provider_switch_service::default())
            .await
            .unwrap();
        assert_eq!(entries.len(), SWITCH_ALL_APP_TYPES.len());
        assert!(entries.iter().all(|entry| {
            entry.outcome == "no_current" && entry.provider_id.is_none() && entry.detail.is_none()
        }));
        let apps: Vec<&str> = entries.iter().map(|e| e.app_type.as_str()).collect();
        for expected in ["claude_code", "claude_desktop", "codex", "gemini", "kilo_vscode"] {
            assert!(apps.contains(&expected), "missing {expected}");
        }
        for excluded in ["antigravity", "openclaw", "hermes", "grok_build"] {
            assert!(!apps.contains(&excluded), "detect-only {excluded} must not be switched");
        }
    }

    #[tokio::test]
    async fn switch_all_applies_selections_and_continues_past_a_failing_app() {
        let _home = isolated_home_async("switch-all-batch").await;
        let db = Database::memory().unwrap();
        db.save_provider(
            "claude_code",
            &Provider::new("a".into(), "A".into(), json!({})),
        )
        .unwrap();
        db.set_current_provider("a", "claude_code").unwrap();
        db.save_provider(
            "codex",
            &Provider::new("b".into(), "B".into(), json!({"model":"m"})),
        )
        .unwrap();
        db.set_current_provider("b", "codex").unwrap();
        // Managed session authority rejects the codex live write.
        db.set_setting("codex_session_authority_enabled", "1")
            .unwrap();

        let entries = switch_all_apps(db.clone(), Provider_switch_service::default())
            .await
            .unwrap();

        assert_eq!(entries.len(), SWITCH_ALL_APP_TYPES.len());
        let claude = entries
            .iter()
            .find(|e| e.app_type == "claude_code")
            .expect("claude_code entry");
        assert_eq!(claude.outcome, "applied");
        assert_eq!(claude.provider_id.as_deref(), Some("a"));
        let codex = entries
            .iter()
            .find(|e| e.app_type == "codex")
            .expect("codex entry");
        assert_eq!(codex.outcome, "error");
        assert_eq!(codex.provider_id.as_deref(), Some("b"));
        assert_eq!(
            codex.detail.as_deref(),
            Some("session_client_authority_unavailable")
        );
        // The failed app did not abort the batch or move the selection.
        assert_eq!(
            db.get_current_provider("codex").unwrap().as_deref(),
            Some("b")
        );
    }
}
