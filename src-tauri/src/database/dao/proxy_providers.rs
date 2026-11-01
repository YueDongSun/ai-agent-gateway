//! Proxy providers DAO (proxy_providers table).

use crate::database::{lock_conn, Database};
use crate::error::AppError;
use rusqlite::params;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use std::collections::HashMap;

fn read_json_column<T: DeserializeOwned>(raw: &str, column: usize) -> rusqlite::Result<T> {
    serde_json::from_str(raw).map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "invalid persisted proxy provider JSON",
            )),
        )
    })
}

/// An upstream API provider used by the proxy for failover chains.
/// Separate from the `providers` table (which holds per-agent config snapshots).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Per-provider model mapping: display model -> upstream model.
    /// Example: `{ "claude-sonnet-5": "deepseek-v4-pro" }`.
    /// When a request arrives with a display model that's a key in this map,
    /// the proxy replaces it with the mapped upstream model before forwarding.
    /// Stored as a JSON object; default `{}` (no mapping).
    #[serde(rename = "modelMapping", default)]
    pub model_mapping: HashMap<String, String>,
    /// Per-provider extra request headers, merged into every upstream request
    /// after the trusted auth/content headers are set. Reserved headers
    /// (content-type, authorization, x-api-key, anthropic-version, host,
    /// content-length) can never be overridden here. Used for provider-mandated
    /// headers such as OpenCode's `x-opencode-session`. Stored as a JSON
    /// object; default `{}`.
    #[serde(rename = "extraHeaders", default)]
    pub extra_headers: HashMap<String, String>,
    /// Model auto-discovery opt-out. When true, the discovery service may
    /// append newly released same-family models and upgrade route hop /
    /// mapping targets pointing at superseded ids.
    #[serde(rename = "autoTrackModels", default = "default_true")]
    pub auto_track_models: bool,
    /// Optional display icon: a designed glyph id from the UI's provider
    /// icon set, or a literal character. Presentation-only — the proxy
    /// runtime ignores it and it stays out of the routing fingerprint so
    /// icon edits never hot-reload routing.
    #[serde(rename = "icon", default)]
    pub icon: Option<String>,
    /// Optional display tile color for `icon` (hex string).
    #[serde(rename = "iconColor", default)]
    pub icon_color: Option<String>,
}

fn default_true() -> bool {
    true
}
fn default_timeout() -> u64 {
    60_000
}
fn default_weight() -> u32 {
    1
}

/// Circuit breaker parameters for a proxy provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

