//! Bounded provider/model terminal-outcome observations; no content,
//! credentials or network addresses.
//!
//! Health identities are tagged: a direct runtime provider id is `Proxy`,
//! while failover-queue members carry the exact `(app_type, source provider
//! row)` provenance emitted by the queue builder. Literal `queue-*` runtime
//! ids, multi-app id collisions, and runtime-id renames therefore cannot
//! merge or split observation history.
use crate::database::dao::routes::RouteHop;
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const MAX_MODELS: usize = 4096;
const OBSERVATION_TTL: Duration = Duration::from_secs(3600);
const FAILURE_COOLDOWN: Duration = Duration::from_secs(60);
const SMOOTHING: f64 = 0.2;

/// What a runtime provider id refers to in observation storage.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub(crate) enum Health_identity {
    #[serde(rename = "proxy")]
    Proxy {
        id: String,
    },
    #[serde(rename = "queue")]
    Queue {
        app_type: String,
        source_provider_id: String,
    },
}

/// Exact provenance of one failover-queue member: the synthesized runtime
/// provider id maps to the app row it was built from. Never guessed by
/// stripping `queue-*` prefixes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Queue_source {
    pub app_type: String,
    pub source_provider_id: String,
}

impl Queue_source {
    pub(crate) fn identity(&self) -> Health_identity {
        Health_identity::Queue {
            app_type: self.app_type.clone(),
            source_provider_id: self.source_provider_id.clone(),
        }
    }
}

/// Bounded terminal-failure facts retained for display: wall-clock time of
/// the failure, the owner sequence that ordered it, and one fixed safe
/// reason code. No raw errors, response content, URLs, or credentials.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Failure_facts {
    pub wall_ms: u64,
    pub sequence: u64,
    pub reason: &'static str,
}

/// Derived provider-level view across all of one identity's models, ordered
/// by owner sequence. `latest_success` is `None` when nothing terminal was
/// observed inside the retention window (unobserved, never a measured zero).
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Provider_health_view {
    pub observed: bool,
    pub latest_success: Option<bool>,
    pub failure_count: u64,
    pub last_failure: Option<Failure_facts>,
    /// The latest terminal outcome is a failure inside the existing
    /// per-model failure cooldown. A later success restores eligibility;
    /// the per-model `unavailable` cooldown stays model-scoped.
    pub in_cooldown: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Model_health_view {
    pub samples: u64,
    pub success_rate: Option<f64>,
    pub latency_ms: Option<f64>,
    pub unavailable: bool,
}

struct Observation {
    view: Model_health_view,
    updated: Instant,
    failed: Option<Instant>,
    /// Terminal failures recorded inside this observation's window
    /// (saturating; resets when the observation expires).
    failure_count: u64,
    /// Latest terminal outcome with the owning sequence.
    latest_success: Option<bool>,
    latest_sequence: u64,
    last_terminal_wall_ms: Option<u64>,
    last_failure: Option<Failure_facts>,
    last_failure_instant: Option<Instant>,
}

impl Default for Observation {
    fn default() -> Self {
        Self {
            view: Model_health_view::default(),
            updated: Instant::now(),
            failed: None,
            failure_count: 0,
            latest_success: None,
            latest_sequence: 0,
            last_terminal_wall_ms: None,
            last_failure: None,
            last_failure_instant: None,
        }
    }
}

#[derive(Default)]
pub(crate) struct Model_health {
    observations: HashMap<(Health_identity, String), Observation>,
    /// Monotonic owner sequence: orders terminal outcomes both within a
    /// model and across one provider's models.
    sequence: u64,
}

/// Normalize one upstream/error code into the bounded fixed reason set.
/// Mirrors the request-status code set for the overlapping values; unknown
/// inputs collapse to `provider_error`, never free-form text.
pub(crate) fn safe_reason(code: &str) -> &'static str {
    match code {
        "upstream_http_error" => "upstream_http_error",
        "upstream_timeout" => "upstream_timeout",
        "upstream_stream_error" | "proxy_stream_error" => "upstream_stream_error",
        "upstream_connection_error" => "upstream_connection_error",
        "cancelled" => "cancelled",
        _ => "provider_error",
    }
}

fn wall_clock_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

impl Model_health {
    pub(crate) fn unavailable_models(&self) -> HashSet<(Health_identity, String)> {
        self.observations
            .keys()
            .filter(|(identity, model)| self.view(identity, model).unavailable)
            .cloned()
            .collect()
    }

