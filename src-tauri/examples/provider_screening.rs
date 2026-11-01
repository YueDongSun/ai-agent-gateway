//! Explicit operator for real, status-first provider screening and filtered import.
//! No SQL, script evaluation, client-file writes, or process lifecycle operations.
use ai_agent_gateway_lib::{
    config,
    database::Database,
    services::{
        import_cc_switch::{self, SupplierImportPreview},
        provider_admission::review,
        provider_screening::ScreeningConfig,
    },
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

const USAGE: &str = "provider_screening preview [--database PATH] [--source PATH] [--subscription-free-source REF]... | apply --plan PLAN_ID --backup NEW_ABSOLUTE_PATH [--database PATH] [--source PATH] [--subscription-free-source REF]... | recheck --provider ID [--provider ID]... --backup NEW_ABSOLUTE_PATH [--database PATH]";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Preview,
    Apply,
    Recheck,
}
struct Options {
    mode: Mode,
    database: PathBuf,
    source: PathBuf,
    plan_id: Option<String>,
    backup: Option<PathBuf>,
    subscription_free_sources: Vec<String>,
    provider_ids: Vec<String>,
}

fn arguments(args: &[String]) -> Result<Options, &'static str> {
    let mode = match args.first().map(String::as_str) {
        Some("preview") => Mode::Preview,
        Some("apply") => Mode::Apply,
        Some("recheck") => Mode::Recheck,
        _ => return Err("provider_screening_arguments_invalid"),
    };
    let mut options = Options {
        mode,
        database: config::get_db_path(),
        source: import_cc_switch::cc_switch_db_path(),
        plan_id: None,
        backup: None,
        subscription_free_sources: Vec::new(),
        provider_ids: Vec::new(),
    };
    let mut seen = BTreeSet::new();
    let mut index = 1;
    while index < args.len() {
        let flag = args[index].as_str();
        if mode == Mode::Recheck && !matches!(flag, "--database" | "--backup" | "--provider") {
            return Err("provider_screening_arguments_invalid");
        }
        let value = args
            .get(index + 1)
            .filter(|value| !value.is_empty() && !value.starts_with("--"))
            .ok_or("provider_screening_arguments_invalid")?;
        if !matches!(flag, "--subscription-free-source" | "--provider") && !seen.insert(flag) {
            return Err("provider_screening_duplicate_argument");
        }
        match flag {
            "--database" => options.database = value.into(),
            "--source" => options.source = value.into(),
            "--plan" => options.plan_id = Some(value.clone()),
            "--backup" => options.backup = Some(value.into()),
            "--provider" if mode == Mode::Recheck => {
                if value.len() > 256
                    || value.trim().is_empty()
                    || value.chars().any(char::is_control)
                {
                    return Err("provider_screening_provider_id_invalid");
                }
                if !options.provider_ids.contains(value) {
                    options.provider_ids.push(value.clone());
                }
            }
            "--subscription-free-source" => {
                if value.len() > 256
                    || !value.contains(':')
                    || !value.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric()
                            || matches!(byte, b'_' | b'-' | b'.' | b':' | b'+')
                    })
                {
                    return Err("provider_screening_source_reference_invalid");
                }
                options.subscription_free_sources.push(value.clone());
            }
            _ => return Err("provider_screening_arguments_invalid"),
        }
        index += 2;
    }
    if (mode == Mode::Apply && (options.plan_id.is_none() || options.backup.is_none()))
        || (mode == Mode::Preview && (options.plan_id.is_some() || options.backup.is_some()))
        || (mode == Mode::Recheck && (options.provider_ids.is_empty() || options.backup.is_none()))
    {
        return Err("provider_screening_arguments_invalid");
    }
    options.subscription_free_sources.sort();
    options.subscription_free_sources.dedup();
    Ok(options)
}

fn safe_code(code: &str) -> &str {
    if code.len() <= 128
        && !code.is_empty()
        && code
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        && (code.starts_with("provider_")
            || code.starts_with("supplier_")
            || code.starts_with("subscription_")
            || matches!(
                code,
                "database_error"
                    | "operation_failed"
                    | "invalid_input"
                    | "app_type_unsupported"
                    | "targets_aag_proxy"
                    | "upstream_unresolved"
            ))
    {
        code
    } else {
        "provider_screening_operation_failed"
    }
}

