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

/// Default proxy listen port. Chosen to avoid the most common
/// development ports (3000/5000/8000/8080/8787/9000) so AAG plays
/// nicely alongside other dev tools. Users can change it via the
/// Proxy page (it persists in the `proxy_config` singleton row).
pub const DEFAULT_PROXY_PORT: u16 = 8989;

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: DEFAULT_PROXY_PORT,
            auth_token: String::new(),
            enabled: false,
        }
    }
}

impl ProxyConfig {
    /// The host a local client must use to reach this listen configuration.
    ///
    /// A wildcard bind is not a client target: `0.0.0.0`/`::` mean "accept
    /// on every interface", the `localhost` alias and an unset host both mean
    /// the local loopback. Any other host is a real endpoint the user chose
    /// and is returned unchanged.
    pub fn client_endpoint_host(&self) -> &str {
        match self.host.trim() {
            "" | "0.0.0.0" | "localhost" => "127.0.0.1",
            "::" | "::1" => "[::1]",
            host => host,
        }
    }

    /// Whether this configuration is reachable from this machine only.
    /// Wildcard binds count as loopback: the port is open on 127.0.0.1.
    pub fn is_loopback_bind(&self) -> bool {
        matches!(self.client_endpoint_host(), "127.0.0.1" | "[::1]")
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

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with_host(host: &str) -> ProxyConfig {
        ProxyConfig {
            host: host.into(),
            ..ProxyConfig::default()
        }
    }

    #[test]
    fn wildcard_and_alias_binds_resolve_to_a_dialable_loopback_endpoint() {
        for (host, expected) in [
            ("", "127.0.0.1"),
            ("0.0.0.0", "127.0.0.1"),
            ("localhost", "127.0.0.1"),
            ("::", "[::1]"),
            ("::1", "[::1]"),
        ] {
            let config = config_with_host(host);
            assert_eq!(config.client_endpoint_host(), expected, "{host}");
            assert!(config.is_loopback_bind(), "{host}");
        }
    }

    #[test]
    fn loopback_and_non_loopback_documentation_literals_are_left_intact() {
        assert_eq!(
            config_with_host("127.0.0.1").client_endpoint_host(),
            "127.0.0.1"
        );
        // RFC 5737 TEST-NET-1 is documentation-only, not a deployed endpoint.
        let documentation_host = config_with_host("192.0.2.5");
        assert_eq!(documentation_host.client_endpoint_host(), "192.0.2.5");
        assert!(
            !documentation_host.is_loopback_bind(),
            "an explicit non-loopback bind is not loopback"
        );
        // Surrounding whitespace is a configuration slip, not a host name.
        assert_eq!(
            config_with_host("  0.0.0.0  ").client_endpoint_host(),
            "127.0.0.1"
        );
    }
}
