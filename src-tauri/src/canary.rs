//! Restricted live canary against the user's real API suppliers.
//!
//! The user authorized a bounded real-upstream canary. The budget is
//! enforced in code, not by convention:
//!
//! * at most 16 real generation attempts — every upstream send consumes
//!   one, including a compatibility retry, a failover hop, and the second
//!   round of a tool exchange;
//! * serial: one probe at a time, never concurrent;
//! * at most 256 output tokens per request and 60 s per request;
//! * synthetic content only — no code, no history, no workspace files;
//! * only suppliers that are already enabled API suppliers in AAG, never an
//!   official or managed account (those carry no usable static key and are
//!   refused by the supplier layer anyway);
//! * an isolated listener on a random loopback port and an isolated copy
//!   of the configuration database, so the user's runtime configuration,
//!   database, and client files are never read for writes or modified.
//!
//! The test is `#[ignore]`d and additionally requires `AAG_LIVE_CANARY=1`,
//! so it can never run as part of the normal suite.
#![cfg(test)]
#![allow(non_camel_case_types)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

const MAX_ATTEMPTS: usize = 16;
const MAX_OUTPUT_TOKENS: u32 = 256;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Remaining real generation attempts. Decremented *before* the upstream
/// send, so a request that fails over or retries cannot overshoot.
static BUDGET: AtomicUsize = AtomicUsize::new(0);
static LEDGER: std::sync::Mutex<Option<rusqlite::Connection>> = std::sync::Mutex::new(None);

fn take_attempt(connection: &rusqlite::Connection) -> Result<usize, String> {
    let changed = connection.execute(
        "UPDATE attempt_budget SET used = used + 1 WHERE id = 1 AND used < 16",
        [],
    ).map_err(|_| "canary_budget_write_failed".to_string())?;
    if changed != 1 {
        return Err("canary_attempt_budget_exhausted".into());
    }
    connection.query_row("SELECT 16 - used FROM attempt_budget WHERE id = 1", [], |row| row.get(0))
        .map_err(|_| "canary_budget_read_failed".into())
}

pub(crate) fn before_upstream_send() -> Result<(), crate::proxy::canonical::ProviderError> {
    let ledger = LEDGER.lock().unwrap_or_else(|error| error.into_inner());
    let Some(connection) = ledger.as_ref() else { return Ok(()); };
    let remaining = take_attempt(connection).map_err(|code| {
        crate::proxy::canonical::make_provider_error(&code, "canary_budget", Some(429), Some(false), None)
    })?;
    BUDGET.store(remaining, Ordering::SeqCst);
    Ok(())
}

struct Budget_guard;

impl Drop for Budget_guard {
    fn drop(&mut self) {
        *LEDGER.lock().unwrap_or_else(|error| error.into_inner()) = None;
    }
}

fn open_budget(path: &Path, initial_used: usize) -> Result<rusqlite::Connection, String> {
    if initial_used > MAX_ATTEMPTS { return Err("canary_budget_invalid".into()); }
    let connection = rusqlite::Connection::open(path).map_err(|_| "canary_budget_open_failed".to_string())?;
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS attempt_budget (id INTEGER PRIMARY KEY CHECK(id = 1), used INTEGER NOT NULL CHECK(used BETWEEN 0 AND 16));"
    ).map_err(|_| "canary_budget_init_failed".to_string())?;
    connection.execute("INSERT OR IGNORE INTO attempt_budget (id, used) VALUES (1, ?1)", [initial_used as i64])
        .map_err(|_| "canary_budget_init_failed".to_string())?;
    Ok(connection)
}

fn attempts_left() -> usize {
    BUDGET.load(Ordering::SeqCst)
}

/// Provider wrapper that enforces the synthetic request's output ceiling.
struct Budgeted_provider {
    inner: Box<dyn crate::proxy::providers::Provider>,
}

#[async_trait::async_trait]
impl crate::proxy::providers::Provider for Budgeted_provider {
    fn id(&self) -> &str {
        self.inner.id()
    }
    fn enabled(&self) -> bool {
        self.inner.enabled()
    }
    fn models(&self) -> &[String] {
        self.inner.models()
    }
    fn breaker(&self) -> &crate::proxy::circuit_breaker::CircuitBreaker {
        self.inner.breaker()
    }
    fn requires_client_credential(&self) -> bool {
        self.inner.requires_client_credential()
    }

