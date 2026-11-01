//! Atomic, hot-swappable routing snapshots.
//!
//! `Live_runtime` owns the runtime's current snapshot, builds replacement
//! snapshots without holding the publication lock, and atomically publishes
//! only snapshots whose routing inputs have not changed during construction.
//! Authenticated catalog refresh may update private subscription evidence;
//! provider configuration and client files remain unchanged. Failed catalog
//! refreshes are refused by the HTTP boundary instead of serving stale lists.

use super::snapshot::Runtime_snapshot;
use crate::database::dao::proxy_config::ProxyConfig;
use crate::database::Database;
use std::sync::{Arc, RwLock};
use tokio::sync::Mutex;

const ADMISSION_CATALOG_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

#[allow(non_camel_case_types)]
enum Admission_reload {
    Refresh,
    Persisted,
}

/// Stable lifecycle errors only; do not expose paths, URLs, credentials, or
/// database details in the user-visible status code.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
#[allow(non_camel_case_types)]
pub struct Live_runtime_status {
    pub generation: u64,
    pub routing_revision: u64,
    pub active: bool,
    pub last_error: Option<String>,
}

#[allow(non_camel_case_types)]
pub struct Live_runtime {
    snapshot: RwLock<Arc<Runtime_snapshot>>,
    generation_gate: Mutex<u64>,
    db: Database,
    config: ProxyConfig,
    last_error: RwLock<Option<String>>,
}

impl Live_runtime {
    pub(crate) fn new(db: Database, config: ProxyConfig, snapshot: Runtime_snapshot) -> Arc<Self> {
        Arc::new(Self {
            snapshot: RwLock::new(Arc::new(snapshot)),
            generation_gate: Mutex::new(0),
            db,
            config,
            last_error: RwLock::new(None),
        })
    }

