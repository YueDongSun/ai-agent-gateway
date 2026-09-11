//! Seed the proxy with default mock providers and routes on first run.
//!
//! Matches the Node `src/config/store.ts::seedConfig` behavior: two mock
//! providers (`mock-primary` with low breaker threshold, `mock-fallback`) and
//! three routes (`claude-sonnet-4`, `gpt-4o`, `mock-fail`).

use crate::database::{lock_conn, Database};
use crate::database::dao::proxy_providers::{CircuitBreakerConfig, ProxyProvider};
use crate::database::dao::routes::{Route, RouteHop};
use crate::error::AppError;
use rusqlite::params;

const MOCK_PRIMARY: &str = "mock-primary";
const MOCK_FALLBACK: &str = "mock-fallback";

impl Database {
    /// Seed mock providers + routes + proxy config on first run. Idempotent —
    /// only inserts when the tables are empty.
    pub fn seed_proxy_if_empty(&self) -> Result<(), AppError> {
        // Seed proxy config (singleton row) if absent
        {
            let conn = lock_conn!(self.conn);
            conn.execute(
                "INSERT OR IGNORE INTO proxy_config (id, host, port, auth_token, enabled)
                 VALUES (1, '127.0.0.1', 8787, '', 0)",
                [],
            )?;
        }

        // Seed proxy providers if empty
        let existing: Vec<String> = {
            let conn = lock_conn!(self.conn);
            let mut stmt = conn.prepare("SELECT id FROM proxy_providers")?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            let mut ids = Vec::new();
            for r in rows { ids.push(r?); }
            ids
        };
        if existing.is_empty() {
            self.save_proxy_provider(&ProxyProvider {
                id: MOCK_PRIMARY.into(),
                name: "Mock Primary (slow)".into(),
                provider_type: "mock".into(),
                api_key: "mock".into(),
                base_url: "mock://primary".into(),
                models: vec!["mock-fast".into(), "mock-slow".into(), "claude-sonnet-4".into(), "gpt-4o".into()],
                enabled: true,
                timeout_ms: 60_000,
                weight: 1,
                circuit_breaker: CircuitBreakerConfig {
                    failure_threshold: 3,
                    cooldown_ms: 15_000,
                    half_open_retries: 1,
                    success_threshold: 2,
                },
            })?;
            self.save_proxy_provider(&ProxyProvider {
                id: MOCK_FALLBACK.into(),
                name: "Mock Fallback (always works)".into(),
                provider_type: "mock".into(),
                api_key: "mock".into(),
                base_url: "mock://fallback".into(),
                models: vec!["claude-sonnet-4".into(), "gpt-4o".into(), "mock-fast".into()],
                enabled: true,
                timeout_ms: 60_000,
                weight: 1,
                circuit_breaker: CircuitBreakerConfig {
                    failure_threshold: 5,
                    cooldown_ms: 30_000,
                    half_open_retries: 1,
                    success_threshold: 2,
                },
            })?;
        }

        // Seed routes if empty
        let route_count: i64 = {
            let conn = lock_conn!(self.conn);
            conn.query_row("SELECT COUNT(*) FROM routes", [], |row| row.get(0))?
        };
        if route_count == 0 {
            self.save_route(&Route {
                alias: "claude-sonnet-4".into(),
                description: "Sonnet via mock chain (primary -> fallback)".into(),
                hops: vec![
                    RouteHop { provider: MOCK_PRIMARY.into(), model: "claude-sonnet-4".into() },
                    RouteHop { provider: MOCK_FALLBACK.into(), model: "claude-sonnet-4".into() },
                ],
            })?;
            self.save_route(&Route {
                alias: "gpt-4o".into(),
                description: "GPT-4o via mock chain".into(),
                hops: vec![
                    RouteHop { provider: MOCK_PRIMARY.into(), model: "gpt-4o".into() },
                    RouteHop { provider: MOCK_FALLBACK.into(), model: "gpt-4o".into() },
                ],
            })?;
            self.save_route(&Route {
                alias: "mock-fail".into(),
                description: "Forces failures on primary to demo circuit-breaker".into(),
                hops: vec![
                    RouteHop { provider: MOCK_PRIMARY.into(), model: "mock-fail".into() },
                    RouteHop { provider: MOCK_FALLBACK.into(), model: "mock-fast".into() },
                ],
            })?;
        }

        // Official Claude model routes (idempotent per alias): each official
        // id gets a failover chain, so Anthropic-native clients that request
        // e.g. "claude-fable-5" are transparently served by the hop chain and
        // see the official name echoed back. Runs even when the demo routes
        // above already exist, so existing installs gain the aliases on
        // upgrade. Users then edit the hops on the Proxy page to point at
        // their real providers.
        for m in crate::proxy::models::OFFICIAL_MODELS {
            let exists = {
                let conn = lock_conn!(self.conn);
                conn.query_row(
                    "SELECT COUNT(*) FROM routes WHERE alias = ?1",
                    params![m.id],
                    |row| row.get::<_, i64>(0),
                )? > 0
            };
            if !exists {
                self.save_route(&Route {
                    alias: m.id.into(),
                    description: format!(
                        "{} via failover chain (edit hops on the Proxy page)",
                        m.display_name
                    ),
                    hops: vec![
                        RouteHop { provider: MOCK_PRIMARY.into(), model: m.id.into() },
                        RouteHop { provider: MOCK_FALLBACK.into(), model: m.id.into() },
                    ],
                })?;
            }
        }

        Ok(())
    }
}