    async fn count_tokens(
        &self,
        body: serde_json::Value,
        client_credential: Option<&crate::proxy::canonical::Client_credential>,
    ) -> Result<u32, crate::proxy::canonical::ProviderError> {
        self.inner.count_tokens(body, client_credential).await
    }

    async fn complete(
        &self,
        req: &crate::proxy::canonical::ChatCompletionRequest,
        upstream_model: &str,
    ) -> Result<crate::proxy::canonical::OAIChatResponse, crate::proxy::canonical::ProviderError> {
        if req.max_tokens.is_none_or(|tokens| tokens > MAX_OUTPUT_TOKENS) {
            return Err(crate::proxy::canonical::make_provider_error(
                "canary_output_limit", "canary_budget", Some(400), Some(false), None,
            ));
        }
        self.inner.complete(req, upstream_model).await
    }

    async fn stream(
        &self,
        req: &crate::proxy::canonical::ChatCompletionRequest,
        upstream_model: &str,
    ) -> Result<
        std::pin::Pin<Box<dyn futures_core::Stream<Item = crate::proxy::canonical::OAIStreamChunk> + Send>>,
        crate::proxy::canonical::ProviderError,
    > {
        if req.max_tokens.is_none_or(|tokens| tokens > MAX_OUTPUT_TOKENS) {
            return Err(crate::proxy::canonical::make_provider_error(
                "canary_output_limit", "canary_budget", Some(400), Some(false), None,
            ));
        }
        self.inner.stream(req, upstream_model).await
    }
}

/// Removes the temporary configuration copy when the canary ends, including
/// after a failed assertion.
struct Temp_dir(PathBuf);

impl Drop for Temp_dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn source_database_path() -> PathBuf {
    match std::env::var("AAG_LIVE_CANARY_DB") {
        Ok(path) if !path.trim().is_empty() => PathBuf::from(path),
        // The live database, not the test-overridden home: the canary is
        // explicitly pointed at the user's real configuration.
        _ => dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".aag")
            .join("aag.db"),
    }
}

/// Copy the live database into the temporary directory. The source is opened
/// read-only and never written; `VACUUM INTO` produces a consistent copy
/// including anything still in the write-ahead log.
fn copy_database(source: &Path, target: &Path) -> Result<(), String> {
    let source_url = format!(
        "file:{}?mode=ro",
        source.to_string_lossy().replace('\\', "/")
    );
    let connection = rusqlite::Connection::open_with_flags(
        &source_url,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|error| format!("canary_source_open_failed:{error}"))?;
    let target_url = target.to_string_lossy().replace('\\', "/");
    connection
        .execute("VACUUM INTO ?1", [target_url])
        .map_err(|error| format!("canary_copy_failed:{error}"))?;
    Ok(())
}

struct Probe_result {
    label: &'static str,
    status: u16,
    ok: bool,
    detail: String,
    tool_use: Option<serde_json::Value>,
}

/// Only the probes named in `AAG_LIVE_CANARY_ONLY` run. The budget is
/// global, so a targeted re-check must not repeat the probes that already
/// have evidence.
fn probe_selected(label: &str) -> bool {
    match std::env::var("AAG_LIVE_CANARY_ONLY") {
        Ok(list) if !list.trim().is_empty() => list
            .split(',')
            .map(|entry| entry.trim())
            .any(|entry| !entry.is_empty() && entry == label),
        _ => true,
    }
}

/// Stable machine codes from an error body, never its free text.
fn error_codes(body: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return "unparsed_error_body".to_string();
    };
    let error = value.get("error").unwrap_or(&value);
    let mut codes: Vec<String> = Vec::new();
    for key in ["type", "code", "status"] {
        if let Some(found) = error.get(key) {
            match found {
                serde_json::Value::String(text) => codes.push(match text.as_str() {
                    "api_error" | "invalid_request_error" | "upstream_http_error" |
                    "authentication_error" | "rate_limit_error" | "permission_error" => text.clone(),
                    _ => "other_error_code".into(),
                }),
                serde_json::Value::Number(number) if number.as_u64().is_some_and(|code| (400..=599).contains(&code)) => codes.push(number.to_string()),
                _ => codes.push("other_error_code".into()),
            }
        }
    }
    if codes.is_empty() {
        "no_error_codes".to_string()
    } else {
        codes.join(",")
    }
}