fn default_failure_threshold() -> u32 {
    5
}
fn default_cooldown_ms() -> u64 {
    30_000
}
fn default_half_open_retries() -> u32 {
    1
}
fn default_success_threshold() -> u32 {
    2
}

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
    /// Discovery enumerates identities before decoding individual rows so a
    /// malformed supplier projection does not discard unrelated results.
    pub(crate) fn proxy_provider_ids_for_refresh(
        &self,
        only_provider: Option<&str>,
    ) -> Result<Vec<String>, AppError> {
        let conn = lock_conn!(self.conn);
        let mut statement = conn.prepare("SELECT id FROM proxy_providers WHERE (?1 IS NULL OR id=?1) ORDER BY weight DESC,id ASC")?;
        let rows = statement.query_map(params![only_provider], |row| row.get(0))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
    }

    /// List all proxy providers.
    pub fn get_all_proxy_providers(&self) -> Result<Vec<ProxyProvider>, AppError> {
        let conn = lock_conn!(self.conn);
        get_proxy_providers_on_conn(&conn, None)
    }

    /// Load one proxy provider by id (single-row query, not a full scan).
    pub fn get_proxy_provider(&self, id: &str) -> Result<Option<ProxyProvider>, AppError> {
        let conn = lock_conn!(self.conn);
        Ok(get_proxy_providers_on_conn(&conn, Some(id))?
            .into_iter()
            .next())
    }

    /// Upsert a proxy provider.
    pub fn save_proxy_provider(&self, p: &ProxyProvider) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        save_proxy_provider_on_conn(&conn, p)
    }

    /// Delete a proxy provider.
    ///
    /// Rows referenced by a supplier endpoint projection are refused: the
    /// supplier service owns (or adopted) that row, and a later
    /// `supplier_save` updates it through the projection mapping and fails
    /// when it is gone. Deleting the supplier/endpoint removes the
    /// projection row with it.
    pub fn delete_proxy_provider(&self, id: &str) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        let supplier_mapped: bool = conn.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM supplier_runtime_projections WHERE proxy_provider_id = ?1
             )",
            params![id],
            |row| row.get(0),
        )?;
        if supplier_mapped {
            return Err(AppError::Message("AAG_SUPPLIER_PROJECTION_IN_USE".into()));
        }
        conn.execute("DELETE FROM proxy_providers WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Proxy-provider ids referenced by a supplier endpoint projection.
    /// Presentation only: the providers page hides direct edit/delete for
    /// these rows because supplier saves own their contents.
    pub fn list_supplier_projection_ids(&self) -> Result<Vec<String>, AppError> {
        let conn = lock_conn!(self.conn);
        let mut stmt = conn.prepare(
            "SELECT proxy_provider_id FROM supplier_runtime_projections
             ORDER BY proxy_provider_id",
        )?;
        let ids = stmt
            .query_map([], |row| row.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        Ok(ids)
    }
}
/// Connection-level DAO used within an existing transaction.
pub(crate) fn get_proxy_providers_on_conn(
    conn: &rusqlite::Connection,
    provider_id: Option<&str>,
) -> Result<Vec<ProxyProvider>, AppError> {
    let mut stmt = conn.prepare(
        "SELECT id, name, type, api_key, base_url, models, enabled,
                timeout_ms, weight, circuit_breaker, model_mapping, extra_headers,
                auto_track_models, icon, icon_color
         FROM proxy_providers WHERE (?1 IS NULL OR id = ?1) ORDER BY weight DESC, id ASC",
    )?;
    let rows = stmt.query_map(params![provider_id], |row| {
        let models_str: String = row.get(5)?;
        let cb_str: String = row.get(9)?;
        let mm_str: String = row.get(10)?;
        let eh_str: String = row.get(11)?;
        let models: Vec<String> = read_json_column(&models_str, 5)?;
        let circuit_breaker: CircuitBreakerConfig = read_json_column(&cb_str, 9)?;
        let model_mapping: HashMap<String, String> = read_json_column(&mm_str, 10)?;
        let extra_headers: HashMap<String, String> = read_json_column(&eh_str, 11)?;
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
            model_mapping,
            extra_headers,
            auto_track_models: row.get::<_, Option<bool>>(12)?.unwrap_or(true),
            icon: row.get::<_, Option<String>>(13)?,
            icon_color: row.get::<_, Option<String>>(14)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Connection-level DAO used within an existing transaction.
pub(crate) fn save_proxy_provider_on_conn(
    conn: &rusqlite::Connection,
    p: &ProxyProvider,
) -> Result<(), AppError> {
    // Same contract as the Node compatibility schema (`src/config/schema.ts`
    // `timeoutMs`): integer milliseconds with a 1000 ms floor. A stored 0
    // would make every hop of the provider an instant retriable timeout.
    const MIN_PROVIDER_TIMEOUT_MS: u64 = 1_000;
    if p.timeout_ms < MIN_PROVIDER_TIMEOUT_MS {
        return Err(AppError::InvalidInput(
            "AAG_PROXY_PROVIDER_TIMEOUT_INVALID".into(),
        ));
    }
    let models = serde_json::to_string(&p.models)?;
    let cb = serde_json::to_string(&p.circuit_breaker)?;
    let mm = serde_json::to_string(&p.model_mapping)?;
    let eh = serde_json::to_string(&p.extra_headers)?;
    let now = chrono::Utc::now().timestamp();
    conn.execute(
        "INSERT INTO proxy_providers (
            id, name, type, api_key, base_url, models, enabled,
            timeout_ms, weight, circuit_breaker, model_mapping, extra_headers,
            auto_track_models, icon, icon_color, created_at
        ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)
        ON CONFLICT(id) DO UPDATE SET
            name=excluded.name, type=excluded.type, api_key=excluded.api_key,
            base_url=excluded.base_url, models=excluded.models, enabled=excluded.enabled,
            timeout_ms=excluded.timeout_ms, weight=excluded.weight,
            circuit_breaker=excluded.circuit_breaker, model_mapping=excluded.model_mapping,
            extra_headers=excluded.extra_headers,
            auto_track_models=excluded.auto_track_models,
            icon=excluded.icon, icon_color=excluded.icon_color",
        params![
            p.id,
            p.name,
            p.provider_type,
            p.api_key,
            p.base_url,
            models,
            p.enabled,
            p.timeout_ms,
            p.weight,
            cb,
            mm,
            eh,
            p.auto_track_models,
            p.icon,
            p.icon_color,
            now,
        ],
    )?;
    Ok(())
}

#[cfg(test)]
pub(crate) fn install_proxy_provider_read_failure(db: &Database) {
    let conn = db.conn.lock().expect("database connection lock");
    let updated = conn
        .execute(
            "UPDATE proxy_providers SET model_mapping = '{' WHERE id = 'fixture'",
            [],
        )
        .expect("install proxy provider read failure");
    assert_eq!(updated, 1, "expected exactly one fixture provider row");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(id: &str) -> ProxyProvider {
        ProxyProvider {
            id: id.into(),
            name: id.into(),
            provider_type: "anthropic".into(),
            api_key: String::new(),
            base_url: "https://upstream.example/v1".into(),
            models: vec!["minimax-m3".into()],
            enabled: true,
            timeout_ms: 60_000,
            weight: 1,
            circuit_breaker: Default::default(),
            model_mapping: Default::default(),
            extra_headers: Default::default(),
            auto_track_models: true,
            icon: None,
            icon_color: None,
        }
    }

    #[test]
    fn get_proxy_provider_reads_one_row_or_none() {
        let db = Database::memory().unwrap();
        assert!(db.get_proxy_provider("p1").unwrap().is_none());
        db.save_proxy_provider(&provider("p1")).unwrap();
        db.save_proxy_provider(&provider("p2")).unwrap();
        assert_eq!(
            db.get_proxy_provider("p1").unwrap().as_ref(),
            Some(&provider("p1"))
        );
        assert!(db.get_proxy_provider("missing").unwrap().is_none());
    }

    #[test]
    fn save_proxy_provider_rejects_timeout_below_the_contract_minimum() {
        let db = Database::memory().unwrap();
        for below_minimum in [0u64, 999] {
            let mut invalid = provider("low");
            invalid.timeout_ms = below_minimum;
            let error = db.save_proxy_provider(&invalid).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("AAG_PROXY_PROVIDER_TIMEOUT_INVALID"),
                "{below_minimum}: {error}"
            );
            assert!(db.get_proxy_provider("low").unwrap().is_none());
        }

        let mut valid = provider("ok");
        valid.timeout_ms = 1_000;
        db.save_proxy_provider(&valid).unwrap();
        let stored = db.get_proxy_provider("ok").unwrap().expect("stored row");
        assert_eq!(stored.timeout_ms, 1_000);
    }

    fn set_raw_proxy_provider_json(db: &Database, column: &str, raw: &str) {
        let conn = db.conn.lock().unwrap();
        let updated = conn
            .execute(
                &format!("UPDATE proxy_providers SET {column} = ?1 WHERE id = ?2"),
                params![raw, "fixture"],
            )
            .unwrap();
        assert_eq!(updated, 1);
    }

    fn stored_proxy_provider_json(db: &Database, column: &str) -> String {
        let conn = db.conn.lock().unwrap();
        conn.query_row(
            &format!("SELECT {column} FROM proxy_providers WHERE id = ?1"),
            params!["fixture"],
            |row| row.get(0),
        )
        .unwrap()
    }

    fn assert_proxy_provider_read_rejected(
        db: &Database,
        column: &str,
        raw: &str,
        reader: &str,
        error: AppError,
    ) {
        let internal_error = error.to_string();
        let external_error = error.external_message();
        assert_eq!(external_error, "database_error", "{reader} / {column}");
        assert!(
            internal_error.contains("invalid persisted proxy provider JSON"),
            "{reader} / {column}: {internal_error}"
        );
        for canary in [
            "PROVIDER_SECRET_CANARY",
            "PROVIDER_HEADER_CANARY",
            "PROVIDER_MODEL_CANARY",
        ] {
            assert!(
                !internal_error.contains(canary),
                "{reader} / {column} leaked {canary}"
            );
            assert!(
                !external_error.contains(canary),
                "{reader} / {column} leaked {canary}"
            );
        }
        assert_eq!(
            stored_proxy_provider_json(db, column),
            raw,
            "{reader} / {column}"
        );
    }

    #[test]
    fn proxy_provider_reads_reject_invalid_json_without_leaking_data_or_mutating_rows() {
        let invalid_values = [
            r#"{"secret":"PROVIDER_SECRET_CANARY","header":"PROVIDER_HEADER_CANARY","model":"PROVIDER_MODEL_CANARY""#,
            r#""PROVIDER_SECRET_CANARY_PROVIDER_HEADER_CANARY_PROVIDER_MODEL_CANARY""#,
        ];
        for column in ["models", "circuit_breaker", "model_mapping", "extra_headers"] {
            for raw in invalid_values {
                let db = Database::memory().unwrap();
                db.save_proxy_provider(&provider("fixture")).unwrap();
                set_raw_proxy_provider_json(&db, column, raw);

                let single_error = db.get_proxy_provider("fixture").unwrap_err();
                assert_proxy_provider_read_rejected(
                    &db,
                    column,
                    raw,
                    "get_proxy_provider",
                    single_error,
                );
                let list_error = db.get_all_proxy_providers().unwrap_err();
                assert_proxy_provider_read_rejected(
                    &db,
                    column,
                    raw,
                    "get_all_proxy_providers",
                    list_error,
                );
            }
        }
    }

    #[test]
    fn proxy_provider_reads_preserve_legacy_defaults_and_configured_json() {
        let db = Database::memory().unwrap();
        let mut primary = provider("primary");
        primary.weight = 7;
        primary.models = vec!["display_one".into(), "display_two".into()];
        primary.circuit_breaker = CircuitBreakerConfig {
            failure_threshold: 9,
            cooldown_ms: 12_345,
            half_open_retries: 3,
            success_threshold: 4,
        };
        primary.model_mapping = HashMap::from([
            ("display_one".into(), "upstream_one".into()),
            ("display_two".into(), "upstream_two".into()),
        ]);
        primary.extra_headers = HashMap::from([
            ("x-opencode-session".into(), "session_fixture".into()),
            ("x-provider-mode".into(), "compat".into()),
        ]);
        primary.auto_track_models = false;
        db.save_proxy_provider(&primary).unwrap();

        let mut secondary = provider("secondary");
        secondary.weight = 4;
        db.save_proxy_provider(&secondary).unwrap();
        {
            let conn = db.conn.lock().unwrap();
            conn.execute(
                "INSERT INTO proxy_providers (id, name, type, weight) VALUES (?1, ?2, ?3, ?4)",
                params!["legacy_defaults", "Legacy defaults", "anthropic", 1],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO proxy_providers (id, name, type, weight, circuit_breaker)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    "legacy_partial",
                    "Legacy partial breaker",
                    "anthropic",
                    1,
                    r#"{"failureThreshold":11}"#,
                ],
            )
            .unwrap();
        }

        let all = db.get_all_proxy_providers().unwrap();
        assert_eq!(
            all.iter().map(|item| item.id.as_str()).collect::<Vec<_>>(),
            vec!["primary", "secondary", "legacy_defaults", "legacy_partial"]
        );
        assert_eq!(all[0], primary);
        assert_eq!(all[1], secondary);

        let legacy_defaults = db
            .get_proxy_provider("legacy_defaults")
            .unwrap()
            .expect("legacy provider");
        assert!(legacy_defaults.models.is_empty());
        assert_eq!(
            legacy_defaults.circuit_breaker,
            CircuitBreakerConfig::default()
        );
        assert!(legacy_defaults.model_mapping.is_empty());
        assert!(legacy_defaults.extra_headers.is_empty());
        assert!(legacy_defaults.auto_track_models);

        let legacy_partial = db
            .get_proxy_provider("legacy_partial")
            .unwrap()
            .expect("partially configured legacy provider");
        assert_eq!(
            legacy_partial.circuit_breaker,
            CircuitBreakerConfig {
                failure_threshold: 11,
                ..CircuitBreakerConfig::default()
            }
        );
    }

    #[test]
    fn display_icon_fields_round_trip_and_update() {
        let db = Database::memory().unwrap();
        let mut p = provider("p1");
        p.icon = Some("moon".into());
        p.icon_color = Some("#111827".into());
        db.save_proxy_provider(&p).unwrap();
        let loaded = db.get_proxy_provider("p1").unwrap().unwrap();
        assert_eq!(loaded.icon.as_deref(), Some("moon"));
        assert_eq!(loaded.icon_color.as_deref(), Some("#111827"));

        // An update without icons must overwrite them (the form is the
        // source of truth), not leave stale values behind.
        db.save_proxy_provider(&provider("p1")).unwrap();
        let cleared = db.get_proxy_provider("p1").unwrap().unwrap();
        assert_eq!(cleared.icon, None);
        assert_eq!(cleared.icon_color, None);
    }

    #[test]
    fn display_icon_fields_are_optional_on_the_wire() {
        // Pre-v22 payloads (IPC and backups) carry no icon keys; they must
        // still deserialize with None rather than erroring.
        let json = r#"{
            "id": "p1", "name": "p1", "type": "anthropic",
            "apiKey": "", "baseURL": "https://upstream.example/v1",
            "models": ["m"], "enabled": true, "timeoutMs": 60000, "weight": 1
        }"#;
        let p: ProxyProvider = serde_json::from_str(json).unwrap();
        assert_eq!(p.icon, None);
        assert_eq!(p.icon_color, None);
        // And present keys round-trip through serde with their wire names.
        let with_icon = ProxyProvider {
            icon: Some("hub".into()),
            icon_color: Some("#5b5bd6".into()),
            ..provider("p2")
        };
        let value = serde_json::to_value(&with_icon).unwrap();
        assert_eq!(value["icon"], "hub");
        assert_eq!(value["iconColor"], "#5b5bd6");
    }

    #[test]
    fn supplier_projection_rows_are_refused_on_direct_delete() {
        let db = Database::memory().unwrap();
        crate::services::suppliers::save_supplier(
            &db,
            crate::services::suppliers::SupplierSaveInput {
                id: Some("supplier_demo".into()),
                expected_generation: 0,
                name: "Demo Supplier".into(),
                base_url: Some("https://api.example.invalid/v1".into()),
                api_key: Some("sk-test".into()),
                enabled: true,
                priority: 100,
                source: None,
                provenance: None,
                icon: None,
                icon_color: None,
                endpoints: Some(vec![crate::services::suppliers::SupplierEndpointInput {
                    protocol: "openai-chat".into(),
                    endpoint_url: None,
                    api_key_override: None,
                    use_shared_api_key: false,
                    enabled: Some(true),
                    priority: Some(100),
                    extra_headers: None,
                    source: None,
                    provenance: None,
                    models: None,
                }]),
            },
        )
        .expect("save supplier");

        let mapped = db.list_supplier_projection_ids().expect("projection ids");
        assert_eq!(mapped.len(), 1, "one endpoint projects one provider row");
        let err = db
            .delete_proxy_provider(&mapped[0])
            .expect_err("supplier-mapped rows must be refused");
        assert!(
            err.to_string().contains("AAG_SUPPLIER_PROJECTION_IN_USE"),
            "stable refusal code, got: {err}"
        );
        assert!(
            db.get_proxy_provider(&mapped[0]).unwrap().is_some(),
            "the refused delete must leave the row in place"
        );

        // Unmapped rows still delete normally.
        db.save_proxy_provider(&provider("pool-only")).unwrap();
        db.delete_proxy_provider("pool-only").expect("plain delete");
        assert!(db.get_proxy_provider("pool-only").unwrap().is_none());
    }
}
