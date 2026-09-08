//! MCP (Model Context Protocol) servers DAO (mcp_servers table).
//!
//! Mirrors cc-switch `src-tauri/src/database/dao/mcp.rs` + `database/schema.rs:64`.
//! Stores MCP server definitions (command + args + env) and per-app enabled
//! flags. The bidirectional sync layer (write to live Claude/Codex/etc. configs)
//! is a later step.

use crate::database::{lock_conn, Database};
use crate::error::AppError;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A registered MCP server. `server_config` is a JSON blob with the wire-format
/// shape: `{ "command": "...", "args": [...], "env": {...} }`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServer {
    pub id: String,
    pub name: String,
    pub server_config: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docs: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub enabled_claude: bool,
    #[serde(default)]
    pub enabled_codex: bool,
}

impl Database {
    /// List all MCP servers, alphabetical by id.
    #[allow(dead_code)]
    pub fn get_all_mcp_servers(&self) -> Result<Vec<McpServer>, AppError> {
        let conn = lock_conn!(self.conn);
        let mut stmt = conn.prepare(
            "SELECT id, name, server_config, description, homepage, docs, tags,
                    enabled_claude, enabled_codex
             FROM mcp_servers ORDER BY id ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            let cfg_str: String = row.get(2)?;
            let cfg: Value = serde_json::from_str(&cfg_str).unwrap_or(Value::Null);
            let tags_str: String = row.get(6)?;
            let tags: Vec<String> = serde_json::from_str(&tags_str).unwrap_or_default();
            Ok(McpServer {
                id: row.get(0)?,
                name: row.get(1)?,
                server_config: cfg,
                description: row.get(3)?,
                homepage: row.get(4)?,
                docs: row.get(5)?,
                tags,
                enabled_claude: row.get(7)?,
                enabled_codex: row.get(8)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Fetch a single MCP server by id.
    #[allow(dead_code)]
    pub fn get_mcp_server(&self, id: &str) -> Result<Option<McpServer>, AppError> {
        let conn = lock_conn!(self.conn);
        let result = conn.query_row(
            "SELECT id, name, server_config, description, homepage, docs, tags,
                    enabled_claude, enabled_codex
             FROM mcp_servers WHERE id = ?1",
            params![id],
            |row| {
                let cfg_str: String = row.get(2)?;
                let cfg: Value = serde_json::from_str(&cfg_str).unwrap_or(Value::Null);
                let tags_str: String = row.get(6)?;
                let tags: Vec<String> = serde_json::from_str(&tags_str).unwrap_or_default();
                Ok(McpServer {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    server_config: cfg,
                    description: row.get(3)?,
                    homepage: row.get(4)?,
                    docs: row.get(5)?,
                    tags,
                    enabled_claude: row.get(7)?,
                    enabled_codex: row.get(8)?,
                })
            },
        );
        match result {
            Ok(s) => Ok(Some(s)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(AppError::Database(e.to_string())),
        }
    }

    /// Upsert an MCP server.
    #[allow(dead_code)]
    pub fn save_mcp_server(&self, s: &McpServer) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        let cfg = serde_json::to_string(&s.server_config)?;
        let tags = serde_json::to_string(&s.tags)?;
        conn.execute(
            "INSERT INTO mcp_servers (
                id, name, server_config, description, homepage, docs, tags,
                enabled_claude, enabled_codex
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            ON CONFLICT(id) DO UPDATE SET
                name=excluded.name, server_config=excluded.server_config,
                description=excluded.description, homepage=excluded.homepage,
                docs=excluded.docs, tags=excluded.tags,
                enabled_claude=excluded.enabled_claude, enabled_codex=excluded.enabled_codex",
            params![
                s.id, s.name, cfg, s.description, s.homepage, s.docs, tags,
                s.enabled_claude, s.enabled_codex,
            ],
        )?;
        Ok(())
    }

    /// Delete an MCP server.
    #[allow(dead_code)]
    pub fn delete_mcp_server(&self, id: &str) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute("DELETE FROM mcp_servers WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Toggle an MCP server's per-app enabled flag.
    #[allow(dead_code)]
    pub fn toggle_mcp_app(&self, id: &str, app: &str, enabled: bool) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        let column = match app {
            "claude" | "claude_code" => "enabled_claude",
            "codex" => "enabled_codex",
            other => return Err(AppError::InvalidInput(format!("unknown app: {other}"))),
        };
        // Validate id exists, then update.
        let updated = conn.execute(
            &format!("UPDATE mcp_servers SET {column} = ?1 WHERE id = ?2"),
            params![enabled, id],
        )?;
        if updated == 0 {
            return Err(AppError::InvalidInput(format!(
                "mcp server \"{id}\" not found"
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_db() -> Database {
        Database::memory().expect("memory db")
    }

    #[test]
    fn save_get_delete_mcp_server() {
        let db = make_db();
        let s = McpServer {
            id: "fs-mcp".into(),
            name: "Filesystem MCP".into(),
            server_config: json!({"command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem"]}),
            description: Some("Access local files".into()),
            homepage: None,
            docs: None,
            tags: vec!["filesystem".into()],
            enabled_claude: true,
            enabled_codex: false,
        };
        db.save_mcp_server(&s).unwrap();

        let fetched = db.get_mcp_server("fs-mcp").unwrap();
        assert!(fetched.is_some());
        let fetched = fetched.unwrap();
        assert_eq!(fetched.name, "Filesystem MCP");
        assert!(fetched.enabled_claude);
        assert!(!fetched.enabled_codex);

        let all = db.get_all_mcp_servers().unwrap();
        assert_eq!(all.len(), 1);

        db.delete_mcp_server("fs-mcp").unwrap();
        assert!(db.get_mcp_server("fs-mcp").unwrap().is_none());
    }

    #[test]
    fn toggle_mcp_app_flag() {
        let db = make_db();
        let s = McpServer {
            id: "test".into(),
            name: "Test".into(),
            server_config: json!({"command": "echo"}),
            description: None,
            homepage: None,
            docs: None,
            tags: vec![],
            enabled_claude: false,
            enabled_codex: false,
        };
        db.save_mcp_server(&s).unwrap();
        db.toggle_mcp_app("test", "claude", true).unwrap();
        let fetched = db.get_mcp_server("test").unwrap().unwrap();
        assert!(fetched.enabled_claude);
        db.toggle_mcp_app("test", "codex", true).unwrap();
        let fetched = db.get_mcp_server("test").unwrap().unwrap();
        assert!(fetched.enabled_codex);
        // Unknown app name -> error
        assert!(db.toggle_mcp_app("test", "bogus", true).is_err());
        // Unknown id -> error
        assert!(db.toggle_mcp_app("nope", "claude", true).is_err());
    }
}