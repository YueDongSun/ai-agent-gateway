//! Proxy config singleton DAO (proxy_config table, id=1).

use crate::database::{lock_conn, Database};
use crate::error::AppError;
use rusqlite::params;
use serde::{Deserialize, Serialize};

/// Proxy listen configuration (singleton row).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyConfig {
    pub host: String,
    pub port: u16,
    #[serde(rename = "authToken", default)]
    pub auth_token: String,
    pub enabled: bool,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 8787,
            auth_token: String::new(),
            enabled: false,
        }
    }
}

impl Database {
    /// Read the singleton proxy config (returns default if absent).
    pub fn get_proxy_config(&self) -> Result<ProxyConfig, AppError> {
        let conn = lock_conn!(self.conn);
        let result = conn.query_row(
            "SELECT host, port, auth_token, enabled FROM proxy_config WHERE id = 1",
            [],
            |row| {
                Ok(ProxyConfig {
                    host: row.get(0)?,
                    port: row.get::<_, u16>(1)?,
                    auth_token: row.get(2)?,
                    enabled: row.get(3)?,
                })
            },
        );
        match result {
            Ok(cfg) => Ok(cfg),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(ProxyConfig::default()),
            Err(e) => Err(AppError::Database(e.to_string())),
        }
    }

    /// Upsert the singleton proxy config.
    pub fn save_proxy_config(&self, cfg: &ProxyConfig) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "INSERT INTO proxy_config (id, host, port, auth_token, enabled)
             VALUES (1, ?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET
                host = excluded.host,
                port = excluded.port,
                auth_token = excluded.auth_token,
                enabled = excluded.enabled",
            params![cfg.host, cfg.port, cfg.auth_token, cfg.enabled],
        )?;
        Ok(())
    }
}