fn preview_report(
    preview: &SupplierImportPreview,
    options: &Options,
    policy: &ScreeningConfig,
) -> Value {
    let mut problems = BTreeMap::<String, usize>::new();
    for problem in &preview.problems {
        *problems.entry(safe_code(&problem.code).into()).or_default() += 1;
    }
    let suppliers: BTreeSet<_> = preview
        .plans
        .iter()
        .map(|entry| entry.supplier_id.as_str())
        .collect();
    let providers:Vec<_>=preview.plans.iter().map(|entry|json!({
        "supplierId":entry.supplier_id,
        "sourceRecordRef":import_cc_switch::subscription_source_ref(&entry.source_app_type,&entry.source_id),
        "name":entry.name,"protocol":entry.protocol,"configuredModels":entry.model_ids,
        "configuredModelCount":entry.model_count,"modelsTruncated":entry.model_count>entry.model_ids.len(),"change":entry.change
    })).collect();
    json!({"ok":true,"mode":if options.mode==Mode::Preview {"preview"}else{"apply"},
        "planId":preview.plan_id,"sourceRows":preview.total_rows,"supplierCount":suppliers.len(),"endpointPlans":preview.plans.len(),
        "excludedAccounts":preview.excluded_oauth_count,"unsupported":preview.pending_unsupported.len(),"problemCounts":problems,
        "providers":providers,"subscriptionFreeSourceRefs":options.subscription_free_sources,
        "latencyThresholdMs":policy.max_latency_ms,"probeDeadlineMs":policy.probe_deadline_ms,
        "screeningExecuted":false,"screeningCompleted":false,"stage":"preview","backupCreated":false,"applied":false,"runtimeReloadRequired":false})
}

#[derive(PartialEq, Eq)]
struct RecheckTarget {
    provider_id: String,
    configuration_identity: String,
    receipt_version: u64,
}

fn validate_recheck_targets(
    db: &Database,
    provider_ids: &[String],
) -> Result<Vec<RecheckTarget>, String> {
    use ai_agent_gateway_lib::database::dao::provider_admission::provider_configuration_identity;
    if provider_ids.is_empty() {
        return Err("provider_screening_arguments_invalid".into());
    }
    let mut seen = BTreeSet::new();
    let mut targets = Vec::new();
    for id in provider_ids {
        if !seen.insert(id) {
            continue;
        }
        let provider = db
            .get_proxy_provider(id)
            .map_err(|_| "provider_screening_recheck_unavailable")?
            .ok_or("provider_screening_recheck_not_registered")?;
        let record = db
            .provider_admission_record(id)
            .map_err(|_| "provider_screening_recheck_unavailable")?
            .ok_or("provider_screening_recheck_not_abnormal")?;
        if !record
            .probe
            .as_ref()
            .is_some_and(|probe| !probe.eligible && probe.reason == "abnormal_response")
        {
            return Err("provider_screening_recheck_not_abnormal".into());
        }
        let configuration_identity = provider_configuration_identity(&provider);
        if record.configuration_identity != configuration_identity {
            return Err("provider_screening_recheck_target_changed".into());
        }
        targets.push(RecheckTarget {
            provider_id: id.clone(),
            configuration_identity,
            receipt_version: record.version,
        });
    }
    Ok(targets)
}

fn recheck_outcome(
    provider_id: &str,
    result: Result<(), String>,
    entry: Option<&review::Screening_entry>,
) -> Value {
    let mut outcome = json!({"providerId":provider_id,"recheckCompleted":result.is_ok(),
        "metadataAvailable":entry.is_some(),"eligible":false,"admittedModels":[]});
    if let Err(error) = result {
        outcome["error"] = json!(safe_code(&error));
    }
    if let Some(entry) = entry {
        // Explicit metadata allowlist: never serialize the overview entry, which
        // also carries the private captured response and presentation fields.
        outcome["eligible"] = json!(!entry.admitted_models.is_empty());
        outcome["admittedModels"] = json!(entry.admitted_models);
        outcome["subscriptionState"] = json!(entry.subscription_state);
        outcome["subscriptionReason"] = json!(entry.subscription_reason);
        outcome["stage"] = json!(entry.stage);
        outcome["reason"] = json!(entry.reason);
        outcome["probeModel"] = json!(entry.probe_model);
        outcome["checkedAt"] = json!(entry.checked_at);
        outcome["latencyMs"] = json!(entry.latency_ms);
    }
    outcome
}