#[tokio::test]
#[ignore = "restricted live canary: needs explicit authorization and enabled API suppliers"]
async fn restricted_live_canary() {
    if std::env::var("AAG_LIVE_CANARY").as_deref() != Ok("1") {
        panic!("AAG_LIVE_CANARY=1 is required for the live canary");
    }
    let budget_path = PathBuf::from(std::env::var("AAG_LIVE_CANARY_BUDGET_DB")
        .expect("a persistent authorization-budget receipt path is required"));
    let initial_used = std::env::var("AAG_LIVE_CANARY_ATTEMPTS_USED")
        .expect("the cumulative attempts already used under this authorization are required")
        .parse::<usize>().expect("attempt count must be an integer");
    let parent = budget_path.parent().expect("budget receipt needs a parent");
    crate::config::private_fs::prepare_private_dir(parent).expect("private budget directory");
    crate::config::private_fs::preflight_sqlite_files(&budget_path).expect("private budget file");
    let budget = open_budget(&budget_path, initial_used).expect("open persistent attempt budget");
    crate::config::private_fs::secure_existing_file(&budget_path).expect("secure budget receipt");
    let remaining = budget.query_row("SELECT 16 - used FROM attempt_budget WHERE id = 1", [], |row| row.get::<_, usize>(0))
        .expect("read remaining authorization budget");
    BUDGET.store(remaining, Ordering::SeqCst);
    *LEDGER.lock().unwrap() = Some(budget);
    let _budget_guard = Budget_guard;
    let source = source_database_path();
    assert!(
        source.exists(),
        "canary source database is missing: {}",
        source.display()
    );

    // Isolated copy of the configuration database. The user's own database
    // is opened read-only and never written.
    let temp_root = std::env::temp_dir().join(format!("aag-canary-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp_root);
    std::fs::create_dir_all(&temp_root).expect("create canary temp dir");
    let _temp = Temp_dir(temp_root.clone());
    let copy_path = temp_root.join("aag.db");
    copy_database(&source, &copy_path).expect("copy the live database read-only");

    let db = crate::database::Database::init_at_path(&copy_path).expect("open the canary copy");
    // Public metadata only: models.dev and EvalPlus documents are metadata
    // fetches, not generation calls, so they do not spend an attempt. The
    // refresh lands in the isolated copy and gives alias planning the
    // capability evidence it needs.
    for source in ["models_dev", "evalplus"] {
        let refreshed = crate::services::model_catalog::refresh_source(
            &db,
            &crate::services::model_catalog::CatalogRefreshInput {
                source: source.to_string(),
            },
        )
        .await;
        println!(
            "canary: catalog source={} refreshed={}",
            source,
            refreshed.as_ref().is_ok_and(|view| !view.stale && view.last_error.is_none())
        );
    }
    let config = db.get_proxy_config().expect("read proxy config");
    let db_providers = db.get_all_proxy_providers().expect("read proxy providers");

    // Only enabled API suppliers participate. Mock providers, passthrough
    // (official-account) providers, and credential-less rows are refused.
    let mut providers: std::collections::HashMap<
        String,
        Arc<dyn crate::proxy::providers::Provider>,
    > = std::collections::HashMap::new();
    let mut usable = 0usize;
    for row in &db_providers {
        if !row.enabled || row.provider_type == "mock" {
            continue;
        }
        if row.provider_type == "anthropic_passthrough" {
            // Official-account forwarding is not implemented; never spend a
            // canary attempt on it.
            continue;
        }
        if row.api_key.trim().is_empty() {
            continue;
        }
        let built = crate::proxy::providers::build_provider(row).expect("build provider");
        providers.insert(
            row.id.clone(),
            Arc::new(Budgeted_provider { inner: built }) as Arc<dyn crate::proxy::providers::Provider>,
        );
        usable += 1;
    }
    assert!(usable > 0, "no enabled API supplier with a static key");

    let routes: std::collections::HashMap<String, Vec<crate::database::dao::routes::RouteHop>> = db
        .get_all_routes()
        .expect("read routes")
        .into_iter()
        .map(|route| (route.alias, route.hops))
        .collect();
    let planner = crate::services::proxy_runtime::planner::build_model_planner(&db, &db_providers)
        .expect("build the capability planner");
    let router = crate::proxy::router::Router::new(providers, routes).with_planner(planner);

    let state = crate::proxy::server::ProxyAppState {
        router: Arc::new(router),
        auth_token: config.auth_token.clone(),
        pinned_provider: Arc::new(std::sync::Mutex::new(None)),
        responses_cot_enabled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        passthrough_auth_allowed: false,
        // No usage database: the canary must not write usage rows anywhere.
        db: None,
        live: None,
        allow_client_config_repair: false,
    };
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let (bound, handle) = crate::proxy::server::serve_detached(
        "127.0.0.1:0".parse().expect("loopback address"),
        state,
        shutdown_rx,
    )
    .await
    .expect("start the isolated canary listener");
    let base = format!("http://{}", bound.addr);

    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .expect("build the canary client");
    let mut auth = reqwest::header::HeaderMap::new();
    if !config.auth_token.trim().is_empty() {
        auth.insert(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", config.auth_token)
                .parse()
                .expect("auth header value"),
        );
    }

    let mut results: Vec<Probe_result> = Vec::new();

    // 1. Anthropic client protocol.
    results.push(
        probe(
            &client,
            &auth,
            &base,
            "messages_surface",
            "/v1/messages",
            json!({
                "model": "claude-sonnet-5",
                "max_tokens": MAX_OUTPUT_TOKENS / 4,
                "messages": [{"role": "user", "content": "Reply with exactly the word PONG."}],
            }),
        )
        .await,
    );

    // 2. OpenAI chat protocol against the same supply (cross-protocol).
    results.push(
        probe(
            &client,
            &auth,
            &base,
            "chat_surface",
            "/v1/chat/completions",
            json!({
                "model": "claude-sonnet-5",
                "max_tokens": MAX_OUTPUT_TOKENS / 4,
                "messages": [{"role": "user", "content": "Reply with exactly the word PONG."}],
            }),
        )
        .await,
    );

    // 3. Responses client protocol.
    results.push(
        probe(
            &client,
            &auth,
            &base,
            "responses_surface",
            "/v1/responses",
            json!({
                "model": "codex-5-6-sol",
                "max_output_tokens": MAX_OUTPUT_TOKENS / 4,
                "input": "Reply with exactly the word PONG.",
            }),
        )
        .await,
    );

    // 4. Gemini client protocol.
    results.push(
        probe(
            &client,
            &auth,
            &base,
            "gemini_surface",
            "/v1beta/models/gemini-3.8-flash:generateContent",
            json!({
                "contents": [{"role": "user", "parts": [{"text": "Reply with exactly the word PONG."}]}],
                "generationConfig": {"maxOutputTokens": MAX_OUTPUT_TOKENS / 4},
            }),
        )
        .await,
    );

    // 5. Tool round trip: one request that must call the tool, one request
    //    that carries the tool result back.
    let tool_first = probe(
        &client,
        &auth,
        &base,
        "tool_call_round",
        "/v1/messages",
        json!({
            "model": "claude-sonnet-5",
            "max_tokens": MAX_OUTPUT_TOKENS / 2,
            "tools": [{
                "name": "ping",
                "description": "Return a ping counter.",
                "input_schema": {"type": "object", "properties": {"count": {"type": "integer"}}}
            }],
            "tool_choice": {"type": "tool", "name": "ping"},
            "messages": [{"role": "user", "content": "Call the ping tool once with count 1."}],
        }),
    )
    .await;
    let tool_block = if probe_selected("tool_call_round") {
        tool_first.tool_use.clone()
    } else {
        None
    };
    let tool_call_available = tool_block.is_some();
    results.push(tool_first);
    if let Some(tool_block) = tool_block {
    results.push(
        probe(
            &client,
            &auth,
            &base,
            "tool_result_round",
            "/v1/messages",
            json!({
                "model": "claude-sonnet-5",
                "max_tokens": MAX_OUTPUT_TOKENS / 2,
                "tools": [{
                    "name": "ping",
                    "description": "Return a ping counter.",
                    "input_schema": {"type": "object", "properties": {"count": {"type": "integer"}}}
                }],
                "messages": [
                    {"role": "user", "content": "Call the ping tool once with count 1."},
                    {"role": "assistant", "content": [tool_block.clone()]},
                    {"role": "user", "content": [
                        {"type": "tool_result", "tool_use_id": tool_block["id"], "content": "pong"}
                    ]}
                ],
            }),
        )
        .await,
    );
    } else if probe_selected("tool_result_round") {
        results.push(Probe_result { label: "tool_result_round", status: 0, ok: false,
            detail: "tool_call_not_emitted".into(), tool_use: None });
    }

    // 6. Capability alias: the planner must pick a qualifying model.
    results.push(
        probe(
            &client,
            &auth,
            &base,
            "alias_fable",
            "/v1/messages",
            json!({
                "model": "fable",
                "max_tokens": MAX_OUTPUT_TOKENS / 4,
                "messages": [{"role": "user", "content": "Reply with exactly the word PONG."}],
            }),
        )
        .await,
    );

    let _ = shutdown_tx.send(());
    let _ = handle.await;

    let mut covered = 0usize;
    let mut failed = 0usize;
    for result in &results {
        if result.ok {
            covered += 1;
        } else if result.detail != "skipped_by_selection" {
            failed += 1;
        }
        println!(
            "canary: probe={} status={} ok={} detail={}",
            result.label, result.status, result.ok, result.detail
        );
    }
    println!(
        "canary: tool_call_emitted={} probes_covered={} probes_failed={} attempts_left={}",
        tool_call_available, covered, failed, attempts_left()
    );
    assert!(covered > 0 && failed == 0, "one or more selected canary probes failed");
}

fn valid_response(path: &str, value: &serde_json::Value) -> bool {
    let nonempty = |value: Option<&serde_json::Value>| value.and_then(|value| value.as_array()).is_some_and(|items| !items.is_empty());
    match path {
        "/v1/messages" => value["type"] == "message" && nonempty(value.get("content")),
        "/v1/chat/completions" => nonempty(value.get("choices")),
        "/v1/responses" => value["object"] == "response" && value["status"] == "completed" && nonempty(value.get("output")),
        _ => nonempty(value.get("candidates")),
    }
}

#[test]
fn budget_receipt_survives_reopening_and_refuses_attempt_seventeen() {
    let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("aag-budget-test-{}-{unique}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let _temp = Temp_dir(root.clone());
    let path = root.join("receipt.db");
    let connection = open_budget(&path, 14).unwrap();
    assert_eq!(take_attempt(&connection).unwrap(), 1);
    drop(connection);
    let connection = open_budget(&path, 0).unwrap();
    assert_eq!(take_attempt(&connection).unwrap(), 0);
    assert_eq!(take_attempt(&connection).unwrap_err(), "canary_attempt_budget_exhausted");
}

/// Run one canary request and summarize the outcome without echoing content.
async fn probe(
    client: &reqwest::Client,
    auth: &reqwest::header::HeaderMap,
    base: &str,
    label: &'static str,
    path: &str,
    body: serde_json::Value,
) -> Probe_result {
    if !probe_selected(label) {
        return Probe_result {
            label,
            status: 0,
            ok: false,
            detail: "skipped_by_selection".to_string(),
            tool_use: None,
        };
    }
    let response = client
        .post(format!("{base}{path}"))
        .headers(auth.clone())
        .json(&body)
        .send()
        .await;
    match response {
        Ok(response) => {
            let status = response.status().as_u16();
            let text = response.text().await.unwrap_or_default();
            let value = serde_json::from_str::<serde_json::Value>(&text).ok();
            let valid = value.as_ref().is_some_and(|body| valid_response(path, body));
            let tool_use = value.as_ref().and_then(|body| body.get("content"))
                .and_then(|content| content.as_array())
                .and_then(|content| content.iter().find(|block| block["type"] == "tool_use" && block["name"] == "ping"))
                .cloned();
            let detail = if status < 400 {
                format!("bytes={} valid_shape={}", text.len(), valid)
            } else {
                format!("bytes={} codes={}", text.len(), error_codes(&text))
            };
            Probe_result {
                label,
                status,
                ok: status < 400 && valid && (label != "tool_call_round" || tool_use.is_some()),
                detail,
                tool_use,
            }
        }
        Err(error) => Probe_result {
            label,
            status: 0,
            ok: false,
            detail: format!("transport_error={}", error.is_timeout()),
            tool_use: None,
        },
    }
}
