//! Full-stack end-to-end test (Phase 10 live verification).
//!
//! Exercises the entire backend the way the real app does:
//!   1. Isolate env (AAG_CONFIG_DIR + USERPROFILE → temp dir) so the real
//!      user DB/home is never touched.
//!   2. Database::init() → creates + seeds (2 mock providers, 3 routes).
//!   3. Provider flow: save provider → set current → write_live_snapshot
//!      (asserts the fake ~/.claude/settings.json appears).
//!   4. Proxy flow: build Router from DB → serve() on a real TCP port →
//!      real HTTP via reqwest: GET /health, GET /v1/models,
//!      POST /v1/chat/completions (non-stream), mock-fail failover,
//!      SSE streaming with [DONE] sentinel.
//!   5. Graceful shutdown via oneshot.
//!   6. Sync config DAO roundtrip sanity.
//!
//! Runs as ONE test fn because env mutation is process-global; the suite is
//! invoked with `--test-threads=1` (see PROGRESS.md) to avoid racing the
//! env-dependent paths of other tests.

#[cfg(test)]
mod tests {
    use crate::database::dao::sync_config::{SyncConfig, SyncTarget};
    use crate::database::Database;
    use crate::provider::Provider;
    use crate::proxy::providers::build_provider;
    use crate::proxy::router::Router;
    use crate::proxy::server::{serve, ProxyAppState};
    use crate::services::provider::{write_live_snapshot, AppType};
    use serde_json::{json, Value};
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::oneshot;

    // Start at a high random-ish port and sweep forward. Avoid the
    // 39787-39791 range which collides with TIME_WAIT sockets from
    // previous failed test runs. (Ephemeral port range on Windows is
    // 49152-65535, so 49800 is comfortably below.)
    const TEST_PORT_START: u16 = 49800;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn full_stack_e2e() {
        // ── 1. Isolate from the real user environment ──
        let tmp = std::env::temp_dir().join("aag-e2e-test");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).expect("create temp dir");
        // AAG_CONFIG_DIR: where the SQLite DB lives.
        // AAG_HOME_DIR: where live agent config files (~/.claude, ~/.codex, …)
        //   are written. The `dirs` crate's home_dir() on Windows can ignore
        //   USERPROFILE mutations inside a running process, so we expose our
        //   own env override in `config::get_home_dir`.
        std::env::set_var("AAG_CONFIG_DIR", &tmp);
        std::env::set_var("AAG_HOME_DIR", &tmp);

        // ── 2. DB init + seed ──
        let db = Database::init().expect("Database::init");
        let db_path = tmp.join("aag.db");
        assert!(db_path.exists(), "aag.db should be created in the temp dir");

        let proxy_providers = db.get_all_proxy_providers().expect("get_all_proxy_providers");
        assert_eq!(proxy_providers.len(), 2, "seed should create 2 mock providers");
        let routes = db.get_all_routes().expect("get_all_routes");
        assert_eq!(routes.len(), 3, "seed should create 3 routes");

        // ── 3. Provider flow: save → current → live write ──
        let provider = Provider::new(
            "e2e-claude".into(),
            "E2E Claude Provider".into(),
            json!({
                "env": { "ANTHROPIC_BASE_URL": "http://127.0.0.1:8787" },
                "apiKey": "e2e-test-key"
            }),
        );
        db.save_provider("claude_code", &provider).expect("save_provider");
        db.set_current_provider("e2e-claude", "claude_code")
            .expect("set_current_provider");
        assert_eq!(
            db.get_current_provider("claude_code")
                .expect("get_current_provider")
                .as_deref(),
            Some("e2e-claude"),
            "current provider should be e2e-claude"
        );

        let live = write_live_snapshot(AppType::ClaudeCode, &provider, None)
            .expect("write_live_snapshot should succeed for ClaudeCode");
        assert!(!live.written.is_empty(), "should write at least one file");
        let actual_written = &live.written[0];
        eprintln!("[e2e] write_live_snapshot wrote to: {actual_written}");