async fn run_recheck(options: &Options) -> Result<Value, String> {
    // Refuse the entire selection before opening writable or creating a backup.
    let targets = {
        let db = Database::open_existing_for_operator(&options.database, true)
            .map_err(|error| error.external_message())?;
        validate_recheck_targets(&db, &options.provider_ids)?
    };
    let db = Database::open_existing_for_operator(&options.database, false)
        .map_err(|error| error.external_message())?;
    if validate_recheck_targets(&db, &options.provider_ids)? != targets {
        return Err("provider_screening_recheck_target_changed".into());
    }
    let backup = options
        .backup
        .as_ref()
        .ok_or("provider_screening_arguments_invalid")?;
    db.create_operator_snapshot(backup)
        .map_err(|error| error.external_message())?;
    let mut results = Vec::new();
    let mut executed = false;
    for target in targets {
        let current = validate_recheck_targets(&db, std::slice::from_ref(&target.provider_id));
        let result = match current {
            Ok(current) if current.as_slice() == std::slice::from_ref(&target) => {
                executed = true;
                review::recheck_registered(&db, &target.provider_id).await
            }
            Ok(_) => Err("provider_screening_recheck_target_changed".into()),
            Err(error) => Err(error),
        };
        results.push((target.provider_id, result));
    }
    let completed = results.iter().all(|(_, result)| result.is_ok());
    let mut report = json!({"ok":completed,"mode":"recheck","stage":"complete",
        "backupCreated":true,"screeningExecuted":executed,"screeningCompleted":completed,
        "runtimeReloadRequired":executed,"selectedProviderCount":results.len()});
    let overview = match review::overview(&db) {
        Ok(overview) => Some(overview),
        Err(error) => {
            report["ok"] = json!(false);
            report["overviewError"] = json!(safe_code(&error));
            None
        }
    };
    let outcomes: Vec<_> = results
        .into_iter()
        .map(|(id, result)| {
            let entry = overview.as_ref().and_then(|overview| {
                overview
                    .entries
                    .iter()
                    .find(|entry| entry.provider_id == id)
            });
            recheck_outcome(&id, result, entry)
        })
        .collect();
    if outcomes
        .iter()
        .any(|outcome| outcome["metadataAvailable"] != true)
    {
        report["ok"] = json!(false);
    }
    report["admittedConnections"] = json!(outcomes
        .iter()
        .filter(|outcome| outcome["eligible"] == true)
        .count());
    report["rechecks"] = json!(outcomes);
    Ok(report)
}

