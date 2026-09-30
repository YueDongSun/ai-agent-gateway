//! IPC contracts contain redacted definitions only; private payloads stay in Rust.
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[allow(non_camel_case_types)]
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Mcp_client {
    Claude,
    Codex,
}

#[allow(non_camel_case_types)]
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mcp_server_input {
    pub id: String,
    pub name: String,
    /// Missing/null preserves an existing private definition; creation requires one.
    pub server_config: Option<Value>,
    pub description: Option<String>,
    #[serde(default)]
    pub create_only: bool,
    pub expected_revision: Option<String>,
    #[serde(default)]
    pub enabled_claude: bool,
    #[serde(default)]
    pub enabled_codex: bool,
}

#[allow(non_camel_case_types)]
#[derive(Clone, Serialize)]
pub struct Mcp_server_summary {
    pub id: String,
    pub revision: String,
    pub name: String,
    pub description: Option<String>,
    pub transport: String,
    pub enabled_claude: bool,
    pub enabled_codex: bool,
    pub redacted_config: Value,
    pub validation_error: Option<String>,
}

#[allow(non_camel_case_types)]
#[derive(Clone, Serialize)]
pub struct Mcp_sync_change {
    pub server_id: String,
    pub action: String,
}

#[allow(non_camel_case_types)]
#[derive(Clone, Serialize)]
pub struct Mcp_sync_preview {
    pub client: Mcp_client,
    pub format: String,
    pub redacted_config: String,
    pub server_ids: Vec<String>,
    pub warnings: Vec<String>,
    pub review_token: Option<String>,
    pub expires_at: Option<String>,
    pub changes: Vec<Mcp_sync_change>,
    pub can_apply: bool,
}

#[allow(non_camel_case_types)]
#[derive(Debug, Serialize)]
pub struct Mcp_sync_receipt {
    pub client: Mcp_client,
    pub applied_server_count: usize,
    pub backup_created: bool,
    pub restart_required: bool,
}
