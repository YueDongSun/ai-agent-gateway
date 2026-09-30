//! Public, credential-free contracts for the managed DSH instance.
use serde::{Deserialize, Serialize};

#[allow(non_camel_case_types)]
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Dsh_settings {
    pub node_path: String,
    pub dsh_entry_path: String,
    pub pnpm_entry_path: String,
    pub port: u16,
    pub startup_timeout_secs: u64,
    pub operation_timeout_secs: u64,
    pub theme: String,
    pub upstream_id: Option<String>,
    pub model_id: Option<String>,
}

impl Default for Dsh_settings {
    fn default() -> Self {
        Self {
            node_path: String::new(),
            dsh_entry_path: String::new(),
            pnpm_entry_path: String::new(),
            port: 3141,
            startup_timeout_secs: 60,
            operation_timeout_secs: 180,
            theme: "system".into(),
            upstream_id: None,
            model_id: None,
        }
    }
}

#[allow(non_camel_case_types)]
#[derive(Clone, Debug, Serialize)]
pub struct Dsh_plugin {
    pub plugin_id: String,
    pub package_name: String,
    pub display_name: String,
    pub category: String,
    pub description: String,
    pub source_url: String,
    pub installed_version: Option<String>,
    pub enabled: bool,
    pub essential: bool,
    pub runtime_phase: String,
}

#[allow(non_camel_case_types)]
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Dsh_operation_request {
    pub action: String,
    pub package_name: Option<String>,
    pub version: Option<String>,
}

#[allow(non_camel_case_types)]
#[derive(Clone, Debug, Serialize)]
pub struct Dsh_operation_preview {
    pub preview_id: String,
    pub action: String,
    pub package_name: Option<String>,
    pub version: Option<String>,
    pub changes: Vec<String>,
    pub warnings: Vec<String>,
    pub expires_at_ms: u64,
}

#[allow(non_camel_case_types)]
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Dsh_receipt {
    pub operation_id: String,
    pub action: String,
    pub package_name: Option<String>,
    pub version: Option<String>,
    pub status: String,
    pub error_code: Option<String>,
    pub started_at_ms: u64,
    pub finished_at_ms: u64,
    pub rollback_status: String,
    pub restart_required: bool,
}

#[allow(non_camel_case_types)]
#[derive(Clone, Debug, Serialize)]
pub struct Dsh_job {
    pub operation_id: String,
    pub action: String,
    pub package_name: Option<String>,
    pub phase: String,
}

#[allow(non_camel_case_types)]
#[derive(Clone, Debug, Serialize)]
pub struct Dsh_runtime {
    pub phase: String,
    pub pid: Option<u32>,
    pub web_url: Option<String>,
    pub error_code: Option<String>,
}

impl Default for Dsh_runtime {
    fn default() -> Self {
        Self {
            phase: "stopped".into(),
            pid: None,
            web_url: None,
            error_code: None,
        }
    }
}

#[allow(non_camel_case_types)]
#[derive(Clone, Debug, Serialize)]
pub struct Dsh_snapshot {
    pub settings: Dsh_settings,
    pub settings_digest: String,
    pub initialized: bool,
    pub managed_home: String,
    pub plugins: Vec<Dsh_plugin>,
    pub runtime: Dsh_runtime,
    pub job: Option<Dsh_job>,
    pub receipts: Vec<Dsh_receipt>,
}
