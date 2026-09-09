//! Proxy providers DAO (proxy_providers table).

use crate::database::{lock_conn, Database};
use crate::error::AppError;
use rusqlite::params;
use serde::{Deserialize, Serialize};

/// An upstream API provider used by the proxy for failover chains.
/// Separate from the `providers` table (which holds per-agent config snapshots).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyProvider {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub provider_type: String, // "mock" | "openai" | "anthropic"
    #[serde(rename = "apiKey", default)]
    pub api_key: String,
    #[serde(rename = "baseURL", default)]
    pub base_url: String,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(rename = "timeoutMs", default = "default_timeout")]
    pub timeout_ms: u64,
    #[serde(default = "default_weight")]
    pub weight: u32,
    #[serde(rename = "circuitBreaker", default)]
    pub circuit_breaker: CircuitBreakerConfig,
}

fn default_true() -> bool { true }
fn default_timeout() -> u64 { 60_000 }
fn default_weight() -> u32 { 1 }

/// Circuit breaker parameters for a proxy provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CircuitBreakerConfig {
    #[serde(rename = "failureThreshold", default = "default_failure_threshold")]
    pub failure_threshold: u32,
    #[serde(rename = "cooldownMs", default = "default_cooldown_ms")]
    pub cooldown_ms: u64,
    #[serde(rename = "halfOpenRetries", default = "default_half_open_retries")]
    pub half_open_retries: u32,
    #[serde(rename = "successThreshold", default = "default_success_threshold")]
    pub success_threshold: u32,
}

fn default_failure_threshold() -> u32 { 5 }
fn default_cooldown_ms() -> u64 { 30_000 }
fn default_half_open_retries() -> u32 { 1 }
fn default_success_threshold() -> u32 { 2 }

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            cooldown_ms: 30_000,
            half_open_retries: 1,
            success_threshold: 2,
        }
    }
}

impl Database {
    /// List all proxy providers.
    pub fn get_all_proxy_providers(&self) -> Result<Vec<ProxyProvider>, AppError> {
        let conn = lock_conn!(self.conn);
        let mut stmt = conn.prepare(
            "SELECT id, name, type, api_key, base_url, models, enabled,
                    timeout_ms, weight, circuit_breaker
             FROM proxy_providers ORDER BY weight DESC, id ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            let models_str: String = row.get(5)?;
            let cb_str: String = row.get(9)?;
            let models: Vec<String> = serde_json::from_str(&models_str).unwrap_or_default();
            let circuit_breaker: CircuitBreakerConfig =
                serde_json::from_str(&cb_str).unwrap_or_default();
            Ok(ProxyProvider {
                id: row.get(0)?,
                name: row.get(1)?,
                provider_type: row.get(2)?,
                api_key: row.get(3)?,
                base_url: row.get(4)?,
                models,
                enabled: row.get(6)?,
                timeout_ms: row.get::<_, u64>(7)?,
                weight: row.get::<_, u32>(8)?,
                circuit_breaker,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Upsert a proxy provider.
    pub fn save_proxy_provider(&self, p: &ProxyProvider) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        let models = serde_json::to_string(&p.models)?;
        let cb = serde_json::to_string(&p.circuit_breaker)?;
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            "INSERT INTO proxy_providers (
                id, name, type, api_key, base_url, models, enabled,
                timeout_ms, weight, circuit_breaker, created_at
            ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
            ON CONFLICT(id) DO UPDATE SET
                name=excluded.name, type=excluded.type, api_key=excluded.api_key,
                base_url=excluded.base_url, models=excluded.models, enabled=excluded.enabled,
                timeout_ms=excluded.timeout_ms, weight=excluded.weight,
                circuit_breaker=excluded.circuit_breaker",
            params![
                p.id, p.name, p.provider_type, p.api_key, p.base_url,
                models, p.enabled, p.timeout_ms, p.weight, cb, now,
            ],
        )?;
        Ok(())
    }

    /// Delete a proxy provider.
    #[allow(dead_code)]
    pub fn delete_proxy_provider(&self, id: &str) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute("DELETE FROM proxy_providers WHERE id = ?1", params![id])?;
        Ok(())
    }
}