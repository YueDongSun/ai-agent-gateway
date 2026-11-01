//! Operator adapter over AAG's import service. No SQL or client-file writes.
use ai_agent_gateway_lib::{
    database::Database,
    services::import_cc_switch::{
        apply_import, apply_reset_import, preview_import, preview_reset_import,
    },
};
use std::{
    collections::{BTreeMap, HashSet},
    path::PathBuf,
};

#[tokio::main]
async fn main() {
    match run().await {
        Ok(report) => println!("{report}"),
        Err(code) => {
            println!("{}", serde_json::json!({"ok":false,"error":code}));
            std::process::exit(1);
        }
    }
}

async fn run() -> Result<serde_json::Value, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() == 1 && args[0] == "--help" {
        return Ok(
            serde_json::json!({"usage":"supplier_import preview DATABASE SOURCE | apply DATABASE SOURCE PLAN_ID BACKUP | reset-preview DATABASE SOURCE | reset-apply DATABASE SOURCE PLAN_ID BACKUP", "scope":"Static CC Switch import. Explicit reset-apply atomically replaces all AAG provider configuration under a full backup; unrelated data and CC Switch remain intact. Runtime reload is separate."}),
        );
    }
    if !matches!(
        args.first().map(String::as_str),
        Some("preview" | "apply" | "reset-preview" | "reset-apply")
    ) || (args[0].ends_with("preview") && args.len() != 3)
        || (args[0].ends_with("apply") && args.len() != 5)
    {
        return Err("supplier_operator_arguments_invalid".into());
    }
    let apply = args[0].ends_with("apply");
    let reset = args[0].starts_with("reset-");
    let path = PathBuf::from(&args[1]);
    let source = PathBuf::from(&args[2]);
    let db = Database::open_existing_for_operator(&path, !apply)
        .map_err(|error| error.external_message())?;
    let preview = if reset {
        preview_reset_import(&db, &source)
    } else {
        preview_import(&db, &source)
    }
    .map_err(|error| error.external_message())?;
    let current = !reset
        && ai_agent_gateway_lib::services::import_cc_switch::source_is_materialized(&db, &source)
            .map_err(|error| error.external_message())?;
    let mut changes = BTreeMap::<String, usize>::new();
    let mut problems = BTreeMap::<String, usize>::new();
    for entry in &preview.plans {
        *changes.entry(entry.change.clone()).or_default() += 1;
    }
    for entry in &preview.problems {
        *problems.entry(entry.code.clone()).or_default() += 1;
    }
    let mut report = serde_json::json!({"ok":true,"planId":preview.plan_id,"sourceRows":preview.total_rows,
        "supplierCount":preview.plans.iter().map(|entry| &entry.supplier_id).collect::<HashSet<_>>().len(),
        "endpointPlans":preview.plans.len(),"changes":changes,"excludedAccounts":preview.excluded_oauth_count,
        "unsupported":preview.pending_unsupported.len(),"problems":problems,"contentCurrent":current,"resetMode":reset,"applied":false});
    if !apply {
        return Ok(report);
    }
    if args[3] != preview.plan_id {
        return Err("supplier_operator_stale_plan".into());
    }
    if reset {
        let result = apply_reset_import(&db, &source, &args[3], &PathBuf::from(&args[4]))
            .map_err(|error| error.external_message())?;
        report["applied"] = serde_json::json!(true);
        report["result"] =
            serde_json::to_value(result).map_err(|_| "supplier_operator_result_failed")?;
        report["backupCreated"] = serde_json::json!(true);
        report["runtimeReloadRequired"] = serde_json::json!(true);
        return Ok(report);
    }
    let inventory = ai_agent_gateway_lib::services::suppliers::list_suppliers(&db)
        .map_err(|error| error.external_message())?;
    let protected: HashSet<_> = inventory
        .suppliers
        .iter()
        .filter(|supplier| supplier.source != "cc_switch")
        .map(|supplier| supplier.id.clone())
        .collect();
    if preview
        .plans
        .iter()
        .any(|entry| protected.contains(&entry.supplier_id))
    {
        return Err("supplier_operator_protected_target".into());
    }
    let before_routes = serde_json::to_value(
        db.get_all_routes()
            .map_err(|error| error.external_message())?,
    )
    .map_err(|_| "supplier_operator_snapshot_failed")?;
    // Preview labels are presentation hints; exact apply idempotency also
    // checks private credential/variant content. Always snapshot before apply.
    db.create_operator_snapshot(&PathBuf::from(&args[4]))
        .map_err(|error| error.external_message())?;
    ai_agent_gateway_lib::services::import_cc_switch::screen_import(&db, &source, &args[3], &[]).await?;
    let result = apply_import(&db, &source, &args[3]).map_err(|error| error.external_message())?;
    let changed =
        !result.already_applied && (result.created_count > 0 || result.refreshed_count > 0);
    report["applied"] = serde_json::json!(true);
    report["result"] =
        serde_json::to_value(result).map_err(|_| "supplier_operator_result_failed")?;
    report["backupCreated"] = serde_json::json!(true);
    match db
        .get_all_routes()
        .ok()
        .and_then(|routes| serde_json::to_value(routes).ok())
    {
        Some(after_routes) => {
            report["routesPreserved"] = serde_json::json!(before_routes == after_routes);
            if before_routes != after_routes {
                report["verificationError"] = serde_json::json!("supplier_operator_route_drift");
            }
        }
        None => {
            report["routesPreserved"] = serde_json::Value::Null;
            report["verificationError"] =
                serde_json::json!("supplier_operator_post_apply_verification_failed");
        }
    }
    report["runtimeReloadRequired"] = serde_json::json!(changed);
    Ok(report)
}
