//! Vendors DAO (vendors table).
//!
//! A vendor is a unified upstream API supplier: one name + one API key that
//! may serve multiple protocols (anthropic / openai-chat / openai-responses).
//! Per-protocol endpoint URLs can override `base_url`. Providers reference
//! vendors by id so switching an app provider to a different protocol of the
//! same vendor reuses the stored key.

use crate::database::{lock_conn, Database};
use crate::error::AppError;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Protocol ids a vendor can serve. Keep in sync with
/// `ui/src/lib/api/vendors.ts` (PROTOCOLS) and the app->protocol mapping in
/// the smart form.
#[allow(dead_code)] // live-config write path retained for tests/history; not called in the app since Phase 15.9
pub const PROTOCOL_ANTHROPIC: &str = "anthropic";
pub const PROTOCOL_OPENAI_CHAT: &str = "openai-chat";
pub const PROTOCOL_OPENAI_RESPONSES: &str = "openai-responses";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Vendor {
    pub id: String,
    pub name: String,
    #[serde(rename = "baseUrl", default)]
    pub base_url: String,
    #[serde(rename = "apiKey", default)]
    pub api_key: String,
    /// Enabled protocol ids (subset of the known protocol constants).
    #[serde(default)]
    pub protocols: Vec<String>,
    /// Per-protocol URL overrides; missing key falls back to base_url.
    #[serde(default)]
    pub endpoints: HashMap<String, String>,
    #[serde(rename = "createdAt", default)]
    pub created_at: Option<i64>,
}

impl Database {
    /// List all vendors ordered by name.
    pub fn get_all_vendors(&self) -> Result<Vec<Vendor>, AppError> {
        let conn = lock_conn!(self.conn);
        let mut stmt = conn.prepare(
            "SELECT id, name, base_url, api_key, protocols, endpoints, created_at
             FROM vendors ORDER BY name ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<i64>>(6)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, name, base_url, api_key, protocols_str, endpoints_str, created_at) = row?;
            out.push(Vendor {
                id,
                name,
                base_url,
                api_key,
                protocols: serde_json::from_str(&protocols_str)?,
                endpoints: serde_json::from_str(&endpoints_str)?,
                created_at,
            });
        }
        Ok(out)
    }

    /// Upsert a vendor.
    pub fn save_vendor(&self, v: &Vendor) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        let protocols_json = serde_json::to_string(&v.protocols)?;
        let endpoints_json = serde_json::to_string(&v.endpoints)?;
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            "INSERT INTO vendors (id, name, base_url, api_key, protocols, endpoints, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                base_url = excluded.base_url,
                api_key = excluded.api_key,
                protocols = excluded.protocols,
                endpoints = excluded.endpoints",
            params![
                v.id,
                v.name,
                v.base_url,
                v.api_key,
                protocols_json,
                endpoints_json,
                now
            ],
        )?;
        Ok(())
    }

    /// Delete a vendor by id. Providers referencing it keep their baked-in
    /// settings_config (no cascade — the vendor only feeds the form).
    pub fn delete_vendor(&self, id: &str) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute("DELETE FROM vendors WHERE id = ?1", params![id])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::Database;

    fn test_db() -> Database {
        Database::memory().expect("in-memory db")
    }

    #[test]
    fn vendor_roundtrip() {
        let db = test_db();
        let mut endpoints = HashMap::new();
        endpoints.insert(
            "anthropic".to_string(),
            "https://api.example.com/anthropic".to_string(),
        );
        let v = Vendor {
            id: "acme".into(),
            name: "Acme Relay".into(),
            base_url: "https://api.acme.dev".into(),
            api_key: "sk-acme".into(),
            protocols: vec!["anthropic".into(), "openai-chat".into()],
            endpoints,
            created_at: None,
        };
        db.save_vendor(&v).unwrap();

        let list = db.get_all_vendors().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "acme");
        assert_eq!(list[0].protocols.len(), 2);
        assert_eq!(
            list[0].endpoints.get("anthropic").map(|s| s.as_str()),
            Some("https://api.example.com/anthropic")
        );

        // Update path
        let mut v2 = v.clone();
        v2.api_key = "sk-acme-2".into();
        db.save_vendor(&v2).unwrap();
        let list = db.get_all_vendors().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].api_key, "sk-acme-2");

        db.delete_vendor("acme").unwrap();
        assert!(db.get_all_vendors().unwrap().is_empty());
    }

    #[test]
    fn malformed_vendor_json_is_reported_instead_of_defaulted() {
        let db = test_db();
        db.conn
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO vendors (id, name, base_url, api_key, protocols, endpoints)
                 VALUES ('broken', 'Broken', '', '', 'not-json', '{}')",
                [],
            )
            .unwrap();
        assert!(db.get_all_vendors().is_err());
    }
}