        let settings_path = tmp.join(".claude").join("settings.json");
        eprintln!(
            "[e2e] checking expected path: {}",
            settings_path.display()
        );
        assert!(
            settings_path.exists(),
            "fake ~/.claude/settings.json should exist at {}",
            settings_path.display()
        );
        let settings_raw = std::fs::read_to_string(&settings_path).expect("read settings.json");
        let settings_json: Value =
            serde_json::from_str(&settings_raw).expect("settings.json must be valid JSON");
        assert!(
            settings_json
                .get("env")
                .and_then(|e| e.get("ANTHROPIC_BASE_URL"))
                .is_some(),
            "written settings should contain env.ANTHROPIC_BASE_URL, got: {settings_json}"
        );

        // ── 4. Build the proxy from the DB snapshot (mirrors proxy_start) ──
        let mut providers: HashMap<String, Arc<dyn crate::proxy::providers::Provider>> =
            HashMap::new();
        for pp in &proxy_providers {
            let p = build_provider(pp).expect("build_provider for seeded mock");
            providers.insert(pp.id.clone(), Arc::from(p));
        }
        let mut route_map: HashMap<String, Vec<crate::database::dao::routes::RouteHop>> =
            HashMap::new();
        for r in routes {
            route_map.insert(r.alias, r.hops);
        }
        let router = Arc::new(Router::new(providers, route_map));

        let (_shutdown_tx_unused, _shutdown_rx_unused) = oneshot::channel::<()>();