    pub(crate) fn view(&self, provider: &Health_identity, model: &str) -> Model_health_view {
        let Some(row) = self.observations.get(&(provider.clone(), model.into())) else {
            return Model_health_view::default();
        };
        if row.updated.elapsed() > OBSERVATION_TTL {
            return Model_health_view::default();
        }
        let mut view = row.view.clone();
        view.unavailable = row
            .failed
            .is_some_and(|time| time.elapsed() < FAILURE_COOLDOWN);
        view
    }

    /// Derived provider-level view: latest terminal outcome across the
    /// identity's in-window models by sequence, summed failure counts, and
    /// retained last-failure facts.
    pub(crate) fn provider_view(&self, provider: &Health_identity) -> Provider_health_view {
        let mut out = Provider_health_view::default();
        let mut latest: Option<&Observation> = None;
        let mut last_failure: Option<&Failure_facts> = None;
        for ((identity, _model), row) in &self.observations {
            if identity != provider || row.updated.elapsed() > OBSERVATION_TTL {
                continue;
            }
            out.observed = true;
            out.failure_count = out.failure_count.saturating_add(row.failure_count);
            if latest.is_none_or(|current| row.latest_sequence > current.latest_sequence) {
                latest = Some(row);
            }
            if let Some(facts) = &row.last_failure {
                if last_failure.is_none_or(|current| facts.sequence > current.sequence) {
                    last_failure = Some(facts);
                }
            }
        }
        if let Some(latest) = latest {
            out.latest_success = latest.latest_success;
            if latest.latest_success == Some(false) {
                out.in_cooldown = latest
                    .last_failure_instant
                    .is_some_and(|time| time.elapsed() < FAILURE_COOLDOWN);
            }
        }
        out.last_failure = last_failure.cloned();
        out
    }

    /// Record one terminal inference-attempt outcome. Failures take a fixed
    /// safe reason; success clears the per-model failure cooldown while the
    /// provider-level history (counts, last-failure facts) is retained.
    pub(crate) fn record_terminal(
        &mut self,
        provider: &Health_identity,
        model: &str,
        success: bool,
        reason: Option<&'static str>,
        latency_ms: f64,
    ) {
        self.record_terminal_at(
            provider,
            model,
            success,
            reason,
            latency_ms,
            Instant::now(),
            wall_clock_ms(),
        );
    }

    /// Time-injected core of [`Self::record_terminal`].
    fn record_terminal_at(
        &mut self,
        provider: &Health_identity,
        model: &str,
        success: bool,
        reason: Option<&'static str>,
        latency_ms: f64,
        now: Instant,
        wall_ms: u64,
    ) {
        self.sequence = self.sequence.saturating_add(1);
        let sequence = self.sequence;
        let key = (provider.clone(), model.to_string());
        if self.observations.len() >= MAX_MODELS && !self.observations.contains_key(&key) {
            if let Some(oldest) = self
                .observations
                .iter()
                .min_by_key(|(_, row)| row.updated)
                .map(|(key, _)| key.clone())
            {
                self.observations.remove(&oldest);
            }
        }
        let row = self.observations.entry(key).or_default();
        if row.updated.elapsed() > OBSERVATION_TTL {
            // Expired window: unknown is unobserved, so the new record starts
            // a fresh window rather than continuing stale counts.
            *row = Observation::default();
        }
        row.view.samples = row.view.samples.saturating_add(1);
        let sample = if success { 1.0 } else { 0.0 };
        row.view.success_rate = Some(row.view.success_rate.map_or(sample, |prior| {
            prior * (1.0 - SMOOTHING) + sample * SMOOTHING
        }));
        // Error response speed is not a useful successful-completion latency.
        if success {
            row.view.latency_ms = Some(row.view.latency_ms.map_or(latency_ms, |prior| {
                prior * (1.0 - SMOOTHING) + latency_ms * SMOOTHING
            }));
        }
        row.updated = now;
        row.failed = if success { None } else { Some(now) };
        row.latest_success = Some(success);
        row.latest_sequence = sequence;
        row.last_terminal_wall_ms = Some(wall_ms);
        if !success {
            row.failure_count = row.failure_count.saturating_add(1);
            row.last_failure = Some(Failure_facts {
                wall_ms,
                sequence,
                // Normalize again at the storage boundary: the retained
                // reason is a fixed safe code by construction.
                reason: safe_reason(reason.unwrap_or("provider_error")),
            });
            row.last_failure_instant = Some(now);
        }
    }

    /// Test hook: expire one observation's failure cooldown so router-level
    /// tests exercise ordinary cooldown recovery without waiting.
    #[cfg(test)]
    pub(crate) fn expire_failure_cooldown(&mut self, provider: &Health_identity, model: &str) {
        if let Some(row) = self
            .observations
            .get_mut(&(provider.clone(), model.to_string()))
        {
            row.last_failure_instant = Some(Instant::now() - FAILURE_COOLDOWN);
            row.failed = Some(Instant::now() - FAILURE_COOLDOWN);
        }
    }