    /// Clone the snapshot captured for one request or in-process consumer.
    /// The lock is held only for the clone; consumers never re-enter it while
    /// awaiting an upstream.
    pub(crate) fn current(&self) -> Arc<Runtime_snapshot> {
        self.snapshot
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    pub(crate) fn routing_revision(&self) -> u64 {
        self.current().routing_revision
    }

    pub(crate) fn pinned_provider(&self) -> Option<String> {
        self.current().pinned_provider.clone()
    }

    /// Caller must authenticate gateway authority before invoking this method.
    /// Refresh only status evidence, then publish a fresh catalog when source
    /// eligibility changed even without a database revision (expiry/CC edits).
    pub(crate) async fn refresh_admission(self: &Arc<Self>) -> Result<bool, String> {
        if !crate::services::provider_admission::enforcement_enabled(&self.db) {
            return Ok(false);
        }
        let result = tokio::time::timeout(ADMISSION_CATALOG_TIMEOUT, async {
            let _gate = self.generation_gate.lock().await;
            if !crate::services::provider_admission::enforcement_enabled(&self.db) {
                return Ok(false);
            }
            crate::services::provider_admission::refresh_runtime_subscriptions(&self.db).await?;
            let current = self.current();
            let revision = self.db.routing_revision().map_err(String::from)?;
            let configured = self.db.get_all_proxy_providers().map_err(String::from)?;
            let mut fresh_pairs = std::collections::BTreeSet::new();
            for provider in &configured {
                for model in
                    crate::services::provider_admission::admitted_models(&self.db, provider)
                {
                    if provider.enabled && provider.models.contains(&model) {
                        fresh_pairs.insert((provider.id.clone(), model));
                    }
                }
            }
            let previous_pairs: std::collections::BTreeSet<_> = current
                .db_providers
                .iter()
                .filter(|provider| provider.enabled)
                .flat_map(|provider| {
                    provider
                        .models
                        .iter()
                        .map(move |model| (provider.id.clone(), model.clone()))
                })
                .collect();
            if revision == current.routing_revision && fresh_pairs == previous_pairs {
                return Ok(false);
            }
            let next = super::snapshot::build_runtime_snapshot_from(
                &self.db,
                &self.config,
                Some(&current),
            )
            .await?;
            *self
                .snapshot
                .write()
                .unwrap_or_else(|error| error.into_inner()) = Arc::new(next);
            Ok(true)
        })
        .await
        .map_err(|_| "model_catalog_refresh_timeout".to_string())
        .and_then(|result: Result<bool, String>| {
            result.map_err(|_| "model_catalog_refresh_failed".to_string())
        });
        *self
            .last_error
            .write()
            .unwrap_or_else(|error| error.into_inner()) = result.as_ref().err().cloned();
        result
    }

    /// Publish a selected recheck's persisted result without refreshing another
    /// provider's subscription/cache/admission records. Expired persisted
    /// evidence stays excluded; this method grants no renewal or approval.
    pub(crate) async fn reload_persisted_admission(self: &Arc<Self>) -> Result<bool, String> {
        self.reload_with_admission(Admission_reload::Persisted)
            .await
    }

    /// Build and publish a fresh snapshot when the routing fingerprint changed.
    /// A failed build or concurrent generation change leaves the previous
    /// snapshot active and records a stable error code.
    pub(crate) async fn reload(self: &Arc<Self>) -> Result<bool, String> {
        self.reload_with_admission(Admission_reload::Refresh).await
    }

    async fn reload_with_admission(
        self: &Arc<Self>,
        admission: Admission_reload,
    ) -> Result<bool, String> {
        let current = self.current();
        let current_revision = current.routing_revision;
        if self.db.routing_revision().map_err(String::from)? == current_revision {
            return Ok(false);
        }
        let gate = self.generation_gate.lock().await;
        let current = self.current();
        if self.db.routing_revision().map_err(String::from)? == current.routing_revision {
            return Ok(false);
        }
        let current_generation = current.generation;
        let built = match admission {
            Admission_reload::Refresh => {
                super::snapshot::build_runtime_snapshot_from(
                    &self.db,
                    &self.config,
                    Some(current.as_ref()),
                )
                .await
            }
            Admission_reload::Persisted => {
                super::snapshot::build_persisted_admission_snapshot_from(
                    &self.db,
                    &self.config,
                    Some(current.as_ref()),
                )
                .await
            }
        };
        let next = match built {
            Ok(next) => next,
            Err(_) => {
                *self
                    .last_error
                    .write()
                    .unwrap_or_else(|error| error.into_inner()) =
                    Some("proxy_runtime_reload_build_failed".into());
                return Err("proxy_runtime_reload_build_failed".into());
            }
        };
        if self
            .snapshot
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .generation
            != current_generation
        {
            return Ok(false);
        }
        let generation = next.generation;
        *self
            .snapshot
            .write()
            .unwrap_or_else(|error| error.into_inner()) = Arc::new(next);
        *self
            .last_error
            .write()
            .unwrap_or_else(|error| error.into_inner()) = None;
        drop(gate);
        log::info!(
            "proxy: hot reload applied generation={} routing_revision={}",
            generation,
            self.routing_revision()
        );
        Ok(true)
    }

    pub(crate) fn status(&self) -> Live_runtime_status {
        let snapshot = self.current();
        Live_runtime_status {
            generation: snapshot.generation,
            routing_revision: snapshot.routing_revision,
            active: true,
            last_error: self
                .last_error
                .read()
                .unwrap_or_else(|error| error.into_inner())
                .clone(),
        }
    }
}

/// Isolated positive source/receipt fixture shared with the HTTP catalog tests.
/// It uses a mock transport and cached evidence; no upstream request is made.
#[cfg(test)]
pub(crate) async fn admission_catalog_fixture() -> (
    Database,
    Arc<Live_runtime>,
    crate::config::private_fs::PrivateTempDir,
) {
    use crate::database::dao::{
        provider_admission::{Admission_evidence, Admission_probe, Admission_state},
        proxy_providers::ProxyProvider,
    };
    use crate::services::{
        provider_screening::{ScreeningConfig, SubscriptionEvidence, SubscriptionState},
        subscription_source::{SourceReason, SubscriptionObservation},
    };
    let root =
        crate::config::private_fs::private_temp_dir(&std::env::temp_dir().canonicalize().unwrap())
            .unwrap();
    let path = root.path().join("source.db");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection.execute_batch("CREATE TABLE providers(id TEXT,app_type TEXT,name TEXT,settings_config TEXT,website_url TEXT,category TEXT,created_at INTEGER,sort_index INTEGER,notes TEXT,icon TEXT,icon_color TEXT,meta TEXT,is_current INTEGER,in_failover_queue INTEGER);").unwrap();
    connection.execute("INSERT INTO providers(id,app_type,name,settings_config,meta,is_current,in_failover_queue) VALUES('source','claude','Fixture',?1,'{}',0,0)", [serde_json::json!({"env":{"ANTHROPIC_BASE_URL":"http://127.0.0.1:9123","ANTHROPIC_AUTH_TOKEN":"catalog_fixture_key"}}).to_string()]).unwrap();
    let db = Database::memory().unwrap();
    let provider:ProxyProvider=serde_json::from_value(serde_json::json!({"id":"catalog_fixture","name":"Fixture","type":"mock","apiKey":"catalog_fixture_key","baseURL":"http://127.0.0.1:9123","models":["m"],"enabled":true})).unwrap();
    db.save_proxy_provider(&provider).unwrap();
    let plan = crate::services::provider_admission::bind_source(
        &db,
        &provider,
        &path,
        "claude_code:source",
        false,
    )
    .unwrap();
    let original = db
        .provider_subscription_value(&provider.id)
        .unwrap()
        .unwrap();
    let mut source: serde_json::Value = serde_json::from_str(&original).unwrap();
    let evidence = SubscriptionEvidence {
        state: SubscriptionState::Active,
        provider_ref: provider.id.clone(),
        evidence_id: format!("cc_switch:{}", plan.binding),
        provenance: "explicit_test_fixture".into(),
        valid_until: Some(chrono::Utc::now().timestamp() + 300),
    };
    source["observation"] = serde_json::to_value(SubscriptionObservation {
        evidence: evidence.clone(),
        probe_allowed: true,
        reason: SourceReason::VerifiedAllowance,
        source_binding: plan.binding,
        source_record_ref: "claude_code:source".into(),
        checked_at: chrono::Utc::now().timestamp(),
        status_http: None,
    })
    .unwrap();
    assert!(db
        .compare_exchange_provider_subscription(&provider.id, Some(&original), &source.to_string())
        .unwrap());
    let evidence = Admission_evidence {
        revision: evidence.evidence_id,
        state: Admission_state::Active,
        valid_until: evidence.valid_until,
    };
    let policy = crate::services::provider_admission::policy_revision(&ScreeningConfig::default());
    let pending = db
        .provider_admission_begin(&provider, &evidence, &policy, 0)
        .unwrap();
    db.provider_admission_finish(
        &provider,
        &evidence,
        &policy,
        pending,
        Admission_probe {
            probe_model: "m".into(),
            checked_at: chrono::Utc::now().timestamp(),
            eligible: true,
            reason: "accepted".into(),
            latency_ms: Some(1),
            response: Some("ok".into()),
            response_truncated: false,
            response_redacted: false,
            upstream_status: None,
            upstream_code: None,
        },
        vec!["m".into()],
    )
    .unwrap();
    db.set_provider_admission_enforced(true).unwrap();
    let config = db.get_proxy_config().unwrap();
    let snapshot = super::snapshot::build_runtime_snapshot(&db, &config)
        .await
        .unwrap();
    let live = Live_runtime::new(db.clone(), config, snapshot);
    (db, live, root)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn accept_selected_fixture(db: &Database, source_path: &std::path::Path, model: &str) {
        use crate::database::dao::provider_admission::{
            Admission_evidence, Admission_probe, Admission_state,
        };
        use crate::services::provider_screening::ScreeningConfig;
        let mut provider = db.get_proxy_provider("catalog_fixture").unwrap().unwrap();
        provider.id = "selected_fixture".into();
        provider.models = vec![model.into()];
        db.save_proxy_provider(&provider).unwrap();
        let plan = crate::services::provider_admission::bind_source(
            db,
            &provider,
            source_path,
            "claude_code:source",
            false,
        )
        .unwrap();
        let original = db
            .provider_subscription_value(&provider.id)
            .unwrap()
            .unwrap();
        let mut source: serde_json::Value = serde_json::from_str(&original).unwrap();
        let now = chrono::Utc::now().timestamp();
        let revision = format!("cc_switch:{}", plan.binding);
        source["observation"] = serde_json::json!({
            "evidence":{"state":"active","providerRef":provider.id,"evidenceId":revision,"provenance":"explicit_test_fixture","validUntil":now+300},
            "probeAllowed":true,"reason":"verified_allowance","sourceBinding":plan.binding,
            "sourceRecordRef":"claude_code:source","checkedAt":now,"statusHttp":null
        });
        assert!(db
            .compare_exchange_provider_subscription(
                &provider.id,
                Some(&original),
                &source.to_string()
            )
            .unwrap());
        let evidence = Admission_evidence {
            revision,
            state: Admission_state::Active,
            valid_until: Some(now + 300),
        };
        let policy =
            crate::services::provider_admission::policy_revision(&ScreeningConfig::default());
        let prior_version = db
            .provider_admission_record(&provider.id)
            .unwrap()
            .map(|record| record.version)
            .unwrap_or(0);
        let pending = db
            .provider_admission_begin(&provider, &evidence, &policy, prior_version)
            .unwrap();
        db.provider_admission_finish(
            &provider,
            &evidence,
            &policy,
            pending,
            Admission_probe {
                probe_model: model.into(),
                checked_at: now,
                eligible: true,
                reason: "accepted".into(),
                latency_ms: Some(1),
                response: Some("ok".into()),
                response_truncated: false,
                response_redacted: false,
                upstream_status: None,
                upstream_code: None,
            },
            vec![model.into()],
        )
        .unwrap();
    }

    #[tokio::test]
    async fn persisted_admission_reload_applies_selected_changes_without_refreshing_unselected_stale_source(
    ) {
        let (db, live, root) = admission_catalog_fixture().await;
        let source_path = root.path().join("source.db");
        accept_selected_fixture(&db, &source_path, "selected-before");
        assert!(live.reload().await.unwrap());
        let in_flight = live.current();
        assert!(in_flight
            .router
            .catalog_models("chat", None)
            .contains(&"selected-before".into()));

        let original = db
            .provider_subscription_value("catalog_fixture")
            .unwrap()
            .unwrap();
        let mut stale: serde_json::Value = serde_json::from_str(&original).unwrap();
        stale["observation"]["evidence"]["validUntil"] =
            serde_json::json!(chrono::Utc::now().timestamp() - 1);
        assert!(db
            .compare_exchange_provider_subscription(
                "catalog_fixture",
                Some(&original),
                &stale.to_string()
            )
            .unwrap());
        accept_selected_fixture(&db, &source_path, "selected-after");
        let unselected_source = db.provider_subscription_value("catalog_fixture").unwrap();
        let unselected_admission =
            serde_json::to_value(db.provider_admission_record("catalog_fixture").unwrap()).unwrap();

        // This is the real publication seam used after a selected provider's
        // persisted check. It must not renew or revoke another provider's record.
        assert!(live.reload_persisted_admission().await.unwrap());
        assert!(
            db.provider_subscription_value("catalog_fixture").unwrap() == unselected_source,
            "singular recheck changed unselected subscription bytes"
        );
        assert_eq!(
            serde_json::to_value(db.provider_admission_record("catalog_fixture").unwrap()).unwrap(),
            unselected_admission
        );
        let current = live.current();
        let models = current.router.catalog_models("chat", None);
        assert!(models.contains(&"selected-after".into()));
        assert!(!models.contains(&"selected-before".into()));
        assert!(
            !models.contains(&"m".into()),
            "expired evidence is excluded without refreshing it"
        );
        assert!(in_flight
            .router
            .catalog_models("chat", None)
            .contains(&"selected-before".into()));
        assert!(!live.reload_persisted_admission().await.unwrap());
        assert!(
            db.provider_subscription_value("catalog_fixture").unwrap() == unselected_source,
            "no-op singular reload changed unselected subscription bytes"
        );
    }

    #[tokio::test]
    async fn fresh_admission_is_noop_but_external_source_change_rebuilds_empty_pool() {
        let (db, live, root) = admission_catalog_fixture().await;
        let generation = live.current().generation;
        let revision = db.routing_revision().unwrap();
        let source = db.provider_subscription_value("catalog_fixture").unwrap();
        assert!(!live.refresh_admission().await.unwrap());
        assert_eq!(live.current().generation, generation);
        assert_eq!(
            db.provider_subscription_value("catalog_fixture").unwrap(),
            source
        );
        rusqlite::Connection::open(root.path().join("source.db"))
            .unwrap()
            .execute("UPDATE providers SET meta='{} '", [])
            .unwrap();
        assert_eq!(
            db.routing_revision().unwrap(),
            revision,
            "CC source edits do not modify target revision"
        );
        assert!(live.refresh_admission().await.unwrap());
        assert_eq!(live.current().queue_count, 0);
        assert!(live
            .current()
            .router
            .catalog_models("chat", None)
            .is_empty());
    }

    #[tokio::test]
    async fn queued_reload_applies_latest_database_after_an_intermediate_publication() {
        let db = Database::memory().unwrap();
        let config = db.get_proxy_config().unwrap();
        let initial = super::super::snapshot::build_runtime_snapshot(&db, &config)
            .await
            .unwrap();
        let live = Live_runtime::new(db.clone(), config.clone(), initial);
        let gate = live.generation_gate.lock().await;
        db.set_setting(crate::database::dao::settings::KEY_ROUTING_MODE, "rotate")
            .unwrap();
        let pending = live.reload();
        tokio::pin!(pending);
        assert!(futures::poll!(&mut pending).is_pending());
        let intermediate = super::super::snapshot::build_runtime_snapshot_from(
            &db,
            &config,
            Some(live.current().as_ref()),
        )
        .await
        .unwrap();
        *live.snapshot.write().unwrap() = Arc::new(intermediate);
        db.set_setting(crate::database::dao::settings::KEY_ROUTING_MODE, "usage")
            .unwrap();
        let latest_revision = db.routing_revision().unwrap();
        drop(gate);
        assert!(pending.await.unwrap());
        assert_eq!(live.routing_revision(), latest_revision);
        assert_eq!(live.current().generation, 3);
    }
}
