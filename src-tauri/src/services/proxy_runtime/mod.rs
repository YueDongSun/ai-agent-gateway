//! Proxy runtime assembly: configuration snapshot building, failover-queue
//! construction, and the bind/fallback strategy for `proxy_start`.
//!
//! Extracted from `commands/proxy.rs` so the IPC layer stays pure
//! orchestration (build order, `RunningProxy` state assembly, user-facing
//! messages) while domain logic lives here, unit-testable without Tauri.

pub(crate) mod bind;
pub(crate) mod lifecycle;
pub(crate) mod live;
pub(crate) mod planner;
pub(crate) mod queues;
pub(crate) mod snapshot;
pub(crate) mod transports;

use crate::database::Database;

/// Explicit pre-start sync hook. When the user enabled
/// `sync_with_cc_switch_enabled`, pull every new claude_code provider out of
/// cc-switch's DB before queues/snapshots are built. The sync is additive —
/// locked rows are skipped, existing rows are untouched. Failures degrade to
/// warnings; the proxy still starts.
///
/// Returns whether the sync may have inserted new rows into the pool (or
/// aborted after partially doing so), i.e. whether a pool snapshot taken
/// before this call is stale. Steady-state auto-syncs (and the disabled
/// path) import nothing, so callers can keep using their snapshot without
/// re-reading the table.
///
/// OFF by default: the key must be the literal string "1" (not absent, not
/// "0", not "true"). `aag-setup` clears any pre-existing "1" after its
/// explicit one-shot import so users don't get silent background imports
/// unless they re-enable the toggle in Settings.
///
/// This used to hide inside `build_failover_queues` (a constructor-named
/// helper that silently imported providers); it is an orchestration step and
/// is called explicitly by `start_proxy_impl`.
pub(crate) fn maybe_auto_sync_cc_switch(db: &Database) -> Result<bool, String> {
    if crate::services::provider_admission::enforcement_enabled(db) {
        // New supply requires the explicit status/probe admission operation.
        return Ok(false);
    }
    let sync_on = db
        .get_setting(crate::database::dao::settings::KEY_SYNC_WITH_CCSWITCH)
        .map_err(String::from)?
        .as_deref()
        == Some("1");
    if sync_on {
        let cc_path = crate::services::import_cc_switch::cc_switch_db_path();
        return match crate::services::import_cc_switch::sync_claude_code_from_cc_switch(
            db, &cc_path,
        ) {
            Ok(r) if !r.imported.is_empty() => {
                log::info!(
                    "proxy: cc-switch auto-sync imported {} new provider(s) (skipped {})",
                    r.imported.len(),
                    r.skipped_existing.len()
                );
                Ok(true)
            }
            // A mid-import failure can leave rows inserted before the error,
            // so the caller's pool snapshot must be treated as stale.
            Ok(_) => Ok(false),
            Err(_) => {
                log::warn!("proxy: cc-switch auto-sync failed code=import_error");
                Ok(true)
            }
        };
    }
    Ok(false)
}
