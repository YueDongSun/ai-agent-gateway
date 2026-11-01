use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Aag_instance {
    pub device_name: String,
    pub address: String,
    pub port: u16,
    pub version: String,
    pub latency_ms: u64,
    pub is_self: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Discovery_counts {
    pub online: usize,
    pub auth_required: usize,
    pub offline: usize,
    pub unverified: usize,
    pub failed: usize,
    pub skipped: usize,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Discovery_report {
    pub request_id: String,
    pub port: u16,
    pub checked_at: i64,
    pub elapsed_ms: u64,
    pub peer_count: usize,
    pub checked_count: usize,
    pub limited: bool,
    pub counts: Discovery_counts,
    /// Only marker-verified AAG instances leave the service. Other peer names
    /// and addresses are reduced to aggregate status counts.
    pub instances: Vec<Aag_instance>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Discovery_status {
    pub port: u16,
    pub phase: String,
    pub error: Option<String>,
    pub last_report: Option<Discovery_report>,
}