async fn run(args: &[String]) -> Result<Value, String> {
    if args == ["--help"] {
        return Ok(
            json!({"ok":true,"usage":USAGE,"scope":"Preview performs local reads only. Apply makes configured status requests, then fixed ok probes only for eligible sources, and imports admitted connections under a full private backup. Recheck explicitly selected registered providers with prior abnormal_response receipts under one fresh backup. No process restart or client-file writes.",
            "defaults":"AAG_CONFIG_DIR or the normal AAG database; CC_SWITCH_DB_PATH or the normal CC Switch source.",
            "subscriptionFreeSource":"Explicit exact CC Switch source reference for a subscription-free loopback provider only; never inferred from its name."}),
        );
    }
    let options = arguments(args).map_err(str::to_owned)?;
    if !options.database.is_absolute()
        || (options.mode != Mode::Recheck && !options.source.is_absolute())
    {
        return Err("provider_screening_absolute_paths_required".into());
    }
    if options.mode != Mode::Recheck
        && options
            .database
            .canonicalize()
            .ok()
            .zip(options.source.canonicalize().ok())
            .is_some_and(|(database, source)| database == source)
    {
        return Err("provider_screening_source_target_same".into());
    }
    if let Some(path) = options.backup.as_ref() {
        if !path.is_absolute()
            || path
                .try_exists()
                .map_err(|_| "provider_screening_backup_unavailable")?
        {
            return Err("provider_screening_backup_path_invalid".into());
        }
    }
    if options.mode == Mode::Recheck {
        return run_recheck(&options).await;
    }
    let db = Database::open_existing_for_operator(&options.database, options.mode == Mode::Preview)
        .map_err(|error| error.external_message())?;
    let preview = import_cc_switch::preview_import(&db, &options.source)
        .map_err(|error| error.external_message())?;
    let policy = ScreeningConfig::load(&db)?;
    let mut report = preview_report(&preview, &options, &policy);
    if options.mode == Mode::Preview {
        return Ok(report);
    }
    if preview.plans.is_empty() {
        return Err("provider_screening_no_candidates".into());
    }
    let plan_id = options
        .plan_id
        .as_deref()
        .ok_or("provider_screening_arguments_invalid")?;
    if plan_id != preview.plan_id {
        return Err("provider_screening_stale_plan".into());
    }
    let backup = options
        .backup
        .as_ref()
        .ok_or("provider_screening_arguments_invalid")?;
    // Screening writes protected admission/source records before import, so the
    // snapshot must precede both operations, including failed screening runs.
    db.create_operator_snapshot(backup)
        .map_err(|error| error.external_message())?;
    report["backupCreated"] = json!(true);
    report["screeningExecuted"] = json!(true);
    report["stage"] = json!("screening");
    report["runtimeReloadRequired"] = json!(true);
    let outcomes = match import_cc_switch::screen_import(
        &db,
        &options.source,
        plan_id,
        &options.subscription_free_sources,
    )
    .await
    {
        Ok(outcomes) => outcomes,
        Err(code) => {
            report["ok"] = json!(false);
            report["error"] = json!(safe_code(&code));
            return Ok(report);
        }
    };
    let admitted = outcomes.iter().filter(|outcome| outcome.eligible).count();
    let subscription_reasons: BTreeMap<_, _> =
        ai_agent_gateway_lib::services::provider_admission::review::overview(&db)?
            .entries
            .into_iter()
            .map(|entry| (entry.provider_id, entry.subscription_reason))
            .collect();
    report["screeningCompleted"] = json!(true);
    // Select output fields explicitly: captured response text and future private
    // outcome fields must never flow into this terminal receipt by serialization.
    report["screening"]=json!(outcomes.iter().map(|outcome|json!({"providerId":outcome.provider_id,"sourceRecordRef":outcome.source_record_ref,
        "model":outcome.model,"eligible":outcome.eligible,"stage":outcome.stage,"reason":outcome.reason,
        "latencyMs":outcome.latency_ms,"checkedAt":outcome.checked_at,"subscriptionReason":subscription_reasons.get(&outcome.provider_id)})).collect::<Vec<_>>());
    report["admittedConnections"] = json!(admitted);
    report["excludedConnections"] = json!(outcomes.len() - admitted);
    report["stage"] = json!("import");
    match import_cc_switch::apply_import(&db, &options.source, plan_id) {
        Ok(result) => {
            report["result"] =
                serde_json::to_value(result).map_err(|_| "provider_screening_receipt_failed")?;
            report["applied"] = json!(true);
            report["stage"] = json!("complete");
        }
        Err(error) => {
            report["ok"] = json!(false);
            report["error"] = json!(safe_code(&error.external_message()));
        }
    }
    Ok(report)
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let report = match run(&args).await {
        Ok(report) => report,
        Err(code) => json!({"ok":false,"error":safe_code(&code)}),
    };
    println!("{report}");
    if report["ok"] != true {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[tokio::test]
    async fn help_returns_success_without_opening_a_database() {
        let report = run(&args(&["--help"])).await.unwrap();
        assert_eq!(report["ok"], true);
        assert!(report["usage"].as_str().unwrap().contains("--plan"));
    }

    #[test]
    fn operator_requires_explicit_apply_plan_and_backup_and_exact_local_refs() {
        assert!(arguments(&args(&["preview"])).is_ok());
        for invalid in [
            vec!["apply"],
            vec!["apply", "--plan", "id"],
            vec!["preview", "--backup", "path"],
            vec![
                "apply",
                "--plan",
                "id",
                "--backup",
                "path",
                "--refresh-subscriptions",
            ],
            vec!["preview", "--source", "a", "--source", "b"],
            vec!["preview", "--subscription-free-source", "*"],
        ] {
            assert!(arguments(&args(&invalid)).is_err());
        }
        let parsed = arguments(&args(&[
            "apply",
            "--plan",
            "id",
            "--backup",
            "path",
            "--subscription-free-source",
            "claude_code:fixture",
            "--subscription-free-source",
            "claude_code:fixture",
        ]))
        .unwrap();
        assert_eq!(parsed.subscription_free_sources, ["claude_code:fixture"]);
    }

    #[test]
    fn recheck_requires_backup_and_provider_ids_and_rejects_import_flags() {
        let parsed = arguments(&args(&[
            "recheck",
            "--provider",
            "second",
            "--provider",
            "first",
            "--provider",
            "second",
            "--backup",
            "new.db",
            "--database",
            "current.db",
        ]))
        .unwrap();
        assert_eq!(parsed.provider_ids, ["second", "first"]);
        assert_eq!(parsed.backup, Some(PathBuf::from("new.db")));
        for invalid in [
            vec!["recheck"],
            vec!["recheck", "--provider", "first"],
            vec!["recheck", "--backup", "new.db"],
            vec![
                "recheck",
                "--provider",
                "first",
                "--backup",
                "a",
                "--backup",
                "b",
            ],
            vec![
                "recheck",
                "--provider",
                "first",
                "--backup",
                "a",
                "--source",
                "source.db",
            ],
            vec![
                "recheck",
                "--provider",
                "first",
                "--backup",
                "a",
                "--plan",
                "plan",
            ],
            vec![
                "recheck",
                "--provider",
                "first",
                "--backup",
                "a",
                "--subscription-free-source",
                "claude_code:first",
            ],
            vec!["preview", "--provider", "first"],
            vec![
                "apply",
                "--plan",
                "plan",
                "--backup",
                "a",
                "--provider",
                "first",
            ],
        ] {
            assert!(arguments(&args(&invalid)).is_err());
        }
    }

    #[tokio::test]
    async fn recheck_refuses_relative_or_existing_backup_before_opening_database() {
        let root = tempfile::tempdir().unwrap();
        let database = root.path().join("missing_database.db");
        let existing = root.path().join("existing_backup.db");
        std::fs::write(&existing, b"fixture_backup").unwrap();
        for backup in [PathBuf::from("relative_backup.db"), existing.clone()] {
            let result = run(&vec![
                "recheck".into(),
                "--provider".into(),
                "fixture".into(),
                "--database".into(),
                database.to_string_lossy().into_owned(),
                "--backup".into(),
                backup.to_string_lossy().into_owned(),
            ])
            .await;
            assert_eq!(
                result.err().as_deref(),
                Some("provider_screening_backup_path_invalid")
            );
        }
        assert!(!database.exists());
        assert_eq!(std::fs::read(existing).unwrap(), b"fixture_backup");
    }

    fn recheck_fixture(db: &Database, id: &str, reason: &str, registered: bool) {
        use ai_agent_gateway_lib::database::dao::provider_admission::{
            Admission_evidence, Admission_probe, Admission_state,
        };
        let provider = serde_json::from_value(json!({"id":id,"name":"Fixture","type":"anthropic",
            "baseURL":"http://127.0.0.1:9876","apiKey":"private_fixture_key","models":["fixture_model"]})).unwrap();
        if registered {
            db.save_proxy_provider(&provider).unwrap();
        }
        let evidence = Admission_evidence {
            revision: format!("fixture_{id}"),
            state: Admission_state::NotRequired,
            valid_until: None,
        };
        let version = db
            .provider_admission_begin(&provider, &evidence, "fixture_policy", 0)
            .unwrap();
        db.provider_admission_finish(
            &provider,
            &evidence,
            "fixture_policy",
            version,
            Admission_probe {
                probe_model: "fixture_model".into(),
                checked_at: chrono::Utc::now().timestamp(),
                eligible: false,
                reason: reason.into(),
                latency_ms: Some(12),
                response: Some("private_response_marker".into()),
                response_truncated: false,
                response_redacted: false,
                upstream_status: None,
                upstream_code: None,
            },
            vec![],
        )
        .unwrap();
    }

    #[test]
    fn recheck_preflight_allows_only_current_registered_abnormal_receipts_without_writes() {
        let db = Database::memory().unwrap();
        recheck_fixture(&db, "abnormal", "abnormal_response", true);
        recheck_fixture(&db, "other_failure", "timeout", true);
        recheck_fixture(&db, "provisional", "abnormal_response", false);
        let before = db
            .provider_admission_record("abnormal")
            .unwrap()
            .unwrap()
            .version;
        let targets = validate_recheck_targets(&db, &args(&["abnormal", "abnormal"])).unwrap();
        assert_eq!(targets.len(), 1);
        for rejected in ["missing", "other_failure", "provisional"] {
            assert!(validate_recheck_targets(&db, &args(&["abnormal", rejected])).is_err());
        }
        assert!(!db.provider_admission_enforced().unwrap());
        assert_eq!(
            db.provider_admission_record("abnormal")
                .unwrap()
                .unwrap()
                .version,
            before
        );
        let mut changed = db.get_proxy_provider("abnormal").unwrap().unwrap();
        changed.base_url = "http://127.0.0.1:9877".into();
        db.save_proxy_provider(&changed).unwrap();
        assert_eq!(
            validate_recheck_targets(&db, &args(&["abnormal"]))
                .err()
                .as_deref(),
            Some("provider_screening_recheck_target_changed")
        );
    }

    #[test]
    fn recheck_receipt_excludes_private_overview_fields() {
        let entry = review::Screening_entry {
            provider_id: "fixture".into(),
            name: "private_name_marker".into(),
            admitted_models: vec!["model".into()],
            subscription_state: "active".into(),
            subscription_reason: "verified_allowance".into(),
            stage: "admission".into(),
            reason: "accepted".into(),
            probe_model: Some("model".into()),
            checked_at: Some(100),
            latency_ms: Some(12),
            response: Some(
                "private_response_marker private_fixture_key https://private.invalid".into(),
            ),
            response_truncated: false,
            response_redacted: false,
        };
        let report = recheck_outcome("fixture", Ok(()), Some(&entry));
        let encoded = report.to_string();
        for forbidden in [
            "private_name_marker",
            "private_response_marker",
            "private_fixture_key",
            "https://",
            "response",
        ] {
            assert!(!encoded.contains(forbidden));
        }
        assert_eq!(report["reason"], "accepted");
        assert_eq!(report["latencyMs"], 12);
        assert_eq!(report["admittedModels"], json!(["model"]));
    }

    #[test]
    fn terminal_preview_excludes_urls_headers_scripts_and_detailed_errors() {
        let preview = SupplierImportPreview {
            plan_id: "plan_fixture".into(),
            source_fingerprint: "not_printed".into(),
            target_generation: 1,
            created_at: 1,
            source_label: "private_source_marker".into(),
            total_rows: 1,
            pending_unsupported: vec![],
            excluded_oauth_count: 0,
            problems: vec![import_cc_switch::SupplierImportProblem {
                source_app_type: None,
                source_id: None,
                code: "supplier_import_fixture_problem".into(),
                detail: "private_detail_marker".into(),
            }],
            plans: vec![import_cc_switch::SupplierImportPlanEntry {
                supplier_id: "fixture".into(),
                source_app_type: "claude_code".into(),
                source_id: "source".into(),
                name: "Fixture provider".into(),
                protocol: "anthropic".into(),
                base_url: Some("https://private_endpoint_marker.invalid".into()),
                api_key_configured: true,
                header_names: vec!["private_header_marker".into()],
                model_ids: vec!["model_fixture".into()],
                model_count: 1,
                change: "create".into(),
                legacy_projection_planned: true,
                source_endpoint_urls: vec!["https://private_alternative_marker.invalid".into()],
            }],
        };
        let report = preview_report(
            &preview,
            &arguments(&args(&["preview"])).unwrap(),
            &ScreeningConfig::default(),
        );
        let encoded = report.to_string();
        for forbidden in [
            "private_source_marker",
            "private_detail_marker",
            "private_endpoint_marker",
            "private_header_marker",
            "private_alternative_marker",
            "https://",
        ] {
            assert!(!encoded.contains(forbidden));
        }
        assert_eq!(
            report["providers"][0]["sourceRecordRef"],
            "claude_code:source"
        );
        assert_eq!(
            report["providers"][0]["configuredModels"],
            json!(["model_fixture"])
        );
        assert_eq!(report["screeningExecuted"], false);
        assert_eq!(
            safe_code("private error body: credential"),
            "provider_screening_operation_failed"
        );
    }
}