    /// Select one preferred candidate, leaving every remaining fallback in
    /// its original deterministic order. Cooldowns expire without probes.
    /// Health lookups go through `identity_of` so callers translate runtime
    /// provider ids to tagged identities exactly once.
    pub(crate) fn prefer_with(
        &self,
        hops: &[RouteHop],
        identity_of: impl Fn(&str) -> Health_identity,
    ) -> Vec<RouteHop> {
        let mut result: Vec<_> = hops
            .iter()
            .filter(|hop| {
                !self
                    .view(&identity_of(&hop.provider), &hop.model)
                    .unavailable
            })
            .cloned()
            .collect();
        if result.len() < 2 {
            return result;
        }
        let views: Vec<Model_health_view> = result
            .iter()
            .map(|hop| self.view(&identity_of(&hop.provider), &hop.model))
            .collect();
        // Success on a different role is not evidence against an untried
        // preferred model. Preserve policy ordering until its own health is known.
        if views[0].samples == 0 { return result; }
        let best = views
            .iter()
            .enumerate()
            .map(|(index, view)| {
                let reliability = view.success_rate.unwrap_or(0.75);
                let latency = view
                    .latency_ms
                    .map(|ms| ms / (ms + 10_000.0))
                    .unwrap_or(0.5);
                // The utility order remains dominant unless observed health
                // differs materially.
                let score =
                    0.65 / (1.0 + index as f64 * 0.15) + 0.30 * reliability - 0.05 * latency;
                (index, score)
            })
            .max_by(|left, right| left.1.total_cmp(&right.1))
            .map(|(index, _)| index)
            .unwrap_or(0);
        let preferred = result.remove(best);
        result.insert(0, preferred);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proxy(id: &str) -> Health_identity {
        Health_identity::Proxy { id: id.into() }
    }

    fn queue(app: &str, source: &str) -> Health_identity {
        Queue_source {
            app_type: app.into(),
            source_provider_id: source.into(),
        }
        .identity()
    }

    fn hops() -> Vec<RouteHop> {
        ["a", "b", "c"]
            .into_iter()
            .map(|model| RouteHop {
                provider: "shared".into(),
                model: model.into(),
            })
            .collect()
    }

    #[test]
    fn warm_fallback_does_not_displace_an_untried_policy_head() {
        let mut health = Model_health::default();
        health.record_terminal(&proxy("shared"), "b", true, None, 1.0);
        let preferred = health.prefer_with(&hops(), |id| proxy(id));
        assert_eq!(preferred.iter().map(|hop| hop.model.as_str()).collect::<Vec<_>>(), vec!["a", "b", "c"]);
    }

    #[test]
    fn failed_model_does_not_disable_sibling_and_recovery_restores_order() {
        let mut health = Model_health::default();
        health.record_terminal(&proxy("shared"), "a", false, Some("upstream_http_error"), 20.0);
        assert_eq!(
            health
                .prefer_with(&hops(), |id| proxy(id))
                .iter()
                .map(|hop| hop.model.as_str())
                .collect::<Vec<_>>(),
            vec!["b", "c"]
        );
        assert_eq!(health.view(&proxy("shared"), "b").samples, 0);
        health.record_terminal(&proxy("shared"), "a", true, None, 100.0);
        assert!(!health.view(&proxy("shared"), "a").unavailable);
        assert_eq!(health.prefer_with(&hops(), |id| proxy(id)).len(), 3);
    }

    #[test]
    fn expired_failure_becomes_eligible_without_paid_probe() {
        let mut health = Model_health::default();
        health.record_terminal(&proxy("shared"), "a", false, Some("upstream_timeout"), 10.0);
        health
            .observations
            .get_mut(&(proxy("shared"), "a".into()))
            .unwrap()
            .last_failure_instant = Some(Instant::now() - FAILURE_COOLDOWN);
        health
            .observations
            .get_mut(&(proxy("shared"), "a".into()))
            .unwrap()
            .failed = Some(Instant::now() - FAILURE_COOLDOWN);
        assert!(!health.view(&proxy("shared"), "a").unavailable);
        assert!(!health.provider_view(&proxy("shared")).in_cooldown);
        assert_eq!(health.prefer_with(&hops(), |id| proxy(id)).len(), 3);
    }

    /// Terminal outcomes accumulate continuously: counts grow per failure,
    /// the latest reason/time follow the most recent failure, and the owner
    /// sequence orders everything.
    #[test]
    fn repeated_failures_accumulate_counts_time_and_latest_reason() {
        let mut health = Model_health::default();
        health.record_terminal(&proxy("p"), "m", false, Some("upstream_timeout"), 10.0);
        health.record_terminal(&proxy("p"), "m", false, Some("upstream_http_error"), 12.0);
        health.record_terminal(&proxy("p"), "m", false, Some("secret-looking-text"), 14.0);
        let view = health.provider_view(&proxy("p"));
        assert_eq!(view.failure_count, 3);
        assert_eq!(view.latest_success, Some(false));
        assert!(view.in_cooldown);
        let facts = view.last_failure.expect("retained failure facts");
        assert_eq!(facts.reason, "provider_error", "unknown codes collapse to the fixed default");
        assert_eq!(facts.sequence, 3, "facts follow the most recent failure");
        assert!(facts.wall_ms > 1_700_000_000_000, "wall time is a real epoch millisecond value");
        assert_eq!(
            health.observations[&(proxy("p"), "m".into())].failure_count, 3,
            "per-model counts accumulate too"
        );
    }

    /// Provider-level state follows the latest terminal outcome across its
    /// models: a later success on a sibling model restores eligibility while
    /// the failed model keeps its own cooldown and history is retained.
    #[test]
    fn later_success_on_sibling_model_restores_provider_eligibility_and_keeps_history() {
        let mut health = Model_health::default();
        health.record_terminal(&proxy("p"), "m1", false, Some("upstream_http_error"), 10.0);
        let failed = health.provider_view(&proxy("p"));
        assert_eq!(failed.latest_success, Some(false));
        assert!(failed.in_cooldown);
        assert!(health.view(&proxy("p"), "m1").unavailable);

        health.record_terminal(&proxy("p"), "m2", true, None, 50.0);
        let restored = health.provider_view(&proxy("p"));
        assert_eq!(restored.latest_success, Some(true), "sequence order wins across models");
        assert!(!restored.in_cooldown, "a later success restores provider eligibility");
        assert_eq!(restored.failure_count, 1, "failure history is retained");
        assert!(restored.last_failure.is_some(), "last-failure facts survive the success");
        assert!(
            health.view(&proxy("p"), "m1").unavailable,
            "sibling-model isolation: the failed model keeps its cooldown"
        );
        assert!(
            !health.view(&proxy("p"), "m2").unavailable,
            "the succeeding model is not penalized"
        );
    }

    /// Tagged identities never merge: a direct runtime id, the same queue
    /// source reached through two runtime ids, and a same-named queue member
    /// of another app are three independent observation histories.
    #[test]
    fn tagged_identities_isolate_queue_multiapp_and_direct_histories() {
        let mut health = Model_health::default();
        health.record_terminal(&queue("claude_code", "shared"), "m", false, Some("upstream_http_error"), 5.0);
        health.record_terminal(&queue("codex", "shared"), "m", true, None, 6.0);
        health.record_terminal(&proxy("shared"), "m", true, None, 7.0);
        let claude = health.provider_view(&queue("claude_code", "shared"));
        assert_eq!(claude.latest_success, Some(false));
        assert!(claude.in_cooldown);
        let codex = health.provider_view(&queue("codex", "shared"));
        assert_eq!(codex.latest_success, Some(true));
        assert!(!codex.in_cooldown);
        let direct = health.provider_view(&proxy("shared"));
        assert_eq!(direct.latest_success, Some(true), "its own success");
        assert_eq!(
            direct.failure_count, 0,
            "the claude queue failure must not leak into the direct runtime id"
        );
        assert!(!direct.in_cooldown);
    }

    /// An expired observation window is unobserved: counts disappear rather
    /// than reporting stale measured zeros.
    #[test]
    fn expired_window_is_unobserved_not_zero_measured() {
        let mut health = Model_health::default();
        health.record_terminal(&proxy("p"), "m", false, Some("upstream_http_error"), 5.0);
        health
            .observations
            .get_mut(&(proxy("p"), "m".into()))
            .unwrap()
            .updated = Instant::now() - OBSERVATION_TTL - Duration::from_secs(1);
        let view = health.provider_view(&proxy("p"));
        assert!(!view.observed);
        assert_eq!(view.latest_success, None);
        assert_eq!(view.failure_count, 0);
        assert!(!view.in_cooldown);
    }

    #[test]
    fn safe_reason_is_a_fixed_bounded_set() {
        assert_eq!(safe_reason("upstream_stream_error"), "upstream_stream_error");
        assert_eq!(safe_reason("proxy_stream_error"), "upstream_stream_error");
        assert_eq!(safe_reason("cancelled"), "cancelled");
        assert_eq!(safe_reason("anything with secrets or urls"), "provider_error");
    }
}