        // Try up to 5 consecutive ports starting from TEST_PORT_START — guards
        // against TIME_WAIT sockets from a previous failed run holding 39787.
        let (port, server_handle, shutdown_tx) = {
            let mut last_err: Option<String> = None;
            let mut result: Option<(u16, tokio::task::JoinHandle<()>, tokio::sync::oneshot::Sender<()>)> = None;
            for offset in 0u16..5 {
                let try_port = TEST_PORT_START + offset;
                let addr: std::net::SocketAddr =
                    format!("127.0.0.1:{try_port}").parse().unwrap();
                let (tx, rx) = oneshot::channel::<()>();
                let state_for_try = ProxyAppState {
                    router: Arc::clone(&router),
                    auth_token: String::new(),
                };
                let handle = tokio::spawn(async move {
                    if let Err(e) = serve(addr, state_for_try, rx).await {
                        eprintln!("[e2e] serve error on port {try_port}: {e}");
                    }
                });
                // Probe /health for ~300ms before giving up on this port.
                let probe = reqwest::Client::builder()
                    .timeout(Duration::from_millis(200))
                    .build()
                    .unwrap();
                let url = format!("http://127.0.0.1:{try_port}/health");
                let mut up = false;
                for _ in 0..6 {
                    if let Ok(r) = probe.get(&url).send().await {
                        if r.status().is_success() {
                            up = true;
                            break;
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                if up {
                    result = Some((try_port, handle, tx));
                    break;
                }
                // Bind failed / port never came up — drop the task and try
                // the next port.
                drop(handle);
                let _ = tx.send(());
                last_err = Some(format!("port {try_port} did not become ready"));
            }
            result.unwrap_or_else(|| panic!(
                "no port in {TEST_PORT_START}..{} bound: {last_err:?}",
                TEST_PORT_START + 5
            ))
        };
        eprintln!("[e2e] proxy listening on port {port}");

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("reqwest client");
        let base = format!("http://127.0.0.1:{port}");

        // Health is already confirmed during port selection; do one final check.
        let health = client
            .get(format!("{base}/health"))
            .send()
            .await
            .expect("GET /health");
        assert!(health.status().is_success(), "/health should 200");

        // ── 4a. GET /v1/models lists the seeded aliases ──
        let models_resp = client
            .get(format!("{base}/v1/models"))
            .send()
            .await
            .expect("GET /v1/models");
        assert!(models_resp.status().is_success(), "/v1/models should 200");
        let models_text = models_resp.text().await.expect("models body");
        assert!(
            models_text.contains("claude-sonnet-4"),
            "/v1/models should list claude-sonnet-4, got: {models_text}"
        );

        // ── 4b. Non-streaming completion via the seeded route ──
        let body = json!({
            "model": "claude-sonnet-4",
            "messages": [{ "role": "user", "content": "hello from e2e" }],
            "stream": false
        });
        let resp = client
            .post(format!("{base}/v1/chat/completions"))
            .json(&body)
            .send()
            .await
            .expect("POST /v1/chat/completions");
        assert!(resp.status().is_success(), "completion should 200");
        let completion: Value = resp.json().await.expect("completion JSON");
        let content = completion["choices"][0]["message"]["content"]
            .as_str()
            .expect("choices[0].message.content should be a string");
        assert!(!content.is_empty(), "mock completion content non-empty");

        // ── 4c. Failover: mock-fail route forces primary failure ──
        let fail_body = json!({
            "model": "mock-fail",
            "messages": [{ "role": "user", "content": "force failover" }],
            "stream": false
        });
        let fail_resp = client
            .post(format!("{base}/v1/chat/completions"))
            .json(&fail_body)
            .send()
            .await
            .expect("POST mock-fail");
        assert!(
            fail_resp.status().is_success(),
            "mock-fail should still 200 via failover to mock-fallback"
        );
        let fail_completion: Value = fail_resp.json().await.expect("failover JSON");
        let fail_content = fail_completion["choices"][0]["message"]["content"]
            .as_str()
            .expect("failover content string");
        assert!(
            !fail_content.is_empty(),
            "failover completion content non-empty"
        );

        // ── 4d. Streaming SSE: role delta, content deltas, [DONE] ──
        let stream_body = json!({
            "model": "claude-sonnet-4",
            "messages": [{ "role": "user", "content": "stream me" }],
            "stream": true
        });
        let stream_resp = client
            .post(format!("{base}/v1/chat/completions"))
            .json(&stream_body)
            .send()
            .await
            .expect("POST stream");
        assert!(stream_resp.status().is_success(), "stream should 200");
        let sse_text = stream_resp.text().await.expect("sse body");
        let data_lines: Vec<&str> = sse_text
            .lines()
            .filter(|l| l.starts_with("data:"))
            .collect();
        assert!(
            data_lines.len() >= 3,
            "SSE stream should have multiple data: lines, got {}:\n{sse_text}",
            data_lines.len()
        );
        assert!(
            sse_text.contains("[DONE]"),
            "SSE stream must end with [DONE] sentinel"
        );
        // First data chunk should carry the role delta.
        let first_payload = data_lines[0].trim_start_matches("data:").trim();
        let first_chunk: Value =
            serde_json::from_str(first_payload).expect("first chunk JSON");
        assert_eq!(
            first_chunk["choices"][0]["delta"]["role"].as_str(),
            Some("assistant"),
            "first SSE chunk should set role=assistant, got: {first_chunk}"
        );

        // ── 5. Graceful shutdown ──
        shutdown_tx
            .send(())
            .expect("send shutdown");
        tokio::time::timeout(Duration::from_secs(10), server_handle)
            .await
            .expect("server should shut down within 10s")
            .expect("server task should not panic");

        // ── 6. Sync config roundtrip sanity ──
        let cfg = SyncConfig {
            provider: "webdav".into(),
            enabled: true,
            endpoint: "https://dav.example.com/dav/".into(),
            bucket: None,
            remote_path: Some("/aag/".into()),
            access_key: "user".into(),
            secret_key: "pass".into(),
            region: None,
            last_sync_at: None,
            last_error: None,
        };
        db.save_sync_config(&cfg).expect("save_sync_config");
        let fetched = db
            .get_sync_config(SyncTarget::Webdav)
            .expect("get_sync_config")
            .expect("webdav row should exist");
        assert_eq!(fetched.remote_path.as_deref(), Some("/aag/"));
        db.record_sync_result(SyncTarget::Webdav, None)
            .expect("record_sync_result");
        let stamped = db
            .get_sync_config(SyncTarget::Webdav)
            .expect("get after stamp")
            .expect("row still exists");
        assert!(stamped.last_sync_at.is_some(), "last_sync_at stamped");

        // Best-effort cleanup.
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
