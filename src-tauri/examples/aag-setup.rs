//! One-shot setup helper: prime the AAG DB with cc-switch's claude_code
//! providers and arm the local proxy on port 8989.
//!
//! Run with: `cargo run --bin aag-setup`
//!
//! What it does, in order:
//! 1. Opens (creates if missing) `~/.aag/aag.db` and applies the schema
//!    seed (`Database::init`). This brings up the `proxy_config` row at
//!    127.0.0.1:8989 and the mock failover demo routes.
//! 2. Imports every provider from `~/.cc-switch/cc-switch.db` into the
//!    AAG `providers` table (idempotent — already-present rows are
//!    skipped). The cc-switch → AAG app_type mapping lives in
//!    `services::import_cc_switch::map_app_type`.
//! 3. Counts the imported claude_code rows and prints them so the user
//!    can sanity-check the import against the source catalogue.
//! 4. Flips `proxy_config.enabled = true` and persists it. When
//!    `tauri:dev` starts the app, `proxy_start` reads this row, walks
//!    the imported claude_code entries, and auto-derives the failover
//!    queue (one `queue-<id>` proxy provider per row).
//! 5. Explicitly clears `sync_with_cc_switch_enabled` so the auto-sync
//!    path inside `proxy_start` is OFF. `aag-setup` is the explicit
//!    one-shot import path; users who want ongoing sync flip the
//!    toggle in the Settings page.
//!
//! Does NOT start the proxy itself — that's owned by the Tauri app so
//! the UI can show start/stop and route editing on the Proxy page.

use ai_agent_gateway_lib::database::dao::proxy_config::ProxyConfig;
use ai_agent_gateway_lib::database::Database;
use ai_agent_gateway_lib::services::import_cc_switch::{
    cc_switch_db_path, import_from_db,
};

fn main() {
    if let Err(e) = run() {
        eprintln!("aag-setup failed: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== aag-setup ===");
    let db = Database::init()?;
    let db_path = ai_agent_gateway_lib::config::get_db_path();
    println!("AAG DB: {}", db_path.display());

    let cc_path = cc_switch_db_path();
    println!("cc-switch DB: {}", cc_path.display());
    if !cc_path.exists() {
        return Err(format!("cc-switch DB not found at {}", cc_path.display()).into());
    }

    let report = import_from_db(&db, &cc_path)?;
    println!(
        "\nimport: total_in_source = {}, imported = {}, skipped_existing = {}",
        report.total_in_source,
        report.imported.len(),
        report.skipped_existing.len(),
    );

    let mut claude_imported: Vec<&ai_agent_gateway_lib::services::import_cc_switch::ImportedProvider> =
        Vec::new();
    for r in &report.imported {
        if r.app_type == "claude_code" {
            claude_imported.push(r);
        }
    }
    println!("\nimported claude_code providers ({}):", claude_imported.len());
    for r in &claude_imported {
        println!("  + {:<24} {:<36} {}", r.app_type, r.name, r.id);
    }
    let claude_skipped: Vec<_> = report
        .skipped_existing
        .iter()
        .filter(|r| r.app_type == "claude_code")
        .collect();
    if !claude_skipped.is_empty() {
        println!("\nskipped (already in AAG) claude_code providers ({}):", claude_skipped.len());
        for r in &claude_skipped {
            println!("  = {:<24} {:<36} {}", r.app_type, r.name, r.id);
        }
    }

    let current_cfg = db.get_proxy_config()?;
    let new_cfg = ProxyConfig {
        host: current_cfg.host.clone(),
        port: 8989,
        auth_token: current_cfg.auth_token.clone(),
        enabled: true,
    };
    db.save_proxy_config(&new_cfg)?;
    println!(
        "\nproxy_config: {}:{} enabled = {}",
        new_cfg.host, new_cfg.port, new_cfg.enabled
    );

    // Make sure the cc-switch auto-sync is OFF. Without this, every
    // subsequent `proxy_start` re-reads ~/.cc-switch/cc-switch.db and
    // silently INSERT-OR-IGNOREs new claude_code rows — which is fine
    // for the failover queue but surprises users who edited AAG
    // providers locally and don't expect cc-switch changes to keep
    // flowing in. `aag-setup` is the explicit one-shot import path;
    // users who want ongoing sync flip the toggle in the Settings page.
    db.set_setting(
        ai_agent_gateway_lib::database::dao::settings::KEY_SYNC_WITH_CCSWITCH,
        "",
    )?;
    let sync_state = db
        .get_setting(ai_agent_gateway_lib::database::dao::settings::KEY_SYNC_WITH_CCSWITCH)?
        .unwrap_or_default();
    println!(
        "sync_with_cc_switch_enabled = {} (default OFF; enable via Settings page for ongoing sync)",
        if sync_state.is_empty() { "off" } else { &sync_state }
    );

    let total_claude = db
        .get_all_providers("claude_code")
        .map_err(|e| format!("list claude_code: {e}"))?
        .len();
    println!(
        "\nAAG `providers` (claude_code) rows now: {}",
        total_claude
    );

    println!("\nNext: `npm run tauri:dev` — the Tauri app will read this DB,");
    println!("auto-derive the failover queue, and start the proxy on 8989.");
    Ok(())
}