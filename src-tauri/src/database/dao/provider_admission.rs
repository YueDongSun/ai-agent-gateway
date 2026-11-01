//! Private, provider-level screening receipts. One CAS record owns all offered
//! model pairs; only `probe_model` was actually probed. No raw configuration is
//! serialized. Callers must supply current authoritative source evidence and
//! current configuration on every lookup, including provisional import targets.
use super::proxy_providers::ProxyProvider;
use crate::{
    database::{lock_conn, Database},
    error::AppError,
};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub const ADMISSION_PREFIX: &str = "provider_admission:";
pub const SUBSCRIPTION_PREFIX: &str = "provider_subscription:";
pub const ENFORCEMENT_KEY: &str = "provider_admission:enforced";
const RECORD_SCHEMA: u32 = 1;
const MAX_RECORD_BYTES: usize = 1024 * 1024;
const MAX_MODELS: usize = 1000;
const MAX_PROVIDER_RECORDS: usize = 2000;
const MAX_IDENTIFIER_CHARS: usize = 256;
const MAX_RESPONSE_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Admission_state {
    Active,
    Inactive,
    Unknown,
    NotRequired,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Admission_evidence {
    /// Opaque authoritative revision, including its source identity.
    pub revision: String,
    pub state: Admission_state,
    pub valid_until: Option<i64>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Admission_probe {
    pub probe_model: String,
    pub checked_at: i64,
    pub eligible: bool,
    pub reason: String,
    pub latency_ms: Option<u64>,
    /// Private bounded/redacted probe text; never diagnostics or general settings IPC.
    pub response: Option<String>,
    pub response_truncated: bool,
    pub response_redacted: bool,
    pub upstream_status: Option<u16>,
    pub upstream_code: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Provider_admission_record {
    pub schema_version: u32,
    pub version: u64,
    pub provider_id: String,
    pub configuration_identity: String,
    pub evidence: Admission_evidence,
    pub policy_revision: String,
    pub begun_at: i64,
    pub probe: Option<Admission_probe>,
    /// Explicit advertised inventory, not a claim that every model was tested.
    pub offered_models: Vec<String>,
}

fn invalid(code: &str) -> AppError {
    AppError::InvalidInput(code.into())
}
fn valid_ref(value: &str) -> bool {
    !value.trim().is_empty()
        && value.chars().count() <= MAX_IDENTIFIER_CHARS
        && !value.chars().any(char::is_control)
}
pub fn is_private_admission_setting(key: &str) -> bool {
    key.starts_with(ADMISSION_PREFIX) || key.starts_with(SUBSCRIPTION_PREFIX)
}
fn record_key(provider_id: &str) -> Result<String, AppError> {
    if !valid_ref(provider_id) {
        return Err(invalid("provider_admission_invalid_provider"));
    }
    // A provider named 'enforced' must not collide with the policy flag.
    Ok(format!("{ADMISSION_PREFIX}record:{provider_id}"))
}

fn subscription_key(provider_id: &str) -> Result<String, AppError> {
    record_key(provider_id)?;
    Ok(format!("{SUBSCRIPTION_PREFIX}record:{provider_id}"))
}

fn validate_subscription_value(value: &str) -> Result<(), AppError> {
    if value.len() > MAX_RECORD_BYTES
        || !serde_json::from_str::<serde_json::Value>(value).is_ok_and(|parsed| parsed.is_object())
    {
        return Err(invalid("provider_subscription_invalid_record"));
    }
    Ok(())
}

/// Exact transport/authentication binding. Presentation and offered inventory
/// do not change transport identity; new models still need explicit inventory
/// membership in the accepted record. Digest is consumed only by this contract.
pub fn provider_configuration_identity(provider: &ProxyProvider) -> String {
    let headers: std::collections::BTreeMap<_, _> = provider.extra_headers.iter().collect();
    let mapping: std::collections::BTreeMap<_, _> = provider.model_mapping.iter().collect();
    let fields = serde_json::json!({"id":provider.id,"type":provider.provider_type,
        "baseUrl":provider.base_url,"apiKey":provider.api_key,"headers":headers,
        "mapping":mapping,"timeoutMs":provider.timeout_ms});
    let mut digest = Sha256::new();
    digest.update(b"aag-provider-admission-v1\0");
    digest.update(fields.to_string().as_bytes());
    hex::encode(digest.finalize())
}

fn validate_context(
    provider: &ProxyProvider,
    evidence: &Admission_evidence,
    policy: &str,
) -> Result<(), AppError> {
    record_key(&provider.id)?;
    if !valid_ref(&evidence.revision)
        || !valid_ref(policy)
        || evidence.valid_until.is_some_and(|time| time <= 0)
    {
        return Err(invalid("provider_admission_invalid_evidence"));
    }
    let refs = [&provider.id, &evidence.revision];
    for secret in std::iter::once(provider.api_key.as_str())
        .chain(provider.extra_headers.values().map(String::as_str))
        .filter(|value| !value.is_empty())
    {
        if refs.iter().any(|value| value.contains(secret)) || policy.contains(secret) {
            return Err(invalid("provider_admission_secret_reference"));
        }
    }
    Ok(())
}

fn read_record(
    conn: &Connection,
    provider_id: &str,
) -> Result<Option<Provider_admission_record>, AppError> {
    let key = record_key(provider_id)?;
    let raw: Option<String> = conn
        .query_row("SELECT value FROM app_meta WHERE key=?1", [&key], |row| {
            row.get(0)
        })
        .optional()?;
    let Some(raw) = raw else {
        return Ok(None);
    };
    if raw.len() > MAX_RECORD_BYTES {
        return Err(invalid("provider_admission_invalid_record"));
    }
    let record: Provider_admission_record =
        serde_json::from_str(&raw).map_err(|_| invalid("provider_admission_invalid_record"))?;
    if record.schema_version != RECORD_SCHEMA
        || record.provider_id != provider_id
        || record.version == 0
        || record.configuration_identity.len() != 64
        || !record
            .configuration_identity
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || !valid_ref(&record.evidence.revision)
        || !valid_ref(&record.policy_revision)
        || record.begun_at <= 0
        || record.evidence.valid_until.is_some_and(|time| time <= 0)
        || record.offered_models.len() > MAX_MODELS
        || record.offered_models.iter().any(|model| !valid_ref(model))
        || record.probe.as_ref().is_some_and(|probe| {
            probe.checked_at < record.begun_at
                || (probe.eligible
                    && (probe.reason != "accepted"
                        || probe.latency_ms.is_none()
                        || !valid_ref(&probe.probe_model)
                        || !record.offered_models.contains(&probe.probe_model)))
        })
    {
        return Err(invalid("provider_admission_invalid_record"));
    }
    Ok(Some(record))
}

fn write_record(conn: &Connection, record: &Provider_admission_record) -> Result<(), AppError> {
    let raw =
        serde_json::to_string(record).map_err(|_| invalid("provider_admission_invalid_record"))?;
    if raw.len() > MAX_RECORD_BYTES {
        return Err(invalid("provider_admission_record_too_large"));
    }
    conn.execute("INSERT INTO app_meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![record_key(&record.provider_id)?, raw])?;
    Ok(())
}

fn next_version(version: u64) -> Result<u64, AppError> {
    version
        .checked_add(1)
        .ok_or_else(|| invalid("provider_admission_version_exhausted"))
}

fn evidence_valid(evidence: &Admission_evidence, now: i64) -> bool {
    matches!(
        evidence.state,
        Admission_state::Active | Admission_state::NotRequired
    ) && evidence.valid_until.is_none_or(|deadline| now < deadline)
}

impl Database {
    /// Source-owner cache only. Raw private JSON never crosses generic settings IPC.
    pub fn provider_subscription_value(
        &self,
        provider_id: &str,
    ) -> Result<Option<String>, AppError> {
        let value = self.get_setting(&subscription_key(provider_id)?)?;
        if let Some(value) = &value {
            validate_subscription_value(value)?;
        }
        Ok(value)
    }

    /// Compare the exact pre-I/O value under a transaction. A late active
    /// response cannot overwrite a newer revocation written by another owner.
    pub fn compare_exchange_provider_subscription(
        &self,
        provider_id: &str,
        expected: Option<&str>,
        value: &str,
    ) -> Result<bool, AppError> {
        let key = subscription_key(provider_id)?;
        validate_subscription_value(value)?;
        let mut conn = lock_conn!(self.conn);
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let prior: Option<String> = transaction
            .query_row("SELECT value FROM app_meta WHERE key=?1", [&key], |row| {
                row.get(0)
            })
            .optional()?;
        if prior.as_deref() != expected {
            return Ok(false);
        }
        transaction.execute("INSERT INTO app_meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![key, value])?;
        transaction.commit()?;
        Ok(true)
    }

    pub fn provider_admission_enforced(&self) -> Result<bool, AppError> {
        match self.get_setting(ENFORCEMENT_KEY)?.as_deref() {
            None | Some("0") => Ok(false),
            Some("1") => Ok(true),
            _ => Err(invalid("provider_admission_invalid_enforcement")),
        }
    }

    pub fn set_provider_admission_enforced(&self, enabled: bool) -> Result<(), AppError> {
        self.set_setting(ENFORCEMENT_KEY, if enabled { "1" } else { "0" })
    }

    /// Private owner API. Generic settings IPC must never expose this record.
    pub fn provider_admission_record(
        &self,
        provider_id: &str,
    ) -> Result<Option<Provider_admission_record>, AppError> {
        let connection = lock_conn!(self.conn);
        read_record(&connection, provider_id)
    }

    /// Private review inventory also includes provisional excluded sources that
    /// were never registered as live providers. Generic settings IPC is blocked.
    pub fn list_provider_admission_records(
        &self,
    ) -> Result<Vec<Provider_admission_record>, AppError> {
        let conn = lock_conn!(self.conn);
        let transaction = conn.unchecked_transaction()?;
        let keys = {
            let mut statement = transaction.prepare("SELECT key FROM app_meta WHERE key GLOB 'provider_admission:record:*' ORDER BY key LIMIT ?1")?;
            let rows = statement.query_map([(MAX_PROVIDER_RECORDS + 1) as i64], |row| {
                row.get::<_, String>(0)
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        if keys.len() > MAX_PROVIDER_RECORDS {
            return Err(invalid("provider_admission_record_limit"));
        }
        let mut records = Vec::with_capacity(keys.len());
        for key in keys {
            let id = key
                .strip_prefix("provider_admission:record:")
                .ok_or_else(|| invalid("provider_admission_invalid_record"))?;
            records.push(
                read_record(&transaction, id)?
                    .ok_or_else(|| invalid("provider_admission_invalid_record"))?,
            );
        }
        transaction.commit()?;
        Ok(records)
    }

    /// CAS starts a new screening revision and clears every formerly admitted
    /// pair immediately. Inactive/unknown evidence uses this same invalidation.
    pub fn provider_admission_begin(
        &self,
        provider: &ProxyProvider,
        evidence: &Admission_evidence,
        policy_revision: &str,
        expected_version: u64,
    ) -> Result<u64, AppError> {
        validate_context(provider, evidence, policy_revision)?;
        let mut conn = lock_conn!(self.conn);
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let prior = read_record(&transaction, &provider.id)?;
        if prior.as_ref().map(|record| record.version).unwrap_or(0) != expected_version {
            return Err(invalid("provider_admission_conflict"));
        }
        let version = next_version(expected_version)?;
        write_record(
            &transaction,
            &Provider_admission_record {
                schema_version: RECORD_SCHEMA,
                version,
                provider_id: provider.id.clone(),
                configuration_identity: provider_configuration_identity(provider),
                evidence: evidence.clone(),
                policy_revision: policy_revision.into(),
                begun_at: chrono::Utc::now().timestamp(),
                probe: None,
                offered_models: Vec::new(),
            },
        )?;
        transaction.commit()?;
        Ok(version)
    }

    /// Finalize only the same admitted candidate/evidence/policy revision.
    /// Parent must refresh the source before calling; no network occurs here.
    pub fn provider_admission_finish(
        &self,
        provider: &ProxyProvider,
        evidence: &Admission_evidence,
        policy_revision: &str,
        expected_version: u64,
        mut probe: Admission_probe,
        offered_models: Vec<String>,
    ) -> Result<u64, AppError> {
        validate_context(provider, evidence, policy_revision)?;
        let now = chrono::Utc::now().timestamp();
        if (!(probe.probe_model.is_empty() && !probe.eligible) && !valid_ref(&probe.probe_model))
            || !valid_ref(&probe.reason)
            || !probe
                .reason
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
            || probe.checked_at <= 0
            || probe.checked_at > now
            || offered_models.len() > MAX_MODELS
            || offered_models.iter().any(|model| !valid_ref(model))
            || probe.upstream_code.as_ref().is_some_and(|code| {
                code.len() > 80
                    || !code
                        .bytes()
                        .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
            })
        {
            return Err(invalid("provider_admission_invalid_probe"));
        }
        if probe.eligible
            && (probe.reason != "accepted"
                || probe.latency_ms.is_none()
                || !provider.enabled
                || !evidence_valid(evidence, now)
                || !offered_models.contains(&probe.probe_model))
        {
            return Err(invalid("provider_admission_invalid_pass"));
        }
        for secret in std::iter::once(provider.api_key.as_str())
            .chain(provider.extra_headers.values().map(String::as_str))
            .filter(|value| !value.is_empty())
        {
            if probe.probe_model.contains(secret)
                || probe.reason.contains(secret)
                || offered_models.iter().any(|model| model.contains(secret))
                || probe
                    .upstream_code
                    .as_ref()
                    .is_some_and(|code| code.contains(secret))
            {
                return Err(invalid("provider_admission_secret_reference"));
            }
        }
        if let Some(response) = &probe.response {
            let capture = crate::proxy::providers::http_client::ProbeResponseCapture::default();
            capture.record(
                response.as_bytes(),
                Some(MAX_RESPONSE_BYTES),
                &provider.api_key,
                &provider.extra_headers,
                probe.response_truncated,
            );
            let (truncated, redacted) = capture.flags();
            probe.response = capture.take();
            probe.response_truncated |= truncated;
            probe.response_redacted |= redacted;
        }
        let mut conn = lock_conn!(self.conn);
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut record = read_record(&transaction, &provider.id)?
            .ok_or_else(|| invalid("provider_admission_conflict"))?;
        if record.version != expected_version
            || record.probe.is_some()
            || record.configuration_identity != provider_configuration_identity(provider)
            || record.evidence != *evidence
            || record.policy_revision != policy_revision
            || probe.checked_at < record.begun_at
        {
            return Err(invalid("provider_admission_conflict"));
        }
        record.version = next_version(record.version)?;
        record.probe = Some(probe);
        record.offered_models = offered_models
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        write_record(&transaction, &record)?;
        transaction.commit()?;
        Ok(record.version)
    }

    /// Refresh positive authoritative status without inventing a new inference
    /// observation. An inactive/unknown prior record must be screened again.
    pub fn provider_admission_refresh_evidence(
        &self,
        provider: &ProxyProvider,
        old_evidence: &Admission_evidence,
        new_evidence: &Admission_evidence,
        policy_revision: &str,
        expected_version: u64,
    ) -> Result<u64, AppError> {
        validate_context(provider, old_evidence, policy_revision)?;
        validate_context(provider, new_evidence, policy_revision)?;
        if !provider.enabled
            || !matches!(
                old_evidence.state,
                Admission_state::Active | Admission_state::NotRequired
            )
            || !evidence_valid(new_evidence, chrono::Utc::now().timestamp())
        {
            return Err(invalid("provider_admission_invalid_refresh"));
        }
        let mut conn = lock_conn!(self.conn);
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut record = read_record(&transaction, &provider.id)?
            .ok_or_else(|| invalid("provider_admission_conflict"))?;
        if record.version != expected_version
            || record.configuration_identity != provider_configuration_identity(provider)
            || record.evidence != *old_evidence
            || record.policy_revision != policy_revision
            || !record
                .probe
                .as_ref()
                .is_some_and(|probe| probe.eligible && probe.reason == "accepted")
        {
            return Err(invalid("provider_admission_conflict"));
        }
        record.version = next_version(record.version)?;
        record.evidence = new_evidence.clone();
        write_record(&transaction, &record)?;
        transaction.commit()?;
        Ok(record.version)
    }

    pub fn accepted_provider_models(
        &self,
        provider: &ProxyProvider,
        current_evidence: &Admission_evidence,
        policy_revision: &str,
        now: i64,
    ) -> Result<Vec<String>, AppError> {
        validate_context(provider, current_evidence, policy_revision)?;
        if !provider.enabled || !evidence_valid(current_evidence, now) {
            return Ok(Vec::new());
        }
        let record = self.provider_admission_record(&provider.id)?;
        let Some(record) = record else {
            return Ok(Vec::new());
        };
        if record.configuration_identity != provider_configuration_identity(provider)
            || record.evidence != *current_evidence
            || record.policy_revision != policy_revision
            || !record.probe.as_ref().is_some_and(|probe| {
                probe.eligible && probe.reason == "accepted" && probe.checked_at <= now
            })
        {
            return Ok(Vec::new());
        }
        Ok(record.offered_models)
    }

    /// Replace the explicit offered inventory after an authorized catalog read,
    /// preserving the provider-level probe and its actual timestamp. This does
    /// not assert that newly offered models were individually probed.
    pub fn provider_admission_extend_models(
        &self,
        provider: &ProxyProvider,
        evidence: &Admission_evidence,
        policy_revision: &str,
        expected_version: u64,
        models: Vec<String>,
    ) -> Result<u64, AppError> {
        validate_context(provider, evidence, policy_revision)?;
        if !provider.enabled
            || !evidence_valid(evidence, chrono::Utc::now().timestamp())
            || models.is_empty()
            || models.len() > MAX_MODELS
            || models.iter().any(|model| !valid_ref(model))
        {
            return Err(invalid("provider_admission_invalid_inventory"));
        }
        for secret in std::iter::once(provider.api_key.as_str())
            .chain(provider.extra_headers.values().map(String::as_str))
            .filter(|value| !value.is_empty())
        {
            if models.iter().any(|model| model.contains(secret)) {
                return Err(invalid("provider_admission_secret_reference"));
            }
        }
        let mut conn = lock_conn!(self.conn);
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut record = read_record(&transaction, &provider.id)?
            .ok_or_else(|| invalid("provider_admission_conflict"))?;
        if record.version != expected_version
            || record.configuration_identity != provider_configuration_identity(provider)
            || record.evidence != *evidence
            || record.policy_revision != policy_revision
            || !record.probe.as_ref().is_some_and(|probe| {
                probe.eligible && probe.reason == "accepted" && models.contains(&probe.probe_model)
            })
        {
            return Err(invalid("provider_admission_conflict"));
        }
        record.version = next_version(record.version)?;
        record.offered_models = models
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        write_record(&transaction, &record)?;
        transaction.commit()?;
        Ok(record.version)
    }

    pub fn provider_model_admitted(
        &self,
        provider: &ProxyProvider,
        current_evidence: &Admission_evidence,
        policy_revision: &str,
        model: &str,
        now: i64,
    ) -> Result<bool, AppError> {
        Ok(self
            .accepted_provider_models(provider, current_evidence, policy_revision, now)?
            .iter()
            .any(|id| id == model))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider() -> ProxyProvider {
        serde_json::from_value(serde_json::json!({"id":"admission_fixture","name":"Fixture","type":"openai",
            "baseURL":"https://fixture.invalid/v1","apiKey":"fixture/key","models":["tested","offered"],"enabled":true})).unwrap()
    }
    fn evidence() -> Admission_evidence {
        Admission_evidence {
            revision: "source_revision_1".into(),
            state: Admission_state::Active,
            valid_until: Some(chrono::Utc::now().timestamp() + 60),
        }
    }
    fn probe(eligible: bool) -> Admission_probe {
        Admission_probe {
            probe_model: "tested".into(),
            checked_at: chrono::Utc::now().timestamp(),
            eligible,
            reason: if eligible {
                "accepted".into()
            } else {
                "authentication".into()
            },
            latency_ms: Some(100),
            response: Some(r#"{"error":"fixture\/key"}"#.into()),
            response_truncated: false,
            response_redacted: false,
            upstream_status: None,
            upstream_code: None,
        }
    }
    fn pass(db: &Database, provider: &ProxyProvider, evidence: &Admission_evidence) -> u64 {
        let version = db
            .provider_admission_begin(provider, evidence, "policy_1", 0)
            .unwrap();
        db.provider_admission_finish(
            provider,
            evidence,
            "policy_1",
            version,
            probe(true),
            vec!["tested".into(), "offered".into(), "offered".into()],
        )
        .unwrap()
    }

    #[test]
    fn provider_pass_admits_explicit_inventory_without_claiming_per_model_probes() {
        let db = Database::memory().unwrap();
        let provider = provider();
        let evidence = evidence();
        assert!(!db.provider_admission_enforced().unwrap());
        assert!(db
            .accepted_provider_models(
                &provider,
                &evidence,
                "policy_1",
                chrono::Utc::now().timestamp()
            )
            .unwrap()
            .is_empty());
        let before = db.routing_revision().unwrap();
        pass(&db, &provider, &evidence);
        assert_ne!(before, db.routing_revision().unwrap());
        let record = db.provider_admission_record(&provider.id).unwrap().unwrap();
        assert_eq!(record.probe.as_ref().unwrap().probe_model, "tested");
        assert_eq!(
            db.accepted_provider_models(
                &provider,
                &evidence,
                "policy_1",
                chrono::Utc::now().timestamp()
            )
            .unwrap(),
            vec!["offered", "tested"]
        );
        assert!(!db
            .provider_model_admitted(
                &provider,
                &evidence,
                "policy_1",
                "new_model",
                chrono::Utc::now().timestamp()
            )
            .unwrap());
        let serialized = serde_json::to_string(&record).unwrap();
        assert!(!serialized.contains("fixture/key"));
        assert!(!serialized.contains("fixture\\/key"));
        assert!(!serialized.contains("fixture.invalid"));
        assert!(record.probe.unwrap().response_redacted);
    }

    #[test]
    fn current_evidence_config_expiry_and_policy_are_all_required() {
        let db = Database::memory().unwrap();
        let provider = provider();
        let evidence = evidence();
        pass(&db, &provider, &evidence);
        let now = chrono::Utc::now().timestamp();
        for state in [Admission_state::Unknown, Admission_state::Inactive] {
            let mut current = evidence.clone();
            current.state = state;
            assert!(!db
                .provider_model_admitted(&provider, &current, "policy_1", "tested", now)
                .unwrap());
        }
        let mut current = evidence.clone();
        current.revision = "source_revision_2".into();
        assert!(!db
            .provider_model_admitted(&provider, &current, "policy_1", "tested", now)
            .unwrap());
        assert!(!db
            .provider_model_admitted(
                &provider,
                &evidence,
                "policy_1",
                "tested",
                evidence.valid_until.unwrap()
            )
            .unwrap());
        assert!(!db
            .provider_model_admitted(&provider, &evidence, "policy_2", "tested", now)
            .unwrap());
        let mut changed = provider.clone();
        changed.api_key = "rotated_key".into();
        assert!(!db
            .provider_model_admitted(&changed, &evidence, "policy_1", "tested", now)
            .unwrap());
    }

    #[test]
    fn begin_revokes_all_pairs_and_stale_finishes_cannot_restore_them() {
        let db = Database::memory().unwrap();
        let provider = provider();
        let evidence = evidence();
        let version = pass(&db, &provider, &evidence);
        let pending = db
            .provider_admission_begin(&provider, &evidence, "policy_1", version)
            .unwrap();
        assert!(db
            .accepted_provider_models(
                &provider,
                &evidence,
                "policy_1",
                chrono::Utc::now().timestamp()
            )
            .unwrap()
            .is_empty());
        assert!(db
            .provider_admission_finish(
                &provider,
                &evidence,
                "policy_1",
                version,
                probe(true),
                vec!["tested".into()]
            )
            .is_err());
        let mut revoked = evidence.clone();
        revoked.state = Admission_state::Inactive;
        db.provider_admission_begin(&provider, &revoked, "policy_1", pending)
            .unwrap();
        assert!(db
            .provider_admission_finish(
                &provider,
                &evidence,
                "policy_1",
                pending,
                probe(true),
                vec!["tested".into()]
            )
            .is_err());
        assert!(db
            .accepted_provider_models(
                &provider,
                &evidence,
                "policy_1",
                chrono::Utc::now().timestamp()
            )
            .unwrap()
            .is_empty());
    }

    #[test]
    fn changed_config_cannot_complete_old_screening_and_failed_reports_stay_private() {
        let db = Database::memory().unwrap();
        let provider = provider();
        let evidence = evidence();
        let pending = db
            .provider_admission_begin(&provider, &evidence, "policy_1", 0)
            .unwrap();
        let mut changed = provider.clone();
        changed.base_url = "https://other.invalid/v1".into();
        assert!(db
            .provider_admission_finish(
                &changed,
                &evidence,
                "policy_1",
                pending,
                probe(true),
                vec!["tested".into()]
            )
            .is_err());
        db.provider_admission_finish(
            &provider,
            &evidence,
            "policy_1",
            pending,
            probe(false),
            vec!["tested".into()],
        )
        .unwrap();
        assert!(db
            .accepted_provider_models(
                &provider,
                &evidence,
                "policy_1",
                chrono::Utc::now().timestamp()
            )
            .unwrap()
            .is_empty());
        assert!(is_private_admission_setting(ENFORCEMENT_KEY));
        assert!(is_private_admission_setting("provider_subscription:source"));
        assert!(!is_private_admission_setting("provider_screening_policy"));
    }

    #[test]
    fn malformed_enforcement_or_record_fails_closed() {
        let db = Database::memory().unwrap();
        let provider = provider();
        let evidence = evidence();
        db.set_setting(ENFORCEMENT_KEY, "invalid").unwrap();
        assert!(db.provider_admission_enforced().is_err());
        db.set_setting(&record_key(&provider.id).unwrap(), "{}")
            .unwrap();
        assert!(db
            .accepted_provider_models(
                &provider,
                &evidence,
                "policy_1",
                chrono::Utc::now().timestamp()
            )
            .is_err());
    }

    #[test]
    fn positive_source_refresh_preserves_probe_and_cannot_restore_revoked_admission() {
        let db = Database::memory().unwrap();
        let provider = provider();
        let evidence = evidence();
        let version = pass(&db, &provider, &evidence);
        let before = db.provider_admission_record(&provider.id).unwrap().unwrap();
        let mut fresh = evidence.clone();
        fresh.valid_until = evidence.valid_until.map(|deadline| deadline + 60);
        let refreshed = db
            .provider_admission_refresh_evidence(&provider, &evidence, &fresh, "policy_1", version)
            .unwrap();
        let after = db.provider_admission_record(&provider.id).unwrap().unwrap();
        assert_eq!(
            serde_json::to_value(before.probe).unwrap(),
            serde_json::to_value(after.probe).unwrap()
        );
        assert_eq!(before.offered_models, after.offered_models);
        assert!(db
            .provider_model_admitted(
                &provider,
                &fresh,
                "policy_1",
                "offered",
                chrono::Utc::now().timestamp()
            )
            .unwrap());
        assert!(db
            .provider_admission_refresh_evidence(&provider, &evidence, &fresh, "policy_1", version)
            .is_err());
        let mut revoked = fresh.clone();
        revoked.state = Admission_state::Inactive;
        let pending = db
            .provider_admission_begin(&provider, &revoked, "policy_1", refreshed)
            .unwrap();
        let mut failed = probe(false);
        failed.probe_model.clear();
        failed.reason = "subscription_inactive".into();
        failed.latency_ms = None;
        let excluded = db
            .provider_admission_finish(&provider, &revoked, "policy_1", pending, failed, vec![])
            .unwrap();
        assert!(db
            .provider_admission_refresh_evidence(&provider, &revoked, &fresh, "policy_1", excluded)
            .is_err());
        assert!(!db
            .provider_model_admitted(
                &provider,
                &fresh,
                "policy_1",
                "offered",
                chrono::Utc::now().timestamp()
            )
            .unwrap());
    }

    #[test]
    fn late_source_response_cannot_overwrite_newer_revocation() {
        let db = Database::memory().unwrap();
        let active = r#"{"state":"active","revision":1}"#;
        let revoked = r#"{"state":"inactive","revision":2}"#;
        assert!(db
            .compare_exchange_provider_subscription("fixture", None, active)
            .unwrap());
        let expected = db.provider_subscription_value("fixture").unwrap();
        let before = db.routing_revision().unwrap();
        assert!(db
            .compare_exchange_provider_subscription("fixture", expected.as_deref(), revoked)
            .unwrap());
        assert_ne!(before, db.routing_revision().unwrap());
        assert!(!db
            .compare_exchange_provider_subscription("fixture", expected.as_deref(), active)
            .unwrap());
        assert_eq!(
            db.provider_subscription_value("fixture")
                .unwrap()
                .as_deref(),
            Some(revoked)
        );
        assert!(db
            .compare_exchange_provider_subscription("fixture", Some(revoked), "[]")
            .is_err());
        assert_eq!(
            db.provider_subscription_value("fixture")
                .unwrap()
                .as_deref(),
            Some(revoked)
        );
    }

    #[test]
    fn private_review_lists_provisional_failures_in_stable_order() {
        let db = Database::memory().unwrap();
        let provider = provider();
        let evidence = evidence();
        pass(&db, &provider, &evidence);
        let mut excluded = provider.clone();
        excluded.id = "aaa_provisional".into();
        let mut unknown = evidence.clone();
        unknown.state = Admission_state::Unknown;
        let pending = db
            .provider_admission_begin(&excluded, &unknown, "policy_1", 0)
            .unwrap();
        let mut failed = probe(false);
        failed.probe_model.clear();
        failed.reason = "subscription_unknown".into();
        db.provider_admission_finish(&excluded, &unknown, "policy_1", pending, failed, vec![])
            .unwrap();
        let records = db.list_provider_admission_records().unwrap();
        assert_eq!(
            records
                .iter()
                .map(|record| record.provider_id.as_str())
                .collect::<Vec<_>>(),
            vec!["aaa_provisional", "admission_fixture"]
        );
        assert!(!records[0].probe.as_ref().unwrap().eligible);
        assert!(db.get_proxy_provider("aaa_provisional").unwrap().is_none());
    }

    #[test]
    fn catalog_inventory_extension_preserves_probe_and_requires_current_authority() {
        let db = Database::memory().unwrap();
        let provider = provider();
        let evidence = evidence();
        let version = pass(&db, &provider, &evidence);
        let before = db.provider_admission_record(&provider.id).unwrap().unwrap();
        let next = db
            .provider_admission_extend_models(
                &provider,
                &evidence,
                "policy_1",
                version,
                vec!["tested".into(), "new-offer".into()],
            )
            .unwrap();
        let after = db.provider_admission_record(&provider.id).unwrap().unwrap();
        assert_eq!(
            serde_json::to_value(before.probe).unwrap(),
            serde_json::to_value(after.probe).unwrap()
        );
        assert_eq!(before.evidence, after.evidence);
        assert!(db
            .provider_model_admitted(
                &provider,
                &evidence,
                "policy_1",
                "new-offer",
                chrono::Utc::now().timestamp()
            )
            .unwrap());
        assert!(!db
            .provider_model_admitted(
                &provider,
                &evidence,
                "policy_1",
                "offered",
                chrono::Utc::now().timestamp()
            )
            .unwrap());
        assert!(db
            .provider_admission_extend_models(
                &provider,
                &evidence,
                "policy_1",
                version,
                vec!["tested".into()]
            )
            .is_err());
        assert!(db
            .provider_admission_extend_models(
                &provider,
                &evidence,
                "policy_1",
                next,
                vec!["untested".into()]
            )
            .is_err());
        let mut inactive = evidence.clone();
        inactive.state = Admission_state::Inactive;
        assert!(db
            .provider_admission_extend_models(
                &provider,
                &inactive,
                "policy_1",
                next,
                vec!["tested".into()]
            )
            .is_err());
        assert_eq!(
            db.provider_admission_record(&provider.id)
                .unwrap()
                .unwrap()
                .version,
            next
        );
    }
}
