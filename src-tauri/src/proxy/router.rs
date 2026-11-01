//! Router — resolves model aliases to ordered failover chains and executes
//! them with circuit breaker protection.
//!
//! Ported from `src/gateway/router.ts`.

use crate::database::dao::routes::RouteHop;
use crate::proxy::canonical::{
    self, ChatCompletionRequest, OAIChatResponse, OAIStreamChunk, ProviderError,
};
use crate::proxy::circuit_breaker::{CircuitBreakerSnapshot, Circuit_outcome, Circuit_permit};
use crate::proxy::model_health::{
    safe_reason, Health_identity, Provider_health_view, Queue_source,
};
use crate::proxy::providers::Provider;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

/// Derived terminal-attempt failure view for one provider, in the shared
/// bounded memory window. `latest_outcome` uses the confirmed vocabulary:
/// `ok` for a terminal inference success, `failed` for any terminal
/// non-success. `None` means unobserved (never a measured zero).
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Provider_failure_health {
    pub observed: bool,
    pub latest_outcome: Option<&'static str>,
    pub failure_count: u64,
    pub in_cooldown: bool,
    pub last_failure: Option<Provider_last_failure>,
}

/// Bounded facts about the most recent terminal failure.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Provider_last_failure {
    /// Unix epoch milliseconds of the failure.
    pub wall_ms: u64,
    /// Owner sequence that ordered the terminal outcome.
    pub sequence: u64,
    /// One fixed safe reason code.
    pub reason: String,
}

impl From<Provider_health_view> for Provider_failure_health {
    fn from(view: Provider_health_view) -> Self {
        Self {
            observed: view.observed,
            latest_outcome: view
                .latest_success
                .map(|success| if success { "ok" } else { "failed" }),
            failure_count: view.failure_count,
            in_cooldown: view.in_cooldown,
            last_failure: view.last_failure.map(|facts| Provider_last_failure {
                wall_ms: facts.wall_ms,
                sequence: facts.sequence,
                reason: facts.reason.to_string(),
            }),
        }
    }
}

/// One configured failover-queue candidate in the read-only health
/// projection: the runtime id, its explicit source association, whether a
/// live provider instance backs it in this snapshot, and the derived
/// failure view (absent, not zero, while unresolved).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Queue_candidate_health {
    pub runtime_id: String,
    pub source: Health_identity,
    pub resolved: bool,
    pub failure: Option<Provider_failure_health>,
}

/// Disjoint candidate outcome counts over the configured queue members.
/// `ok` + `failed` + `unobserved` partition the resolved candidates;
/// `unresolved` counts configured ids with no live provider instance.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Queue_health_totals {
    pub total: u64,
    pub ok: u64,
    pub failed: u64,
    pub unobserved: u64,
    pub unresolved: u64,
}

/// Read-only projection of the configured legacy queue and per-surface
/// failover queues, including configured ids the runtime cannot resolve.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Queue_health_projection {
    pub candidates: Vec<Queue_candidate_health>,
    pub totals: Queue_health_totals,
}

/// Health snapshot for a single provider (used in /health response).
#[derive(Debug, Serialize)]
pub struct ProviderHealth {
    pub id: String,
    pub enabled: bool,
    pub breaker: CircuitBreakerSnapshot,
    pub failure: Provider_failure_health,
}

/// A resolved route: the alias name and its ordered hops.
#[allow(dead_code)]
pub struct ResolvedRoute {
    pub alias: String,
    pub hops: Vec<RouteHop>,
}

/// How failover-queue candidate chains are ordered before the hop walkers
/// try them (Magpie's `internal/provider/routing.go` mode list). Magpie's
/// `pace` mode stays out of scope: AAG has no vendor allowance windows to
/// pace against, so [`Routing_mode::Usage`] is observed-traffic spreading
/// only, not quota pacing.
///
/// Scope: only chains the queues build (same-model matches and the
/// last-chance chains) are spread. Explicit per-model routes and
/// planner-assessed alias candidates keep their order in every mode — a
/// hand-ordered route is a priority statement, and the planner's order is
/// its tier assessment.
#[allow(non_camel_case_types)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Routing_mode {
    /// Configured order (queue priority, then stable provider id). The
    /// historical behavior and the default.
    #[default]
    Order,
    /// Round-robin the chain's starting position per request ("each in
    /// turn"). Failover walks the rotated order, so a dead first hop still
    /// falls through to the next member.
    Rotate,
    /// Smooth weighted round-robin over the chain's members using each
    /// provider's configured weight (nginx-style: 3 and 1 go a, a, b, a…).
    /// Weight 0 means standby-only: never picked first while any
    /// positive-weight member is available, but still present as failover.
    Weight,
    /// Least observed usage first: members are ordered by the token traffic
    /// (prompt + completion) AAG recorded for them on the request success
    /// path over a tumbling one-hour window, lowest first. Each provider's
    /// bucket resets on the first record after it expires, so the window is
    /// TTL-based rather than a timestamp history (see `Usage_ledger`).
    /// Ties and providers without history keep the configured order, so a
    /// cold chain behaves exactly like `Order` until traffic is observed.
    Usage,
}

impl Routing_mode {
    /// Settings-decoded form. Unknown or absent values are `Order`, so a
    /// corrupt setting degrades to the historical behavior rather than
    /// failing proxy start.
    pub fn parse(value: Option<&str>) -> Self {
        match value.map(str::trim) {
            Some("rotate") => Self::Rotate,
            Some("weight") => Self::Weight,
            Some("usage") => Self::Usage,
            _ => Self::Order,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Order => "order",
            Self::Rotate => "rotate",
            Self::Weight => "weight",
            Self::Usage => "usage",
        }
    }
}

/// Mutable spread state for queue-built chains. Held behind an
/// `Arc<Mutex<_>>` on the Router and carried across snapshot generations
/// like `last_used`, so hot-apply does not reset the rotation. The rotate
/// offset is keyed by the chain's provider list (not by model): several
/// models served by the same chain share one rotation, which is the spread
/// the mode is for. Weight counters are keyed by provider id globally, so
/// they survive chain reshapes and still self-correct inside any chain
/// (SWRR only ever compares chain members).
#[allow(non_camel_case_types)]
#[derive(Default, Clone)]
pub(crate) struct Chain_spread {
    /// Next starting offset per chain key (`rotate`).
    offsets: HashMap<String, usize>,
    /// Persisted SWRR current per provider id (`weight`). One classic
    /// round is persisted per resolve (the first-pick spread); the rest of
    /// the failover order is computed from a scratch walk so a request that
    /// only uses its first hop mutates the state exactly once.
    currents: HashMap<String, i64>,
}

impl Chain_spread {
    /// Bound the maps: an unbounded key set would grow with client traffic.
    /// When a map exceeds the cap it is dropped wholesale — losing rotation
    /// state is benign, the next resolve re-seeds it.
    fn bound(&mut self) {
        const MAX_KEYS: usize = 1024;
        if self.offsets.len() > MAX_KEYS {
            self.offsets.clear();
        }
        if self.currents.len() > MAX_KEYS {
            self.currents.clear();
        }
    }
}

/// Window over which [`Routing_mode::Usage`] counts traffic: one hour.
///
/// The choice trades resolution for a cheap hot path. A tumbling per-provider
/// bucket (reset when the first request lands after expiry) needs one
/// `Instant` and one counter per provider, no per-request timestamp history,
/// and no sweep on the request path. One hour outlives a typical failover
/// burst (so a provider that just absorbed a spike keeps yielding to its
/// peers long enough for the spread to matter) while still forgetting stale
/// traffic the next hour.
const USAGE_WINDOW: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Cap on distinct provider ids in [`Usage_ledger`]. Mirrors
/// `Chain_spread::bound`: an unbounded key set would grow with client
/// traffic, and losing usage accounting is benign (the next success re-seeds
/// it), unlike unbounded memory.
const USAGE_LEDGER_MAX_PROVIDERS: usize = 1024;

/// One provider's traffic bucket: when the current window started and the
/// token total recorded inside it.
#[allow(non_camel_case_types)]
struct Usage_bucket {
    start: Instant,
    tokens: u64,
}

/// In-memory observed-usage ledger backing [`Routing_mode::Usage`].
///
/// Magpie's least-used spread, but on AAG's own recorded usage: every
/// answered request contributes `prompt + completion` tokens to its serving
/// provider's bucket on the same success path as `usage_recorder`, and chain
/// resolution ranks members by the bucket ascending. Only the queues'
/// chains are ranked — explicit routes and planner plans never are.
///
/// The ledger is routing state, not reporting state: memory-only, updated
/// best-effort behind one mutex, carried across hot-apply snapshot rebuilds
/// exactly like `Chain_spread`, and never read from SQLite on the hot path.
/// Absent history ranks as zero, so ties keep the configured order.
#[allow(non_camel_case_types)]
pub(crate) struct Usage_ledger {
    window: std::time::Duration,
    buckets: HashMap<String, Usage_bucket>,
}

impl Default for Usage_ledger {
    fn default() -> Self {
        Self::with_window(USAGE_WINDOW)
    }
}

impl Usage_ledger {
    /// `window` is configurable so tests can exercise rollover without
    /// waiting an hour; production always uses [`USAGE_WINDOW`].
    pub(crate) fn with_window(window: std::time::Duration) -> Self {
        Self {
            window,
            buckets: HashMap::new(),
        }
    }

    /// Record one answered request's token traffic for `provider_id`.
    pub(crate) fn record(&mut self, provider_id: &str, prompt_tokens: i64, completion_tokens: i64) {
        let tokens = prompt_tokens.max(0) as u64 + completion_tokens.max(0) as u64;
        self.record_at(provider_id, tokens, Instant::now());
    }

    /// Time-injected core of [`Self::record`]: a bucket past its window
    /// restarts at `now`, otherwise tokens accumulate.
    fn record_at(&mut self, provider_id: &str, tokens: u64, now: Instant) {
        if self.buckets.len() >= USAGE_LEDGER_MAX_PROVIDERS
            && !self.buckets.contains_key(provider_id)
        {
            self.buckets.clear();
        }
        let bucket = self
            .buckets
            .entry(provider_id.to_string())
            .or_insert(Usage_bucket {
                start: now,
                tokens: 0,
            });
        if now.duration_since(bucket.start) >= self.window {
            bucket.start = now;
            bucket.tokens = 0;
        }
        bucket.tokens = bucket.tokens.saturating_add(tokens);
    }

    /// Tokens recorded for `provider_id` inside the live window; expired or
    /// unknown ids rank as zero.
    pub(crate) fn tokens(&self, provider_id: &str) -> u64 {
        self.tokens_at(provider_id, Instant::now())
    }

    fn tokens_at(&self, provider_id: &str, now: Instant) -> u64 {
        self.buckets
            .get(provider_id)
            .filter(|bucket| now.duration_since(bucket.start) < self.window)
            .map_or(0, |bucket| bucket.tokens)
    }
}

/// The core routing engine. Immutable after construction (providers + routes
/// are built from the DB at proxy_start time). Each provider holds its own
/// `CircuitBreaker` internally, which is mutated atomically per-request.
#[derive(Clone)]
pub struct Router {
    providers: HashMap<String, Arc<dyn Provider>>,
    routes: HashMap<String, Arc<Vec<RouteHop>>>,
    /// Legacy global failover queue (checked after explicit routes and
    /// per-provider auto-routing miss). Kept for tests / compatibility.
    queue: Vec<String>,
    /// Per-surface failover queues (Phase 17). A "surface" is an entry
    /// endpoint family: `"claude"` (`/v1/messages`), `"codex"`
    /// (`/v1/responses`), `"chat"` (`/v1/chat/completions`). Each app's
    /// provider chain (from the `providers` table, `in_failover_queue = 1`,
    /// current provider first) is tried in order when no explicit route or
    /// auto-route matches — the requested model passes through unchanged.
    queues: HashMap<String, Vec<String>>,
    /// Exact provenance of every failover-queue member the queue builder
    /// synthesized: runtime provider id -> (app_type, source provider row).
    /// Health identities are tagged from this map; `queue-*` prefixes are
    /// never stripped or guessed.
    queue_sources: HashMap<String, Queue_source>,
    /// The provider id that served the most recent successful request, per
    /// app type. Updated atomically by `complete_hops` / `stream_hops` on
    /// success. Read by the health endpoint for UI flow visualization AND
    /// by `persist_failover_winner` to record the current provider.
    ///
    /// Round 1 review fix: was a single global slot keyed only by
    /// provider id, which made cross-app races possible (a claude_code
    /// request's persist could read a codex winner after a concurrent
    /// codex request overwrote the slot). Scoping by app_type eliminates
    /// the cross-app race.
    pub last_used: Arc<std::sync::Mutex<std::collections::HashMap<String, String>>>,
    /// Per-hop first-byte timeout for streaming requests (ms). Once a
    /// provider has been selected and its stream opened, the router probes
    /// the first chunk within this budget; if nothing arrives, it records a
    /// STREAM TIMEOUT failure and moves to the next hop. Production default
    /// is 10 s (overridable by tests to keep them fast).
    pub first_byte_timeout_ms: u64,
    /// Handle used to persist per-request usage. `None` in tests and in the
    /// legacy entry points that build a Router without a database; recording
    /// is skipped entirely rather than failing the request.
    usage_db: Option<crate::database::Database>,
    /// Optional capability-alias planner (supplier/model-catalog
    /// integration). Installed at snapshot build time; `None` keeps the
    /// legacy exact/queue chain only.
    planner: Option<Arc<dyn crate::proxy::planner::Model_planner>>,
    /// The actual configured model used by Claude Desktop role selectors.
    /// Frozen with this router so request admission never consults mutable DB state.
    desktop_default_model: Option<String>,
    /// Explicit client compatibility roles use operator-configured upstream
    /// queues, independently of capability-tier inference.
    mirror_roles_enabled: bool,
    mirror_role_routes: HashMap<String, HashMap<String, Arc<Vec<RouteHop>>>>,
    /// Bounded session-affinity table (memory-only). Explicit session
    /// markers keep a conversation on its serving provider; requests
    /// without one make no stickiness claim.
    pub(crate) affinity: Arc<crate::proxy::session_affinity::Session_affinity>,
    provider_bindings: HashMap<String, crate::proxy::session_affinity::Provider_binding>,
    /// Opaque per-generation resource identities. Alternate protocols and
    /// model rows using the same credential/origin share one quota resource.
    quota_scopes: HashMap<String, usize>,
    /// How queue-built chains are ordered before the hop walkers try them.
    /// Default `Order` preserves the historical configured-order behavior.
    routing_mode: Routing_mode,
    /// Whether same-tier planned hops order native-protocol upstreams ahead
    /// of cross-protocol ones (`native_protocol_preference` setting, default
    /// on). Explicit manual routes are never reordered by this flag.
    native_protocol_preference: bool,
    /// Per-provider hop weights for `Routing_mode::Weight`, read from
    /// `proxy_providers.weight` at snapshot build. Missing ids default to 1
    /// (queue-synthesized members carry no row, so they spread evenly).
    provider_weights: HashMap<String, u32>,
    /// Rotation/weighted state for queue-built chains; carried across
    /// snapshot generations like `last_used`.
    pub(crate) chain_spread: Arc<std::sync::Mutex<Chain_spread>>,
    /// Observed token traffic per provider for [`Routing_mode::Usage`],
    /// updated on the usage-recording success path. Memory-only, bounded,
    /// and carried across snapshot generations like `chain_spread`.
    pub(crate) usage_ledger: Arc<std::sync::Mutex<Usage_ledger>>,
    pub(crate) model_observations: Arc<std::sync::Mutex<crate::proxy::model_health::Model_health>>,
    admission_gate: Option<Arc<dyn crate::proxy::admission::Admission_gate>>,
}

/// Entry-endpoint surfaces, each backed by its own failover chain.
pub const SURFACE_CLAUDE: &str = "claude";
pub const SURFACE_CODEX: &str = "codex";
pub const SURFACE_CHAT: &str = "chat";
pub const SURFACE_GEMINI: &str = "gemini";

pub(crate) type Mirror_role_routes = HashMap<String, HashMap<String, Vec<RouteHop>>>;

/// Armed terminal-outcome observation for one admitted inference attempt.
/// Armed on the complete/stream paths only after hop-request preparation
/// succeeds, immediately before the provider is invoked; `count_tokens`
/// never arms (a local estimate must not clear or muddy inference health)
/// and a prep rejection never arms (no provider method ran, so there is no
/// actual attempt to judge). Finishing records exactly once; dropping an
/// armed-but-unfinished attempt records a cancelled failure — the attempt
/// ended without terminal evidence (client disconnect, abandonment), which
/// is a terminal non-success per the confirmed criterion, while the breaker
/// still releases its permit as `Cancelled`.
struct Armed_health {
    sink: Arc<std::sync::Mutex<crate::proxy::model_health::Model_health>>,
    identity: Health_identity,
    model: String,
    started: Instant,
}

/// One admitted provider attempt. Dropping a canceled setup or response stream
/// releases its own half-open permit without fabricating success or failure.
#[allow(non_camel_case_types)]
struct Provider_attempt {
    provider: Arc<dyn Provider>,
    permit: Option<Circuit_permit>,
    health: Option<Armed_health>,
}

impl Provider_attempt {
    fn acquire(provider: Arc<dyn Provider>) -> Option<Self> {
        let permit = provider.breaker().acquire()?;
        Some(Self {
            provider,
            permit: Some(permit),
            health: None,
        })
    }

    fn provider(&self) -> &Arc<dyn Provider> {
        &self.provider
    }

    fn settle(&mut self, outcome: Circuit_outcome) {
        if let Some(permit) = self.permit.take() {
            self.provider.breaker().settle(permit, outcome);
        }
    }

    fn arm_health(
        &mut self,
        sink: Arc<std::sync::Mutex<crate::proxy::model_health::Model_health>>,
        identity: Health_identity,
        model: String,
        started: Instant,
    ) {
        self.health = Some(Armed_health {
            sink,
            identity,
            model,
            started,
        });
    }

    /// Record the attempt's terminal outcome exactly once. Any terminal
    /// non-success of an armed inference attempt is a failure; pending or
    /// in-flight states record nothing here.
    fn finish_health(&mut self, success: bool, reason: Option<&'static str>) {
        if let Some(armed) = self.health.take() {
            if let Ok(mut sink) = armed.sink.lock() {
                sink.record_terminal(
                    &armed.identity,
                    &armed.model,
                    success,
                    reason,
                    armed.started.elapsed().as_secs_f64() * 1000.0,
                );
            }
        }
    }
}

impl Drop for Provider_attempt {
    fn drop(&mut self) {
        if self.health.is_some() {
            self.finish_health(false, Some("cancelled"));
        }
        self.settle(Circuit_outcome::Cancelled);
    }
}

/// Some OpenAI-shaped upstreams accept plain Responses requests but reject
/// function tools with a pre-stream HTTP 400. That is a hop capability mismatch,
/// so a finite route should try its next provider.
///
/// Only TRUE tool-capability rejections are retried: the upstream must have
/// been classified as such by `classify_upstream_4xx` (`kind ==
/// ToolCapability`). All other 400s — authentication failures
/// (`refresh_token` invalidated, "sign in again"), malformed user input,
/// and post-commit stream errors — remain terminal, matching the invariant
/// that auth and malformed input must never be replayed or hidden by
/// failover. An unclassified 400 is also terminal: the legacy default.
fn should_failover_tool_capability_rejection(
    request: &ChatCompletionRequest,
    error: &ProviderError,
) -> bool {
    request
        .tools
        .as_ref()
        .map(|tools| !tools.is_empty())
        .unwrap_or(false)
        && error.status == Some(400)
        && error.code == "upstream_http_error"
        && error.stage == "response_status"
        && error.kind == Some(canonical::UpstreamErrorKind::ToolCapability)
}

/// Leading vendor token of a selector id (`claude-opus-4-8` -> `claude`),
/// used to pair legacy route-alias spellings with the unified role names of
/// the same vendor tier when pruning the public model listing.
fn family_prefix(selector: &str) -> Option<String> {
    selector
        .split(['-', '_', '.', '/'])
        .next()
        .map(str::to_lowercase)
        .filter(|token| !token.is_empty())
}

/// Apply the trusted edge's credential scope before every provider dispatch,
/// including pinned routes. Passthrough admission never grants stored-key use.
fn provider_accepts_credential(
    provider: &dyn Provider,
    credential: Option<&canonical::Client_credential>,
) -> bool {
    if provider.requires_client_credential() {
        credential.is_some()
    } else {
        !credential.is_some_and(|credential| credential.passthrough_only)
    }
}

impl Router {
    /// Build a Router from the proxy providers and routes loaded from the DB.
    /// `build_provider` in `providers::mod.rs` must have already been called.
    pub fn new(
        providers: HashMap<String, Arc<dyn Provider>>,
        routes: HashMap<String, Vec<RouteHop>>,
    ) -> Self {
        let provider_bindings = providers
            .keys()
            .map(|id| {
                (
                    id.clone(),
                    crate::proxy::session_affinity::Provider_binding {
                        identity: crate::proxy::session_affinity::next_transport_identity(),
                        native_responses: false,
                    },
                )
            })
            .collect();
        let routes: HashMap<String, Arc<Vec<RouteHop>>> = routes
            .into_iter()
            .map(|(alias, hops)| (alias, Arc::new(hops)))
            .collect();
        Self {
            providers,
            routes,
            queue: Vec::new(),
            queues: HashMap::new(),
            queue_sources: HashMap::new(),
            last_used: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            // Production default: 10 s. A working-but-slow upstream with
            // TTFB > 1.5 s (observed with GLM) must not be killed. This cap
            // only catches 200-then-silence stalls; with providers now
            // propagating HTTP errors eagerly, dead hops already fail over in
            // one RTT without waiting for the timeout.
            first_byte_timeout_ms: 10_000,
            usage_db: None,
            planner: None,
            desktop_default_model: None,
            mirror_roles_enabled: false,
            mirror_role_routes: HashMap::new(),
            affinity: Arc::new(crate::proxy::session_affinity::Session_affinity::default()),
            provider_bindings,
            quota_scopes: HashMap::new(),
            routing_mode: Routing_mode::default(),
            native_protocol_preference: true,
            provider_weights: HashMap::new(),
            chain_spread: Arc::new(std::sync::Mutex::new(Chain_spread::default())),
            usage_ledger: Arc::new(std::sync::Mutex::new(Usage_ledger::default())),
            model_observations: Arc::new(std::sync::Mutex::new(Default::default())),
            admission_gate: None,
        }
    }

    pub(crate) fn with_provider_bindings(
        mut self,
        bindings: HashMap<String, crate::proxy::session_affinity::Provider_binding>,
    ) -> Self {
        self.provider_bindings = bindings;
        self
    }

    pub(crate) fn with_quota_scopes(mut self, scopes: HashMap<String, usize>) -> Self {
        self.quota_scopes = scopes;
        self
    }

    /// The application installs one shared owner when screening enforcement is
    /// explicitly enabled. Existing standalone callers remain unchanged.
    pub fn with_admission_gate(mut self, gate: Arc<dyn crate::proxy::admission::Admission_gate>) -> Self {
        self.admission_gate = Some(gate);
        self
    }

    fn receipt_observation(&self, hop_index: usize, provider: &dyn Provider) -> crate::proxy::routing_receipts::ProviderObservation {
        use crate::proxy::routing_receipts::contract::{safe_reference, AssociationKind, RoutingIdentity};
        let Some(request) = crate::proxy::routing_receipts::current() else { return Default::default(); };
        let identity = self.planner.as_ref().and_then(|planner| planner.routing_identity(provider.id())).unwrap_or_else(|| RoutingIdentity {
            provider_ref: safe_reference(provider.id()), supplier_ref: None, endpoint_ref: None,
            association: if self.queue.iter().any(|id| id == provider.id())
                || self.queues.values().any(|ids| ids.iter().any(|id| id == provider.id())) {
                AssociationKind::LegacyQueue
            } else { AssociationKind::Unmapped },
        });
        request.hop(hop_index, identity, provider.wire_protocol())
    }

    fn quota_scope_failed(
        &self,
        hop: &RouteHop,
        failed: &std::collections::HashSet<usize>,
    ) -> bool {
        self.quota_scopes
            .get(&hop.provider)
            .is_some_and(|scope| failed.contains(scope))
    }

    fn record_quota_failure(
        &self,
        hop: &RouteHop,
        error: &ProviderError,
        failed: &mut std::collections::HashSet<usize>,
    ) {
        if error.retriable && error.kind == Some(canonical::UpstreamErrorKind::QuotaExceeded) {
            if let Some(scope) = self.quota_scopes.get(&hop.provider) {
                failed.insert(*scope);
            }
        }
    }

    pub(crate) fn with_desktop_default_model(mut self, model: Option<String>) -> Self {
        self.desktop_default_model = model;
        self
    }

    pub(crate) fn desktop_default_model(&self) -> Option<&str> {
        self.desktop_default_model.as_deref()
    }

    pub(crate) fn with_mirror_role_routes(
        mut self,
        enabled: bool,
        routes: Mirror_role_routes,
    ) -> Self {
        self.mirror_roles_enabled = enabled;
        self.mirror_role_routes = routes
            .into_iter()
            .map(|(surface, roles)| {
                let roles = roles
                    .into_iter()
                    .map(|(alias, hops)| {
                        let mut seen = std::collections::HashSet::new();
                        let hops = hops
                            .into_iter()
                            .filter(|hop| {
                                self.providers.get(&hop.provider).is_some_and(|provider| {
                                    provider.enabled()
                                        && !provider.requires_client_credential()
                                        && provider.models().contains(&hop.model)
                                }) && seen.insert((hop.provider.clone(), hop.model.clone()))
                            })
                            .collect();
                        (alias, Arc::new(hops))
                    })
                    .collect();
                (surface, roles)
            })
            .collect();
        self
    }

    pub(crate) fn mirror_roles_enabled(&self) -> bool {
        self.mirror_roles_enabled
    }

    /// The picker receives canonical compatibility IDs. Bare role words stay
    /// accepted for older CLI configuration but are not duplicate picker rows.
    pub(crate) fn mirror_role_models(
        &self,
        surface: &str,
        pinned_provider: Option<&str>,
    ) -> Vec<String> {
        if !self.mirror_roles_enabled {
            return Vec::new();
        }
        let Some(roles) = self.mirror_role_routes.get(surface) else {
            return Vec::new();
        };
        let mut models: Vec<_> = roles
            .keys()
            .filter(|model| model.starts_with("claude-") || model.starts_with("gpt-"))
            .filter(|model| match surface {
                SURFACE_CLAUDE => model.starts_with("claude-"),
                SURFACE_CODEX => model.starts_with("gpt-"),
                _ => true,
            })
            .filter(|model| {
                self.resolve_model(surface, model, Default::default())
                    .is_ok_and(|hops| {
                        hops.iter().any(|hop| {
                            pinned_provider.is_none_or(|pin| pin == hop.provider)
                                && self.providers.get(&hop.provider).is_some_and(|provider| {
                                    provider.enabled()
                                        && !provider.requires_client_credential()
                                        && provider.models().contains(&hop.model)
                                })
                        })
                    })
            })
            .cloned()
            .collect();
        models.sort();
        models
    }

    /// Exact, enabled model offers usable without forwarding client credentials.
    /// The planner, explicit routes and optional pin belong to this same snapshot.
    pub(crate) fn desktop_models(&self, pinned_provider: Option<&str>) -> Vec<String> {
        let candidates: std::collections::BTreeSet<_> = self
            .providers
            .values()
            .filter(|provider| provider.enabled() && !provider.requires_client_credential())
            .flat_map(|provider| provider.models().iter().cloned())
            .filter(|model| !model.trim().is_empty())
            .collect();
        candidates
            .into_iter()
            .filter(|model| {
                let Ok(hops) = self.resolve_model(SURFACE_CLAUDE, model, Default::default()) else {
                    return false;
                };
                let static_hops: Vec<_> = hops
                    .iter()
                    .filter(|hop| {
                        pinned_provider.is_none_or(|pinned| pinned == hop.provider)
                            && self.providers.get(&hop.provider).is_some_and(|provider| {
                                provider.enabled()
                                    && !provider.requires_client_credential()
                                    && provider.models().contains(&hop.model)
                            })
                    })
                    .collect();
                // Snapshot planners already enforce exact offered names. Retain
                // that guarantee for legacy routers built without a planner too.
                !static_hops.is_empty() && static_hops.iter().all(|hop| hop.model == *model)
            })
            .collect()
    }

    /// Model choices this snapshot can serve without an inbound upstream credential.
    /// Explicit legacy aliases remain valid when their static target is configured
    /// on the route, even if that provider has no separate model inventory.
    pub(crate) fn credentialless_models(
        &self,
        surface: &str,
        pinned_provider: Option<&str>,
    ) -> Vec<String> {
        let offered: std::collections::BTreeSet<_> = self
            .providers
            .values()
            .filter(|provider| provider.enabled())
            .flat_map(|provider| provider.models().iter().cloned())
            .filter(|model| !model.trim().is_empty())
            .collect();
        let candidates: std::collections::BTreeSet<_> = self
            .providers
            .values()
            .filter(|provider| provider.enabled() && !provider.requires_client_credential())
            .flat_map(|provider| provider.models().iter().cloned())
            .chain(self.routes.keys().cloned())
            .chain(self.mirror_role_models(surface, pinned_provider))
            .filter(|model| !model.trim().is_empty())
            .collect();
        candidates
            .into_iter()
            .filter(|model| {
                let Ok(hops) = self.resolve_model(surface, model, Default::default()) else {
                    return false;
                };
                let static_hops: Vec<_> = hops
                    .iter()
                    .filter(|hop| {
                        !hop.model.trim().is_empty()
                            && pinned_provider.is_none_or(|pinned| pinned == hop.provider)
                            && self.providers.get(&hop.provider).is_some_and(|provider| {
                                provider.enabled() && !provider.requires_client_credential()
                            })
                    })
                    .collect();
                !static_hops.is_empty()
                    && (!offered.contains(model)
                        || static_hops.iter().all(|hop| hop.model == *model))
            })
            .collect()
    }

    pub(crate) fn offers_exact_model(&self, id: &str) -> bool {
        self.providers
            .values()
            .any(|provider| provider.enabled() && provider.models().iter().any(|model| model == id))
    }

    /// Next lower-tier mirror role of the same vendor family that still has
    /// admitted supply. The tier chain follows `CLIENT_ROLE_SELECTORS`
    /// (fable→opus→sonnet→haiku; sol→terra→luna); cross-vendor degradation
    /// never happens.
    fn degraded_role_supply(
        &self,
        surface: &str,
        requested_role: &str,
    ) -> Option<(String, Arc<Vec<RouteHop>>)> {
        let roles = self.mirror_role_routes.get(surface)?;
        let requested_tier = crate::proxy::planner::client_role_tier(requested_role)?;
        let requested_family = family_prefix(requested_role)?;
        let mut candidates: Vec<(&String, u8)> = roles
            .keys()
            .filter_map(|role| {
                let tier = crate::proxy::planner::client_role_tier(role)?;
                (tier < requested_tier
                    && family_prefix(role).is_some_and(|family| family == requested_family))
                    .then_some((role, tier))
            })
            .collect();
        candidates.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(right.0)));
        candidates
            .into_iter()
            .find_map(|(role, _)| {
                let hops = roles.get(role)?;
                (!hops.is_empty()).then(|| (role.clone(), Arc::clone(hops)))
            })
    }

    /// Public discovery advertises only selectors that resolve to an enabled
    /// credentialless chain in this captured generation and optional pin.
    pub(crate) fn catalog_models(
        &self,
        surface: &str,
        pinned_provider: Option<&str>,
    ) -> Vec<String> {
        let mut candidates: std::collections::BTreeSet<_> = self
            .credentialless_models(surface, pinned_provider)
            .into_iter()
            .collect();
        if let Some(planner) = &self.planner {
            let aliases = crate::proxy::planner::CAPABILITY_ALIAS_SELECTORS
                .iter()
                .flat_map(|(alias, _)| [(*alias).to_string(), format!("alias:{alias}")]);
            let roles = crate::proxy::planner::CLIENT_ROLE_SELECTORS
                .iter()
                .map(|(role, _)| (*role).to_string());
            for selector in aliases.chain(roles) {
                if matches!(
                    planner.resolve(&selector, Default::default()),
                    Ok(crate::proxy::planner::Resolution::Alias { .. })
                        | Ok(crate::proxy::planner::Resolution::Precise { .. })
                ) {
                    candidates.insert(selector);
                }
            }
        }
        // Unify the client-facing vocabulary: a legacy route-alias spelling of
        // a tier a unified role name already covers is suppressed unless the
        // id is genuinely offered as an exact model. Requests for the legacy
        // spelling still resolve; only the listing stops advertising both
        // generations of the same tier.
        let offered: std::collections::BTreeSet<String> = self
            .providers
            .values()
            .filter(|provider| provider.enabled())
            .flat_map(|provider| provider.models().iter().cloned())
            .collect();
        let covered_roles: std::collections::BTreeSet<(String, u8)> =
            crate::proxy::planner::CLIENT_ROLE_SELECTORS
                .iter()
                .filter(|entry| candidates.contains(entry.0))
                .filter_map(|entry| Some((family_prefix(entry.0)?, entry.1)))
                .collect();
        candidates.retain(|selector| {
            if offered.contains(selector.as_str()) || !self.routes.contains_key(selector) {
                return true;
            }
            match family_prefix(selector)
                .and_then(|family| {
                    crate::services::model_catalog::tiers::capability_tier_for_alias(selector)
                        .map(|tier| (family, tier))
                }) {
                Some(key) => !covered_roles.contains(&key),
                None => true,
            }
        });
        candidates
            .into_iter()
            .filter(|selector| {
                self.resolve_model(surface, selector, Default::default())
                    .is_ok_and(|hops| {
                        hops.iter().any(|hop| {
                            !hop.model.trim().is_empty()
                                && pinned_provider.is_none_or(|pin| pin == hop.provider)
                                && self.providers.get(&hop.provider).is_some_and(|provider| {
                                    provider.enabled() && !provider.requires_client_credential()
                                })
                        })
                    })
            })
            .collect()
    }

    /// Select the failover-queue spread mode. Defaults to
    /// [`Routing_mode::Order`] when never set.
    pub fn with_routing_mode(mut self, mode: Routing_mode) -> Self {
        self.routing_mode = mode;
        self
    }

    /// Toggle same-tier native-protocol preference (setting
    /// `native_protocol_preference`; disabled only by an explicit `"0"`).
    pub fn with_native_protocol_preference(mut self, enabled: bool) -> Self {
        self.native_protocol_preference = enabled;
        self
    }

    /// Supply hop weights for [`Routing_mode::Weight`], keyed by provider
    /// id. Providers absent from the map spread at weight 1.
    pub fn with_provider_weights(mut self, weights: HashMap<String, u32>) -> Self {
        self.provider_weights = weights;
        self
    }

    /// Attach the database used for per-request usage accounting.
    ///
    /// Optional: without it the router serves requests normally and records
    /// nothing. Usage must never be a precondition for proxying, so the
    /// recording path degrades to a no-op rather than failing a request.
    pub fn with_usage_db(mut self, db: crate::database::Database) -> Self {
        self.usage_db = Some(db);
        self
    }

    /// Attach the homepage failover queue (checked after explicit routes
    /// and per-provider auto-routing miss).
    pub fn with_queue(mut self, queue: Vec<String>) -> Self {
        self.queue = queue;
        self
    }

    /// Attach the per-surface failover queues. Keys are surface ids
    /// (`SURFACE_CLAUDE` / `SURFACE_CODEX` / `SURFACE_CHAT`); values are
    /// ordered provider ids (chain head first).
    pub fn with_queues(mut self, queues: HashMap<String, Vec<String>>) -> Self {
        self.queues = queues;
        self
    }

    /// Attach the exact failover-queue provenance emitted by the queue
    /// builder, so health observations key on the stable
    /// `(app_type, source provider row)` identity instead of synthesized
    /// runtime ids.
    pub fn with_queue_sources(mut self, sources: HashMap<String, Queue_source>) -> Self {
        self.queue_sources = sources;
        self
    }

    /// Attach the capability-alias planner. Without one, alias names fall
    /// through to the legacy exact/queue chain unchanged.
    pub fn with_planner(mut self, planner: Arc<dyn crate::proxy::planner::Model_planner>) -> Self {
        self.planner = Some(planner);
        self
    }

    /// Resolve a model name to its route hops. If no explicit route exists,
    /// Resolve a model alias to a failover chain: explicit route first, then
    /// check if any enabled provider directly serves the model (auto-route).
    ///
    /// Hop order is the configured order (route editor / queue sort_index) —
    /// deliberately NOT reordered by recent success. cc-switch parity: its
    /// failover queue is static and the "sticky switch" on failover success
    /// only updates the current-provider marker, never the queue order. The
    /// fast path comes from the circuit breakers skipping dead hops at zero
    /// cost, not from reordering. The one exception is the user-selected
    /// [`Routing_mode`] (`rotate` / `weight` / `usage`), which spreads
    /// queue-built chains; explicit routes above are never reordered.
    pub fn resolve_route(&self, model: &str) -> Option<Arc<Vec<RouteHop>>> {
        self.resolve_route_with_origin(model).0
    }

    /// `resolve_route` plus whether the returned chain is queue-built (the
    /// only chains health deferral may partition).
    fn resolve_route_with_origin(&self, model: &str) -> (Option<Arc<Vec<RouteHop>>>, bool) {
        if let Some(hops) = self.routes.get(model) {
            return (Some(Arc::clone(hops)), false);
        }
        let matches = self.matching_model_hops(model, &self.queue);
        if !matches.is_empty() {
            return (Some(Arc::new(self.spread_chain(matches))), true);
        }
        // Homepage failover queue: last-chance chain, model passed through.
        if !self.queue.is_empty() {
            return (
                Some(Arc::new(
                    self.spread_chain(
                        self.queue
                            .iter()
                            .map(|pid| RouteHop {
                                provider: pid.clone(),
                                model: model.into(),
                            })
                            .collect(),
                    ),
                )),
                true,
            );
        }
        (None, false)
    }

    /// Surface-aware resolution: same precedence as `resolve_route`, but the
    /// last-chance chain is the surface's per-app queue instead of the global
    /// one. `/v1/messages` falls back to the Claude chain, `/v1/responses`
    /// to the Codex chain, `/v1/chat/completions` to the copilot+pi chain.
    pub fn resolve_route_for(&self, surface: &str, model: &str) -> Option<Arc<Vec<RouteHop>>> {
        self.resolve_route_for_with_origin(surface, model).0
    }

    /// `resolve_route_for` plus whether the returned chain is queue-built.
    fn resolve_route_for_with_origin(
        &self,
        surface: &str,
        model: &str,
    ) -> (Option<Arc<Vec<RouteHop>>>, bool) {
        if let Some(hops) = self.routes.get(model) {
            return (Some(Arc::clone(hops)), false);
        }
        let priority = self
            .queues
            .get(surface)
            .filter(|queue| !queue.is_empty())
            .unwrap_or(&self.queue);
        let matches = self.matching_model_hops(model, priority);
        if !matches.is_empty() {
            return (Some(Arc::new(self.spread_chain(matches))), true);
        }
        if let Some(chain) = self.queues.get(surface) {
            if !chain.is_empty() {
                return (
                    Some(Arc::new(
                        self.spread_chain(
                            chain
                                .iter()
                                .map(|pid| RouteHop {
                                    provider: pid.clone(),
                                    model: model.into(),
                                })
                                .collect(),
                        ),
                    )),
                    true,
                );
            }
        }
        // Legacy global queue as the very last resort.
        if !self.queue.is_empty() {
            return (
                Some(Arc::new(
                    self.spread_chain(
                        self.queue
                            .iter()
                            .map(|pid| RouteHop {
                                provider: pid.clone(),
                                model: model.into(),
                            })
                            .collect(),
                    ),
                )),
                true,
            );
        }
        (None, false)
    }

    /// Manual routes may order or restrict planned candidates, never substitute
    /// an offered exact model or introduce an ineligible alias candidate.
    pub fn resolve_request(
        &self,
        surface: &str,
        req: &ChatCompletionRequest,
    ) -> Result<Arc<Vec<RouteHop>>, ProviderError> {
        self.resolve_request_with_origin(surface, req)
            .map(|(hops, _)| hops)
    }

    /// `resolve_request` plus whether the resolved chain is queue-built (and
    /// therefore eligible for failure-cooldown deferral downstream).
    fn resolve_request_with_origin(
        &self,
        surface: &str,
        req: &ChatCompletionRequest,
    ) -> Result<(Arc<Vec<RouteHop>>, bool), ProviderError> {
        self.resolve_model_with_origin(
            surface,
            &req.model,
            crate::proxy::planner::request_features(req),
        )
    }

    /// Read the next routing strategy without advancing rotation/weight state,
    /// issuing a request, changing affinity, or touching a circuit breaker.
    pub(crate) fn preview_model(&self, surface: &str, model: &str,
        features: crate::proxy::planner::Request_features, pinned: Option<&str>,
    ) -> Result<Arc<Vec<RouteHop>>, ProviderError> {
        let mut preview = self.clone();
        let spread = self.chain_spread.lock().unwrap_or_else(|error| error.into_inner()).clone();
        preview.chain_spread = Arc::new(std::sync::Mutex::new(spread));
        let hops = preview.resolve_model(surface, model, features)?;
        Ok(match pinned {
            Some(id) => Arc::new(hops.iter().filter(|hop| hop.provider == id).cloned().collect()),
            None => hops,
        })
    }

    /// Tagged health identity of one runtime provider id: a failover-queue
    /// member resolves to its exact `(app_type, source provider row)`
    /// provenance, everything else is a direct `Proxy` id.
    fn health_identity(&self, runtime_id: &str) -> Health_identity {
        match self.queue_sources.get(runtime_id) {
            Some(source) => source.identity(),
            None => Health_identity::Proxy {
                id: runtime_id.to_string(),
            },
        }
    }

    fn provider_failure_health(&self, runtime_id: &str) -> Provider_failure_health {
        let identity = self.health_identity(runtime_id);
        self.model_observations
            .lock()
            .map(|health| health.provider_view(&identity).into())
            .unwrap_or_default()
    }

    /// Translate identity-keyed unavailable (provider, model) observations
    /// into the runtime ids the planner's planned hops carry. Ids renamed or
    /// removed since an observation was recorded surface here only when
    /// this snapshot still contains them.
    fn planner_model_exclusions(&self) -> std::collections::HashSet<(String, String)> {
        let unavailable = match self.model_observations.lock() {
            Ok(health) => health.unavailable_models(),
            Err(_) => return Default::default(),
        };
        if unavailable.is_empty() {
            return Default::default();
        }
        let mut runtime_ids: HashMap<Health_identity, Vec<&String>> = HashMap::new();
        for id in self.providers.keys().chain(self.queue_sources.keys()) {
            runtime_ids
                .entry(self.health_identity(id))
                .or_default()
                .push(id);
        }
        let mut excluded = std::collections::HashSet::new();
        for (identity, model) in unavailable {
            if let Some(ids) = runtime_ids.get(&identity) {
                for id in ids {
                    excluded.insert(((*id).clone(), model.clone()));
                }
            }
        }
        excluded
    }

    /// Runtime ids of chain members whose provider identity failed its
    /// latest terminal attempt inside the existing failure cooldown.
    fn cooldown_identities(&self) -> std::collections::HashSet<Health_identity> {
        let Ok(health) = self.model_observations.lock() else {
            return Default::default();
        };
        self.providers
            .keys()
            .map(|id| self.health_identity(id))
            .filter(|identity| health.provider_view(identity).in_cooldown)
            .collect()
    }

    pub(crate) fn model_health(&self, provider: &str, model: &str) -> crate::proxy::model_health::Model_health_view {
        let identity = self.health_identity(provider);
        self.model_observations.lock().map(|health| health.view(&identity, model)).unwrap_or_default()
    }

    /// The resolution core behind [`Self::resolve_request`]; the token
    /// counting path reuses it with default features.
    pub fn resolve_model(
        &self,
        surface: &str,
        model: &str,
        features: crate::proxy::planner::Request_features,
    ) -> Result<Arc<Vec<RouteHop>>, ProviderError> {
        self.resolve_model_with_origin(surface, model, features)
            .map(|(hops, _)| hops)
    }

    fn resolve_model_with_origin(
        &self,
        surface: &str,
        model: &str,
        features: crate::proxy::planner::Request_features,
    ) -> Result<(Arc<Vec<RouteHop>>, bool), ProviderError> {
        let resolution = self
            .planner
            .as_ref()
            .map(|planner| {
                if self.routes.contains_key(model) { return planner.resolve(model, features); }
                let excluded = self.planner_model_exclusions();
                planner.resolve_excluding(model, features, &excluded)
            })
            .transpose()?;
        let offered_exact = self.providers.values().any(|provider| {
            provider.enabled() && provider.models().iter().any(|offered| offered == model)
        }) || matches!(
            resolution.as_ref(),
            Some(crate::proxy::planner::Resolution::Precise { .. })
        );
        if self.mirror_roles_enabled
            && !offered_exact
            && !self.routes.contains_key(model)
            && !matches!(
                resolution.as_ref(),
                Some(crate::proxy::planner::Resolution::Alias { .. })
            )
        {
            if let Some(hops) = self
                .mirror_role_routes
                .get(surface)
                .and_then(|roles| roles.get(model))
            {
                if hops.is_empty() {
                    // Seamless degradation: serve the requested role from the
                    // next lower-tier role of the same vendor that still has
                    // supply, and record the degradation for the surface to
                    // relay. Only a family with no supplied tier at all errors.
                    if let Some((_fallback_role, hops)) = self.degraded_role_supply(surface, model)
                    {
                        crate::proxy::request_timeline::observe_role_degradation(model);
                        return Ok((hops, false));
                    }
                    return Err(canonical::make_provider_error(
                        "mirror_role_supply_unavailable",
                        "route_resolution",
                        Some(503),
                        Some(false),
                        None,
                    ));
                }
                return Ok((Arc::clone(hops), false));
            }
        }
        if let Some(resolution) = resolution {
            use crate::proxy::planner::Resolution;
            match resolution {
                Resolution::Precise {
                    model: literal,
                    hops,
                } => {
                    let hops = self.restrict_planned_route(surface, model, hops);
                    if hops.is_empty() {
                        return Err(crate::proxy::planner::precise_unavailable_error(&literal));
                    }
                    return Ok((hops, false));
                }
                Resolution::Alias { alias, hops, .. } => {
                    let hops = self.restrict_planned_route(surface, model, hops);
                    let hops = if self.routes.contains_key(model) { hops } else {
                        self.model_observations.lock().map(|health| Arc::new(
                            crate::proxy::planner::diverse_fallback_order(
                                health.prefer_with(&hops, |id| self.health_identity(id)),
                                |hop| (hop.provider.as_str(), hop.model.as_str())))).unwrap_or(hops)
                    };
                    if hops.is_empty() {
                        return Err(crate::proxy::planner::alias_no_candidates_error(&alias));
                    }
                    return Ok((hops, false));
                }
                Resolution::Exact(name)
                    if !model.starts_with("precise:") && !model.starts_with("alias:") =>
                {
                    let (hops, queue_built) =
                        self.resolve_route_for_with_origin(surface, &name);
                    return hops.ok_or_else(|| {
                        canonical::make_provider_error(
                            "no_route",
                            "route_resolution",
                            Some(404),
                            Some(false),
                            None,
                        )
                    }).map(|hops| (hops, queue_built));
                }
                Resolution::Exact(_) => {}
            }
        }
        if let Some(literal) = model.strip_prefix("precise:") {
            let literal = literal.trim();
            if literal.is_empty() {
                return Err(crate::proxy::planner::selector_invalid_error());
            }
            // `precise:` means that exact model on any provider that offers
            // it — the provider choice is queue-built, so the spread mode
            // applies; the requested model never changes.
            let hops = self.spread_chain(self.matching_model_hops(literal, &self.queue));
            if hops.is_empty() {
                return Err(crate::proxy::planner::precise_unavailable_error(literal));
            }
            return Ok((Arc::new(hops), true));
        }
        if let Some(alias) = model.strip_prefix("alias:") {
            return Err(crate::proxy::planner::alias_unknown_error(alias));
        }
        let (hops, queue_built) = self.resolve_route_for_with_origin(surface, model);
        hops.ok_or_else(|| {
            canonical::make_provider_error(
                "no_route",
                "route_resolution",
                Some(404),
                Some(false),
                None,
            )
        })
        .map(|hops| (hops, queue_built))
    }

    fn restrict_planned_route(
        &self,
        surface: &str,
        model: &str,
        mut planned: Vec<crate::proxy::planner::Planned_hop>,
    ) -> Arc<Vec<RouteHop>> {
        if let Some(manual) = self.routes.get(model) {
            return Arc::new(
                manual
                    .iter()
                    .filter(|hop| {
                        planned.iter().any(|candidate| {
                            candidate.provider == hop.provider && candidate.model == hop.model
                        })
                    })
                    .cloned()
                    .collect(),
            );
        }
        // Native-protocol affinity outranks configured priority inside one
        // capability tier: an anthropic hop serves the Claude surface with
        // verbatim bodies, typed errors, and untouched SSE, so it leads its
        // tier ahead of cheaper cross-protocol supply (and likewise for the
        // Codex/Gemini/chat surfaces). The capability-tier boundary itself is
        // never crossed, cross-protocol supply stays in the chain behind the
        // native hops, and explicit manual routes above keep their exact
        // order.
        let preferred = match surface {
            SURFACE_CLAUDE => "anthropic",
            SURFACE_CODEX => "openai-responses",
            SURFACE_GEMINI => "gemini",
            SURFACE_CHAT => "openai-chat",
            _ => "unknown",
        };
        if preferred != "unknown" && self.native_protocol_preference {
            let mut start = 0;
            while start < planned.len() {
                let mut end = start + 1;
                while end < planned.len() && planned[end].tier == planned[start].tier {
                    end += 1;
                }
                planned[start..end].sort_by(|left, right| {
                    let cross_protocol = |hop: &crate::proxy::planner::Planned_hop| {
                        !self
                            .providers
                            .get(&hop.provider)
                            .is_some_and(|provider| provider.wire_protocol() == preferred)
                    };
                    cross_protocol(left)
                        .cmp(&cross_protocol(right))
                        .then_with(|| left.priority.cmp(&right.priority))
                        .then_with(|| left.model.cmp(&right.model))
                });
                start = end;
            }
        }
        crate::proxy::planner::planned_hops(planned)
    }

    fn serving_plan(
        &self,
        req: &ChatCompletionRequest,
        hop: &RouteHop,
    ) -> crate::diagnostics::Plan_trace {
        use crate::proxy::planner::Resolution;
        let resolved = self.planner.as_ref().and_then(|planner| {
            planner
                .resolve(&req.model, crate::proxy::planner::request_features(req))
                .ok()
        });
        if self.mirror_roles_enabled
            && !matches!(
                resolved.as_ref(),
                Some(Resolution::Precise { .. } | Resolution::Alias { .. })
            )
            && !self
                .providers
                .values()
                .any(|provider| provider.enabled() && provider.models().contains(&req.model))
            && self.mirror_role_routes.values().any(|roles| {
                roles.get(&req.model).is_some_and(|hops| {
                    hops.iter().any(|candidate| {
                        candidate.provider == hop.provider && candidate.model == hop.model
                    })
                })
            })
        {
            return crate::diagnostics::Plan_trace {
                kind: Some("alias".into()),
                tier: None,
                downgraded: None,
                basis: Some("operator_queue".into()),
                reason: Some("configured_mirror_role".into()),
            };
        }
        let (kind, planned) = match resolved {
            Some(Resolution::Precise { hops, .. }) => ("precise", hops),
            Some(Resolution::Alias { hops, .. }) => ("alias", hops),
            _ => ("exact", Vec::new()),
        };
        let selected = planned
            .iter()
            .find(|candidate| candidate.provider == hop.provider && candidate.model == hop.model);
        crate::diagnostics::Plan_trace {
            kind: Some(kind.to_string()),
            tier: selected.and_then(|candidate| candidate.tier),
            downgraded: Some(selected.is_some_and(|candidate| candidate.downgraded)),
            basis: Some(
                selected
                    .map_or("manual_route", |candidate| candidate.basis.as_str())
                    .to_string(),
            ),
            reason: Some(
                selected
                    .map_or("explicit_route", |candidate| candidate.reason.as_str())
                    .to_string(),
            ),
        }
    }

    fn pinned_hops(
        &self,
        surface: &str,
        provider_id: &str,
        req: &ChatCompletionRequest,
    ) -> Result<Arc<Vec<RouteHop>>, ProviderError> {
        self.pinned_hops_with_origin(surface, provider_id, req)
            .map(|(hops, _)| hops)
    }

    fn pinned_hops_with_origin(
        &self,
        surface: &str,
        provider_id: &str,
        req: &ChatCompletionRequest,
    ) -> Result<(Arc<Vec<RouteHop>>, bool), ProviderError> {
        let (resolved, queue_built) = self.resolve_request_with_origin(surface, req)?;
        let hops: Vec<RouteHop> = resolved
            .iter()
            .filter(|hop| hop.provider == provider_id)
            .cloned()
            .collect();
        if hops.is_empty() {
            return Err(canonical::make_provider_error(
                "pinned_provider_ineligible",
                "route_resolution",
                Some(409),
                Some(false),
                None,
            ));
        }
        Ok((Arc::new(hops), queue_built))
    }

    fn source_target(
        &self,
        hop: &RouteHop,
    ) -> Option<crate::proxy::session_affinity::Source_target> {
        self.provider_bindings
            .get(&hop.provider)
            .filter(|binding| binding.native_responses)
            .map(|binding| crate::proxy::session_affinity::Source_target {
                provider: hop.provider.clone(),
                model: hop.model.clone(),
                identity: binding.identity,
            })
    }

    fn constrain_source(
        &self,
        req: &ChatCompletionRequest,
        hops: Arc<Vec<RouteHop>>,
    ) -> Result<Arc<Vec<RouteHop>>, ProviderError> {
        let Some(target) = self.affinity.source_target(req)? else {
            return Ok(hops);
        };
        let bound: Vec<RouteHop> = hops
            .iter()
            .filter(|hop| self.source_target(hop).as_ref() == Some(&target))
            .cloned()
            .collect();
        if bound.is_empty() {
            return Err(crate::proxy::session_affinity::source_error(
                "responses_source_unavailable",
            ));
        }
        Ok(Arc::new(bound))
    }

    fn request_for_hop<'a>(
        &self,
        req: &'a ChatCompletionRequest,
        hop: &RouteHop,
    ) -> Result<std::borrow::Cow<'a, ChatCompletionRequest>, ProviderError> {
        if req.source_gemini_request.is_some()
            && self
                .providers
                .get(&hop.provider)
                .is_some_and(|provider| provider.wire_protocol() != "gemini")
        {
            return crate::proxy::gemini::request_for_foreign_hop(req).map(std::borrow::Cow::Owned);
        }
        if self
            .provider_bindings
            .get(&hop.provider)
            .is_some_and(|binding| binding.native_responses)
        {
            return Ok(std::borrow::Cow::Borrowed(req));
        }
        let Some(source) = &req.source_responses_request else {
            return Ok(std::borrow::Cow::Borrowed(req));
        };
        let mut strict = crate::proxy::converter::try_responses_to_chat(source)?;
        let baseline = crate::proxy::converter::responses_to_chat(source);
        let prefix_len = req.messages.len().saturating_sub(baseline.messages.len());
        if req.messages.len() < baseline.messages.len()
            || req.messages[..prefix_len]
                .iter()
                .any(|message| message.role != "system")
            || serde_json::to_value(&req.messages[prefix_len..]).ok()
                != serde_json::to_value(&baseline.messages).ok()
        {
            return Err(canonical::make_provider_error(
                "responses_state_unsupported",
                "request_conversion",
                Some(400),
                Some(false),
                None,
            ));
        }
        let mut messages = req.messages[..prefix_len].to_vec();
        messages.append(&mut strict.messages);
        strict.messages = messages;
        strict.model = req.model.clone();
        strict.stream = req.stream;
        strict.temperature = req.temperature;
        strict.max_tokens = req.max_tokens;
        strict.request_id = req.request_id.clone();
        strict.client_credential = req.client_credential.clone();
        Ok(std::borrow::Cow::Owned(strict))
    }

    /// Automatic same-model routes use configured queue priority, then stable
    /// provider id. Explicit routes/pins remain authoritative and no model
    /// mapping is inferred from a provider's position in another queue.
    fn matching_model_hops(&self, model: &str, priority: &[String]) -> Vec<RouteHop> {
        let mut ids: Vec<&String> = self
            .providers
            .iter()
            .filter(|(_, provider)| {
                provider.enabled() && provider.models().iter().any(|candidate| candidate == model)
            })
            .map(|(id, _)| id)
            .collect();
        ids.sort_by_key(|id| {
            (
                priority
                    .iter()
                    .position(|candidate| candidate == *id)
                    .unwrap_or(usize::MAX),
                id.as_str(),
            )
        });
        ids.into_iter()
            .map(|id| RouteHop {
                provider: id.clone(),
                model: model.into(),
            })
            .collect()
    }

    /// Reorder one queue-built candidate chain per the configured
    /// [`Routing_mode`], first partitioning members whose provider identity
    /// failed its latest terminal attempt inside the existing failure
    /// cooldown behind the ready members:
    ///
    /// - all members ready → the spread behaves exactly as before;
    /// - a nonempty ready bucket is spread once under the configured mode and
    ///   the deferred members follow in their existing fallback order;
    /// - all members deferred → the full chain is spread once (a sole or
    ///   all-cooling chain keeps its ordinary behavior).
    ///
    /// No spread state advances more than once per resolve either way.
    /// Single-hop chains and `Order` mode return the list untouched; a
    /// poisoned state lock also degrades to the configured order rather than
    /// failing resolution.
    fn spread_chain(&self, hops: Vec<RouteHop>) -> Vec<RouteHop> {
        let (ready, deferred) = self.partition_cooldown_members(hops);
        if deferred.is_empty() {
            return self.apply_spread(ready);
        }
        if ready.is_empty() {
            return self.apply_spread(deferred);
        }
        let mut order = self.apply_spread(ready);
        order.extend(deferred);
        order
    }

    /// Stable partition of one candidate chain into (ready, deferred) by
    /// provider failure cooldown. Relative order inside each bucket is
    /// preserved; nothing is mutated.
    fn partition_cooldown_members(
        &self,
        hops: Vec<RouteHop>,
    ) -> (Vec<RouteHop>, Vec<RouteHop>) {
        if hops.len() < 2 {
            return (hops, Vec::new());
        }
        let cooldown = self.cooldown_identities();
        if cooldown.is_empty() {
            return (hops, Vec::new());
        }
        let mut ready = Vec::with_capacity(hops.len());
        let mut deferred = Vec::new();
        for hop in hops {
            if cooldown.contains(&self.health_identity(&hop.provider)) {
                deferred.push(hop);
            } else {
                ready.push(hop);
            }
        }
        (ready, deferred)
    }

    /// Stable post-affinity re-partition of a resolved queue-built chain:
    /// session-affinity promotion may have moved a cooling provider to the
    /// front; ready members move back ahead of it without a second spread
    /// advance and without losing the promoted member from the chain.
    fn defer_cooldown_members_stable(&self, hops: Arc<Vec<RouteHop>>) -> Arc<Vec<RouteHop>> {
        let Some(first) = hops.first() else {
            return hops;
        };
        let cooldown = self.cooldown_identities();
        if cooldown.is_empty() || !cooldown.contains(&self.health_identity(&first.provider)) {
            return hops;
        }
        let mut ready = Vec::with_capacity(hops.len());
        let mut deferred = Vec::new();
        for hop in hops.iter() {
            if cooldown.contains(&self.health_identity(&hop.provider)) {
                deferred.push(hop.clone());
            } else {
                ready.push(hop.clone());
            }
        }
        if ready.is_empty() {
            return hops;
        }
        ready.extend(deferred);
        Arc::new(ready)
    }

    /// The configured spread itself, over one already-partitioned bucket.
    fn apply_spread(&self, hops: Vec<RouteHop>) -> Vec<RouteHop> {
        if self.routing_mode == Routing_mode::Order || hops.len() < 2 {
            return hops;
        }
        // `usage` ranks by observed traffic and mutates nothing: it needs
        // only one ledger lock (no rotation offset, no weighted current).
        if self.routing_mode == Routing_mode::Usage {
            let ledger = match self.usage_ledger.lock() {
                Ok(ledger) => ledger,
                Err(_) => return hops,
            };
            let mut ranked: Vec<(u64, RouteHop)> = hops
                .into_iter()
                .map(|hop| (ledger.tokens(&hop.provider), hop))
                .collect();
            // Stable sort: equal (or absent) traffic keeps the configured
            // order, so a cold ledger is indistinguishable from `Order`.
            ranked.sort_by_key(|(tokens, _)| *tokens);
            return ranked.into_iter().map(|(_, hop)| hop).collect();
        }
        // Chain identity: the ordered provider list. Models are excluded so
        // the same provider chain shares one rotation across models.
        let key = hops
            .iter()
            .map(|hop| hop.provider.as_str())
            .collect::<Vec<_>>()
            .join("\u{1e}");
        let mut state = match self.chain_spread.lock() {
            Ok(state) => state,
            Err(_) => return hops,
        };
        state.bound();
        match self.routing_mode {
            // Unreachable: both were handled before the spread lock.
            Routing_mode::Order | Routing_mode::Usage => hops,
            Routing_mode::Rotate => {
                let at = {
                    let offset = state.offsets.entry(key).or_insert(0);
                    let at = *offset % hops.len();
                    *offset = offset.wrapping_add(1);
                    at
                };
                let mut rotated = hops;
                rotated.rotate_left(at);
                rotated
            }
            Routing_mode::Weight => {
                let weights: Vec<i64> = hops
                    .iter()
                    .map(|hop| {
                        self.provider_weights
                            .get(&hop.provider)
                            .copied()
                            .unwrap_or(1) as i64
                    })
                    .collect();
                let total = weights.iter().sum::<i64>().max(1);
                // One classic SWRR round is persisted (the first-pick
                // spread). The remaining failover order walks a scratch
                // copy so a request that never leaves its first hop still
                // mutates the shared state exactly once.
                for (hop, weight) in hops.iter().zip(&weights) {
                    *state.currents.entry(hop.provider.clone()).or_insert(0) += *weight;
                }
                let winner = (0..hops.len())
                    .max_by_key(|&index| {
                        let hop = &hops[index];
                        (
                            *state.currents.get(&hop.provider).unwrap_or(&0),
                            std::cmp::Reverse(index),
                        )
                    })
                    .expect("non-empty chain");
                *state
                    .currents
                    .entry(hops[winner].provider.clone())
                    .or_insert(0) -= total;
                // Scratch walk over a copy: order the rest without further
                // shared-state mutation.
                let mut scratch: HashMap<String, i64> = hops
                    .iter()
                    .map(|hop| {
                        (
                            hop.provider.clone(),
                            *state.currents.get(&hop.provider).unwrap_or(&0),
                        )
                    })
                    .collect();
                let mut order = Vec::with_capacity(hops.len());
                let mut picked = vec![false; hops.len()];
                picked[winner] = true;
                order.push(hops[winner].clone());
                while order.len() < hops.len() {
                    for (index, weight) in weights.iter().enumerate() {
                        if !picked[index] {
                            *scratch.entry(hops[index].provider.clone()).or_insert(0) += *weight;
                        }
                    }
                    let next = (0..hops.len())
                        .filter(|&index| !picked[index])
                        .max_by_key(|&index| {
                            let hop = &hops[index];
                            (
                                *scratch.get(&hop.provider).unwrap_or(&0),
                                std::cmp::Reverse(index),
                            )
                        })
                        .expect("unpicked candidate");
                    *scratch.entry(hops[next].provider.clone()).or_insert(0) -= total;
                    picked[next] = true;
                    order.push(hops[next].clone());
                }
                order
            }
        }
    }

    /// Shared hop gate for the three hop walkers (`complete_hops`,
    /// `count_tokens_for`, `stream_hops`): resolve the provider, enforce the
    /// enabled flag and the trusted credential scope, then acquire a breaker
    /// permit. Every skip is logged here with a content-free reason so the
    /// walkers cannot drift apart; `stream` only selects the established
    /// `stream=true` log suffix.
    async fn admit_hop(
        &self,
        hop: &RouteHop,
        credential: Option<&canonical::Client_credential>,
        stream: bool,
    ) -> Option<Provider_attempt> {
        let provider = match self.providers.get(&hop.provider) {
            Some(p) => p,
            None => {
                log::warn!(
                    "router: skip provider_ref={} reason=unknown",
                    crate::diagnostics::safe_reference(&hop.provider)
                );
                return None;
            }
        };
        if !provider.enabled() {
            self.log_hop_skip(hop, "disabled", stream);
            return None;
        }
        // Apply the trusted edge's credential scope before acquiring a
        // breaker permit or invoking a provider, including pinned routes.
        // Passthrough admission never grants stored-key use.
        if !provider_accepts_credential(provider.as_ref(), credential) {
            self.log_hop_skip(hop, "credential_scope", stream);
            return None;
        }
        if let Some(gate) = &self.admission_gate {
            if !gate.allows(&hop.provider, &hop.model).await {
                self.log_hop_skip(hop, "subscription_or_screening", stream);
                return None;
            }
        }
        match Provider_attempt::acquire(Arc::clone(provider)) {
            Some(attempt) => Some(attempt),
            None => {
                self.log_hop_skip(hop, "circuit_open", stream);
                None
            }
        }
    }

    fn log_hop_skip(&self, hop: &RouteHop, reason: &str, stream: bool) {
        if stream {
            log::warn!(
                "router: skip provider_ref={} reason={} stream=true",
                crate::diagnostics::safe_reference(&hop.provider),
                reason
            );
        } else {
            log::warn!(
                "router: skip provider_ref={} reason={}",
                crate::diagnostics::safe_reference(&hop.provider),
                reason
            );
        }
    }

    /// Non-streaming completion with automatic failover. For each hop:
    /// 1. Check circuit breaker — skip if open.
    /// 2. Call provider.complete.
    /// 3. On success: record_success, return.
    /// 4. On retriable error: record_failure, try next hop.
    /// 5. On non-retriable error: propagate immediately.
    pub async fn complete(
        &self,
        req: &ChatCompletionRequest,
    ) -> Result<OAIChatResponse, ProviderError> {
        let (hops, queue_built) = self.resolve_request_with_origin("_legacy", req)?;
        // Legacy complete(): no app_type known; use empty key so last_used
        // writes still succeed (only used by tests; persist won't read it).
        self.complete_hops_with_hop_inner("", "_legacy", req, hops, queue_built)
            .await
            .map(|(response, _)| response)
    }

    /// Surface-aware non-streaming completion: the last-resort chain is the
    /// entry endpoint's per-app queue (`/v1/messages` -> Claude chain,
    /// `/v1/responses` -> Codex chain, `/v1/chat/completions` -> chat chain).
    pub async fn complete_for(
        &self,
        surface: &str,
        app_type: &str,
        req: &ChatCompletionRequest,
    ) -> Result<OAIChatResponse, ProviderError> {
        let (hops, queue_built) = self.resolve_request_with_origin(surface, req)?;
        self.complete_hops_with_hop_inner(surface, app_type, req, hops, queue_built)
            .await
            .map(|(response, _)| response)
    }

    /// Request-local serving identity for workflows that continue an answer.
    /// Provider-reported model labels and the shared last-used map are display
    /// metadata; neither identifies the exact configured hop under concurrency.
    pub(crate) async fn complete_for_with_hop(
        &self,
        surface: &str,
        app_type: &str,
        req: &ChatCompletionRequest,
    ) -> Result<(OAIChatResponse, RouteHop), ProviderError> {
        let (hops, queue_built) = self.resolve_request_with_origin(surface, req)?;
        self.complete_hops_with_hop_inner(surface, app_type, req, hops, queue_built)
            .await
    }

    /// A pin restricts the eligible model plan to one provider; it cannot
    /// bypass model selection or forward a capability alias literally.
    pub async fn complete_with_provider(
        &self,
        surface: &str,
        app_type: &str,
        provider_id: &str,
        req: &ChatCompletionRequest,
    ) -> Result<OAIChatResponse, ProviderError> {
        let (hops, queue_built) = self.pinned_hops_with_origin(surface, provider_id, req)?;
        self.complete_hops_with_hop_inner(surface, app_type, req, hops, queue_built)
            .await
            .map(|(response, _)| response)
    }

    /// Force-stream variant of `complete_with_provider`.
    pub async fn stream_with_provider(
        &self,
        surface: &str,
        app_type: &str,
        provider_id: &str,
        req: &ChatCompletionRequest,
    ) -> Result<futures_util::stream::BoxStream<'static, OAIStreamChunk>, ProviderError> {
        let (hops, queue_built) = self.pinned_hops_with_origin(surface, provider_id, req)?;
        self.stream_hops_inner(surface, app_type, req, hops, queue_built).await
    }

    pub async fn complete_hops(
        &self,
        surface: &str,
        app_type: &str,
        req: &ChatCompletionRequest,
        hops: Arc<Vec<RouteHop>>,
    ) -> Result<OAIChatResponse, ProviderError> {
        self.complete_hops_with_hop_inner(surface, app_type, req, hops, false)
            .await
            .map(|(response, _)| response)
    }

    pub(crate) async fn complete_hops_with_hop(
        &self,
        surface: &str,
        app_type: &str,
        req: &ChatCompletionRequest,
        hops: Arc<Vec<RouteHop>>,
    ) -> Result<(OAIChatResponse, RouteHop), ProviderError> {
        self.complete_hops_with_hop_inner(surface, app_type, req, hops, false)
            .await
    }

    pub(crate) async fn complete_hops_with_hop_inner(
        &self,
        _surface: &str,
        app_type: &str,
        req: &ChatCompletionRequest,
        hops: Arc<Vec<RouteHop>>,
        queue_built: bool,
    ) -> Result<(OAIChatResponse, RouteHop), ProviderError> {
        // Prefer the session's serving provider inside the planned chain;
        // requests without an explicit session marker keep plan order.
        let hops = self.constrain_source(req, hops)?;
        let affinity_key = crate::proxy::session_affinity::affinity_key(req);
        let hops = self.affinity.preferred_hops(affinity_key.as_ref(), hops);
        let hops = if queue_built {
            self.defer_cooldown_members_stable(hops)
        } else {
            hops
        };
        let mut last_err: Option<ProviderError> = None;
        let mut failed_quota_scopes = std::collections::HashSet::new();
        for (hop_index, hop) in hops.iter().enumerate() {
            if self.quota_scope_failed(hop, &failed_quota_scopes) {
                self.log_hop_skip(hop, "quota_scope_exhausted", false);
                continue;
            }
            crate::proxy::request_timeline::observe_hop();
            let Some(mut attempt) = self.admit_hop(hop, req.client_credential.as_ref(), false).await
            else {
                continue;
            };
            let provider = Arc::clone(attempt.provider());
            // Zero-copy hop dispatch: the hop's upstream model rides as a
            // parameter, so the shared request payload is never cloned here
            // (Codex CLI conversations reach hundreds of KB — a per-hop
            // clone was pure overhead).
            let attempt_started = Instant::now();
            let hop_req = match self.request_for_hop(req, hop) {
                Ok(hop_req) => hop_req,
                Err(error) => return Err(error),
            };
            let hop_req = crate::proxy::prompt_budget::request_for_hop(
                app_type, &hop_req, &hop.model, provider.wire_protocol(),
            );
            // Arm only when the hop request is built and the provider is
            // about to be invoked: an armed attempt always corresponds to an
            // actual inference attempt. A prep rejection never reaches a
            // provider, records no health outcome, and only releases the
            // breaker permit as `Cancelled` on drop (unchanged).
            attempt.arm_health(
                Arc::clone(&self.model_observations),
                self.health_identity(&hop.provider),
                hop.model.clone(),
                attempt_started,
            );
            crate::proxy::request_timeline::observe_target(&req.model, &hop.model);
            let observation = self.receipt_observation(hop_index, provider.as_ref());
            let result = if observation.enabled() {
                provider.complete_observed(&hop_req, &hop.model, observation.clone()).await
            } else { provider.complete(&hop_req, &hop.model).await };
            match result {
                Ok(resp) => {
                    attempt.finish_health(true, None);
                    observation.selected(false);
                    crate::proxy::request_timeline::observe_plan(self.serving_plan(req, hop));
                    if let (Some(target), Some(source)) =
                        (self.source_target(hop), &resp.source_responses_response)
                    {
                        self.affinity.record_response(
                            &crate::proxy::session_affinity::client_ref(req),
                            source,
                            &target,
                        );
                    }
                    attempt.settle(Circuit_outcome::Success);
                    if let Ok(mut lu) = self.last_used.lock() {
                        lu.insert(app_type.to_string(), provider.id().to_string());
                    }
                    // Failure-only session stickiness: the provider that
                    // actually served becomes the session's preference.
                    if let Some(key) = affinity_key.clone() {
                        self.affinity.record_winner(key, provider.id(), &hop.model);
                    }
                    // Usage accounting: best-effort, off the response path.
                    // A write failure is logged and dropped by `record` so an
                    // answered request is never turned into an error.
                    let record = crate::proxy::usage_recorder::build_record(
                        app_type,
                        provider.id(),
                        &hop.model,
                        &req.model,
                        &resp,
                        attempt_started.elapsed().as_millis() as i64,
                    );
                    // Same success path feeds the routing ledger. It is
                    // routing state (not reporting), so it updates even
                    // when no database is attached; the lock is held for a
                    // single map update and failures are ignored.
                    if let Ok(mut ledger) = self.usage_ledger.lock() {
                        ledger.record(
                            provider.id(),
                            record.prompt_tokens,
                            record.completion_tokens,
                        );
                    }
                    crate::proxy::usage_recorder::record(self.usage_db.as_ref(), record);
                    log::info!(
                        "router: complete request_id={} model_ref={} provider_ref={} upstream_model_ref={} elapsed_ms={}",
                        req.request_id.as_deref().unwrap_or("unknown"),
                        crate::diagnostics::safe_reference(&req.model),
                        crate::diagnostics::safe_reference(provider.id()),
                        crate::diagnostics::safe_reference(&hop.model),
                        attempt_started.elapsed().as_millis()
                    );
                    return Ok((resp, hop.clone()));
                }
                Err(e) => {
                    observation.failed(&e.code);
                    attempt.finish_health(false, Some(safe_reason(&e.code)));
                    crate::proxy::request_timeline::observe_failure(&e.code, e.status);
                    let retriable =
                        e.retriable || should_failover_tool_capability_rejection(req, &e);
                    self.record_quota_failure(hop, &e, &mut failed_quota_scopes);
                    // Only retriable (provider-level) failures count against
                    // the breaker - non-retriable errors are local
                    // construction problems, not the provider's fault.
                    if retriable {
                        attempt.settle(Circuit_outcome::Failure);
                    }
                    log::warn!(
                        "router: failed request_id={} provider_ref={} upstream_model_ref={} elapsed_ms={} status={} retriable={} code={} stage={} response_bytes={}",
                        req.request_id.as_deref().unwrap_or("unknown"),
                        crate::diagnostics::safe_reference(provider.id()),
                        crate::diagnostics::safe_reference(&hop.model),
                        attempt_started.elapsed().as_millis(),
                        e.status.unwrap_or(500),
                        retriable,
                        e.code,
                        e.stage,
                        e.response_bytes.unwrap_or(0)
                    );
                    last_err = Some(e);
                    if !retriable {
                        return Err(last_err.unwrap());
                    }
                }
            }
        }
        Err(last_err.unwrap_or_else(|| {
            canonical::make_provider_error(
                "all_hops_exhausted",
                "routing",
                Some(503),
                Some(false),
                None,
            )
        }))
    }

    /// Input-token count for the `/v1/messages/count_tokens` surface.
    /// Walks the resolved hops with the same skip rules as `complete_hops`
    /// (unknown/disabled/circuit-open/no-credential-passthrough): the first
    /// hop that answers wins, a retriable failure tries the next hop
    /// (mirroring passthrough 401 semantics on messages).
    pub async fn count_tokens_for(
        &self,
        surface: &str,
        model: &str,
        body: &serde_json::Value,
        client_credential: Option<&crate::proxy::canonical::Client_credential>,
    ) -> Result<u32, ProviderError> {
        // Token counting is best-effort; plan with default features (no
        // hard-requirement filtering) so estimates work for any request.
        let hops = self.resolve_model(surface, model, Default::default())?;
        let mut last_err: Option<ProviderError> = None;
        let mut failed_quota_scopes = std::collections::HashSet::new();
        for hop in hops.iter() {
            if self.quota_scope_failed(hop, &failed_quota_scopes) {
                self.log_hop_skip(hop, "quota_scope_exhausted", false);
                continue;
            }
            crate::proxy::request_timeline::observe_hop();
            let Some(mut attempt) = self.admit_hop(hop, client_credential, false).await else {
                continue;
            };
            let provider = Arc::clone(attempt.provider());
            // The hop's upstream model rides on the body exactly like the
            // messages surface (count accuracy follows the routed upstream).
            // This clone is the only whole-body copy: the provider receives
            // the owned, model-patched body.
            let mut hop_body = body.clone();
            if let Some(object) = hop_body.as_object_mut() {
                object.insert("model".to_string(), serde_json::json!(hop.model));
            }
            crate::proxy::request_timeline::observe_target(model, &hop.model);
            match provider.count_tokens(hop_body, client_credential).await {
                Ok(tokens) => {
                    attempt.settle(Circuit_outcome::Success);
                    log::info!(
                        "router: count_tokens model_ref={} provider_ref={} upstream_model_ref={} input_tokens={}",
                        crate::diagnostics::safe_reference(model),
                        crate::diagnostics::safe_reference(provider.id()),
                        crate::diagnostics::safe_reference(&hop.model),
                        tokens
                    );
                    return Ok(tokens);
                }
                Err(e) => {
                    crate::proxy::request_timeline::observe_failure(&e.code, e.status);
                    let retriable = e.retriable;
                    self.record_quota_failure(hop, &e, &mut failed_quota_scopes);
                    if retriable {
                        attempt.settle(Circuit_outcome::Failure);
                    }
                    log::warn!(
                        "router: count_tokens failed provider_ref={} upstream_model_ref={} status={} retriable={} code={}",
                        crate::diagnostics::safe_reference(provider.id()),
                        crate::diagnostics::safe_reference(&hop.model),
                        e.status.unwrap_or(500),
                        retriable,
                        e.code
                    );
                    last_err = Some(e);
                    if !retriable {
                        return Err(last_err.unwrap());
                    }
                }
            }
        }
        Err(last_err.unwrap_or_else(|| {
            canonical::make_provider_error(
                "all_hops_exhausted",
                "routing",
                Some(503),
                Some(false),
                None,
            )
        }))
    }

    /// Health snapshot of all providers (for /health endpoint), extended
    /// with the derived terminal-attempt failure view.
    pub fn health(&self) -> Vec<ProviderHealth> {
        self.providers
            .values()
            .map(|p| ProviderHealth {
                id: p.id().to_string(),
                enabled: p.enabled(),
                breaker: p.breaker().snapshot(),
                failure: self.provider_failure_health(p.id()),
            })
            .collect()
    }

    /// Read-only projection of the configured failover queues — the legacy
    /// global queue first, then each surface chain in a deterministic
    /// surface order — including configured ids no live provider instance
    /// resolves. Unresolved candidates carry no measured health (absent,
    /// not zero).
    pub fn queue_health_projection(&self) -> Queue_health_projection {
        let mut candidates: Vec<Queue_candidate_health> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut totals = Queue_health_totals::default();
        let mut push = |router: &Self, id: &str| {
            if !seen.insert(id.to_string()) {
                return;
            }
            let resolved = router.providers.contains_key(id);
            let failure = resolved.then(|| router.provider_failure_health(id));
            match failure.as_ref() {
                Some(failure) if failure.latest_outcome == Some("ok") => totals.ok += 1,
                Some(failure) if failure.latest_outcome == Some("failed") => totals.failed += 1,
                Some(_) => totals.unobserved += 1,
                None => totals.unresolved += 1,
            }
            totals.total += 1;
            candidates.push(Queue_candidate_health {
                runtime_id: id.to_string(),
                source: router.health_identity(id),
                resolved,
                failure,
            });
        };
        for id in &self.queue {
            push(self, id);
        }
        let mut surfaces: Vec<&String> = self.queues.keys().collect();
        surfaces.sort();
        for surface in surfaces {
            for id in &self.queues[surface] {
                push(self, id);
            }
        }
        Queue_health_projection {
            candidates,
            totals,
        }
    }

    /// All model aliases (for /v1/models endpoint).
    pub fn list_models(&self) -> Vec<String> {
        self.routes.keys().cloned().collect()
    }
    /// Streaming completion with automatic failover (only pre-first-byte).
    /// Mirrors `Router.stream` in `src/gateway/router.ts:85`.
    ///
    /// For each hop: check breaker, call provider.stream(). Probe the first chunk
    /// before committing - allows failover if the upstream errors before sending
    /// data. Once we yield the first chunk, mid-stream errors surface to the client.
    pub async fn stream(
        &self,
        req: &ChatCompletionRequest,
    ) -> Result<futures_util::stream::BoxStream<'static, OAIStreamChunk>, ProviderError> {
        let (hops, queue_built) = self.resolve_request_with_origin("_legacy", req)?;
        self.stream_hops_inner("_legacy", "_legacy", req, hops, queue_built)
            .await
    }

    /// Surface-aware streaming completion (see `complete_for`).
    pub async fn stream_for(
        &self,
        surface: &str,
        app_type: &str,
        req: &ChatCompletionRequest,
    ) -> Result<futures_util::stream::BoxStream<'static, OAIStreamChunk>, ProviderError> {
        let (hops, queue_built) = self.resolve_request_with_origin(surface, req)?;
        self.stream_hops_inner(surface, app_type, req, hops, queue_built)
            .await
    }

    pub async fn stream_hops(
        &self,
        surface: &str,
        app_type: &str,
        req: &ChatCompletionRequest,
        hops: Arc<Vec<RouteHop>>,
    ) -> Result<futures_util::stream::BoxStream<'static, OAIStreamChunk>, ProviderError> {
        self.stream_hops_inner(surface, app_type, req, hops, false)
            .await
    }

    pub(crate) async fn stream_hops_inner(
        &self,
        _surface: &str,
        app_type: &str,
        req: &ChatCompletionRequest,
        hops: Arc<Vec<RouteHop>>,
        queue_built: bool,
    ) -> Result<futures_util::stream::BoxStream<'static, OAIStreamChunk>, ProviderError> {
        use futures_util::StreamExt;

        // Same session preference as the non-streaming walker: rotate the
        // bound provider to the front when it is a chain member.
        let hops = self.constrain_source(req, hops)?;
        let affinity_key = crate::proxy::session_affinity::affinity_key(req);
        let hops = self.affinity.preferred_hops(affinity_key.as_ref(), hops);
        let hops = if queue_built {
            self.defer_cooldown_members_stable(hops)
        } else {
            hops
        };
        let mut last_err: Option<ProviderError> = None;
        let mut failed_quota_scopes = std::collections::HashSet::new();
        for (hop_index, hop) in hops.iter().enumerate() {
            if self.quota_scope_failed(hop, &failed_quota_scopes) {
                self.log_hop_skip(hop, "quota_scope_exhausted", true);
                continue;
            }
            crate::proxy::request_timeline::observe_hop();
            let Some(mut attempt) = self.admit_hop(hop, req.client_credential.as_ref(), true).await
            else {
                continue;
            };
            let provider = Arc::clone(attempt.provider());
            let attempt_started = Instant::now();
            let hop_req = match self.request_for_hop(req, hop) {
                Ok(hop_req) => hop_req,
                Err(error) => return Err(error),
            };
            // Armed only after prep succeeds, immediately before the stream
            // call: a prep rejection never reaches a provider and records no
            // health outcome (breaker permit still releases as `Cancelled`).
            attempt.arm_health(
                Arc::clone(&self.model_observations),
                self.health_identity(&hop.provider),
                hop.model.clone(),
                attempt_started,
            );
            crate::proxy::request_timeline::observe_target(&req.model, &hop.model);
            let observation = self.receipt_observation(hop_index, provider.as_ref());
            let stream_result = if observation.enabled() {
                provider.stream_observed(&hop_req, &hop.model, observation.clone()).await
            } else { provider.stream(&hop_req, &hop.model).await };
            let mut chunk_stream = match stream_result {
                Ok(s) => s,
                Err(e) => {
                    attempt.finish_health(false, Some(safe_reason(&e.code)));
                    observation.failed(&e.code);
                    crate::proxy::request_timeline::observe_failure(&e.code, e.status);
                    let retriable =
                        e.retriable || should_failover_tool_capability_rejection(req, &e);
                    self.record_quota_failure(hop, &e, &mut failed_quota_scopes);
                    // Only retriable (provider-level) failures count against the
                    // breaker - non-retriable errors are local construction
                    // problems (serialize / client build), not the provider's
                    // fault, and must not pollute its health.
                    if retriable {
                        attempt.settle(Circuit_outcome::Failure);
                    }
                    log::warn!(
                    "router: stream setup failed request_id={} provider_ref={} upstream_model_ref={} elapsed_ms={} status={} retriable={} code={} stage={} response_bytes={}",
                    req.request_id.as_deref().unwrap_or("unknown"),
                    crate::diagnostics::safe_reference(provider.id()),
                    crate::diagnostics::safe_reference(&hop.model),
                    attempt_started.elapsed().as_millis(),
                    e.status.unwrap_or(500),
                    retriable,
                    e.code,
                    e.stage,
                    e.response_bytes.unwrap_or(0)
                );
                    last_err = Some(e);
                    if !retriable {
                        return Err(last_err.unwrap());
                    }
                    continue;
                }
            };
            // Probe first chunk. Bound the wait with FIRST_BYTE_TIMEOUT_MS so a
            // dead upstream (opened a connection but never sent bytes, or took
            // longer than this budget to send its first event) can't burn a full
            // upstream-RTT before the router fails over. Without this cap, every
            // hop in the chain adds ~1 s of wall-clock latency — a 5-hop walk
            // turned a fast reply into a 5 s pre-stream wait. The cap also keeps
            // circuit-breaker failures accumulating fast enough that the bad
            // hops open after a few rounds and get skipped on subsequent
            // requests.
            let first = match tokio::time::timeout(
                std::time::Duration::from_millis(self.first_byte_timeout_ms),
                chunk_stream.next(),
            )
            .await
            {
                Ok(Some(c)) => c,
                Ok(None) => {
                    observation.failed("upstream_stream_error");
                    attempt.finish_health(false, Some("upstream_stream_error"));
                    // An upstream that opened the stream but yielded zero chunks
                    // (e.g. the OpenAI/Anthropic provider swallowing an HTTP 4xx/5xx
                    // as an empty SSE body) should NOT be treated as a successful
                    // empty completion — that would surface a 200 with no data to
                    // the client. Record a failure and fail over to the next hop.
                    // Mock providers never hit this path (they always yield a role
                    // chunk + a finish chunk), so the seeded demo routes are
                    // unaffected.
                    attempt.settle(Circuit_outcome::Failure);
                    log::warn!(
                    "router: stream empty request_id={} model_ref={} provider_ref={} upstream_model_ref={} elapsed_ms={}",
                    req.request_id.as_deref().unwrap_or("unknown"),
                    crate::diagnostics::safe_reference(&req.model),
                    crate::diagnostics::safe_reference(provider.id()),
                    crate::diagnostics::safe_reference(&hop.model),
                    attempt_started.elapsed().as_millis()
                );
                    last_err = Some(canonical::make_provider_error(
                        "upstream_stream_error",
                        "stream_empty",
                        Some(502),
                        Some(true),
                        None,
                    ));
                    crate::proxy::request_timeline::observe_failure("upstream_stream_error", Some(502));
                    continue;
                }
                Err(_elapsed) => {
                    observation.failed("upstream_timeout");
                    attempt.finish_health(false, Some("upstream_timeout"));
                    // Upstream stalled past the first-byte budget. Treat as a
                    // retriable failure (counts against the breaker) so the next
                    // hop gets a chance and the breaker opens quickly when a
                    // provider is consistently slow-to-first-byte.
                    attempt.settle(Circuit_outcome::Failure);
                    log::warn!(
                    "router: stream first event timeout request_id={} budget_ms={} model_ref={} provider_ref={} upstream_model_ref={} elapsed_ms={}",
                    req.request_id.as_deref().unwrap_or("unknown"),
                    self.first_byte_timeout_ms,
                    crate::diagnostics::safe_reference(&req.model),
                    crate::diagnostics::safe_reference(provider.id()),
                    crate::diagnostics::safe_reference(&hop.model),
                    attempt_started.elapsed().as_millis()
                );
                    last_err = Some(canonical::make_provider_error(
                        "upstream_timeout",
                        "stream_first_event",
                        Some(504),
                        Some(true),
                        None,
                    ));
                    crate::proxy::request_timeline::observe_failure("upstream_timeout", Some(504));
                    continue;
                }
            };
            if let Some(error) = canonical::stream_chunk_error(&first) {
                attempt.finish_health(false, Some(safe_reason(&error.code)));
                observation.failed(&error.code);
                crate::proxy::request_timeline::observe_failure(&error.code, error.status);
                if error.retriable {
                    self.record_quota_failure(hop, &error, &mut failed_quota_scopes);
                    attempt.settle(Circuit_outcome::Failure);
                    last_err = Some(error);
                    continue;
                }
                return Err(error);
            }
            log::info!(
            "router: stream first event request_id={} model_ref={} provider_ref={} upstream_model_ref={} elapsed_ms={}",
            req.request_id.as_deref().unwrap_or("unknown"),
            crate::diagnostics::safe_reference(&req.model),
            crate::diagnostics::safe_reference(provider.id()),
            crate::diagnostics::safe_reference(&hop.model),
            attempt_started.elapsed().as_millis()
        );
            // Chain the first chunk with the rest of the stream. Mid-stream errors
            // surface to the client (no transparent failover).
            let last_used = Arc::clone(&self.last_used);
            let affinity = Arc::clone(&self.affinity);
            let affinity_key = affinity_key.clone();
            let source_target = self.source_target(hop);
            let source_client = crate::proxy::session_affinity::client_ref(req);
            let timeline = crate::proxy::request_timeline::Stream_observation::capture();
            timeline.streaming();
            observation.selected(true);
            let serving_plan = self.serving_plan(req, hop);
            let app_type = app_type.to_string();
            let upstream_model = hop.model.clone();
            // Streaming usage settles with the stream, not at commit.
            let usage_db = self.usage_db.clone();
            let usage_ledger = Arc::clone(&self.usage_ledger);
            let usage_inputs = (
                app_type.clone(),
                provider.id().to_string(),
                hop.model.clone(),
                req.model.clone(),
                attempt_started,
            );
            let combined = async_stream::stream! {
                timeline.selected_plan(serving_plan);
                let mut terminal_seen = false;
                let mut pending = Some(first);
                let mut native_response = None;
                let mut prompt_tokens: i64 = 0;
                let mut completion_tokens: i64 = 0;
                loop {
                    let chunk = if let Some(first) = pending.take() {
                        Some(first)
                    } else {
                        chunk_stream.next().await
                    };
                    let Some(chunk) = chunk else { break; };
                    if let Some(usage) = chunk.extra.get("usage") {
                        prompt_tokens = usage
                            .get("prompt_tokens")
                            .or_else(|| usage.get("input_tokens"))
                            .and_then(serde_json::Value::as_i64)
                            .unwrap_or(prompt_tokens);
                        completion_tokens = usage
                            .get("completion_tokens")
                            .or_else(|| usage.get("output_tokens"))
                            .and_then(serde_json::Value::as_i64)
                            .unwrap_or(completion_tokens);
                    }
                    if let Some(error) = canonical::stream_chunk_error(&chunk) {
                        attempt.finish_health(false, Some(safe_reason(&error.code)));
                        observation.failed(&error.code);
                        timeline.failure(&error.code, error.status);
                        timeline.failed();
                        attempt.settle(if error.retriable { Circuit_outcome::Failure } else { Circuit_outcome::Cancelled });
                        yield chunk;
                        return;
                    }
                    if let Some(frame) = &chunk.source_responses_event {
                        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&frame.data) {
                            if matches!(value.get("type").and_then(serde_json::Value::as_str), Some("response.completed" | "response.incomplete")) {
                                native_response = value.get("response").cloned();
                            }
                        }
                    }
                    terminal_seen |= chunk.choices.iter().any(|choice| choice.finish_reason.is_some())
                        || chunk.source_anthropic_event.as_ref().and_then(|event| event.get("type")).and_then(serde_json::Value::as_str) == Some("message_stop");
                    yield chunk;
                }
                if !terminal_seen {
                    attempt.finish_health(false, Some("upstream_stream_error"));
                    observation.failed("upstream_stream_error");
                    timeline.failed();
                    attempt.settle(Circuit_outcome::Failure);
                    yield canonical::stream_error_chunk(&upstream_model, 502, "proxy_stream_error");
                    return;
                }
                attempt.settle(Circuit_outcome::Success);
                attempt.finish_health(true, None);
                if let (Some(target), Some(response)) = (&source_target, &native_response) {
                    affinity.record_response(&source_client, response, target);
                }
                if let Ok(mut winners) = last_used.lock() {
                    winners.insert(app_type, attempt.provider.id().to_string());
                }
                // Session stickiness settles with the stream, not at commit.
                if let Some(key) = affinity_key {
                    affinity.record_winner(key, attempt.provider.id(), &upstream_model);
                }
                let (app_id, provider_id, hop_model, requested_model, stream_started) = usage_inputs;
                let upstream_model_for_price = hop_model.clone();
                crate::proxy::usage_recorder::record(
                    usage_db.as_ref(),
                    crate::database::dao::usage::UsageRecordInput {
                        ts: chrono::Utc::now().timestamp(),
                        app_id,
                        provider_id,
                        model: if hop_model.is_empty() { requested_model } else { hop_model },
                        prompt_tokens,
                        completion_tokens,
                        cost_usd: crate::proxy::usage_recorder::cost_usd(
                            &upstream_model_for_price,
                            prompt_tokens,
                            completion_tokens,
                        ),
                        duration_ms: stream_started.elapsed().as_millis() as i64,
                    },
                );
                // Feed the routing ledger from the same settle path; a
                // poisoned lock only loses spread accounting.
                if let Ok(mut ledger) = usage_ledger.lock() {
                    ledger.record(attempt.provider.id(), prompt_tokens, completion_tokens);
                }
            };
            return Ok(combined.boxed());
        }
        Err(last_err.unwrap_or_else(|| {
            canonical::make_provider_error(
                "all_hops_exhausted",
                "stream_routing",
                Some(503),
                Some(false),
                None,
            )
        }))
    }
}

#[cfg(test)]
mod router_runtime_tests {
    include!("router_runtime_tests.rs");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::dao::proxy_providers::CircuitBreakerConfig;
    use crate::proxy::circuit_breaker::CircuitBreaker;
    use crate::proxy::providers::Provider;
    use async_trait::async_trait;
    use futures_core::Stream;
    use futures_util::StreamExt;
    use std::pin::Pin;

    fn empty_cb() -> CircuitBreakerConfig {
        CircuitBreakerConfig {
            failure_threshold: 3,
            cooldown_ms: 15_000,
            half_open_retries: 1,
            success_threshold: 2,
        }
    }

    /// A provider that yields a stream but with zero chunks on `stream()` —
    /// simulating an upstream that swallowed an HTTP error into an empty SSE
    /// body (see the OpenAI/Anthropic streaming providers).
    struct EmptyStreamProvider {
        id: &'static str,
        breaker: CircuitBreaker,
    }

    impl EmptyStreamProvider {
        fn new(id: &'static str) -> Self {
            Self {
                id,
                breaker: CircuitBreaker::new(empty_cb()),
            }
        }
    }

    #[async_trait]
    impl Provider for EmptyStreamProvider {
        fn id(&self) -> &str {
            self.id
        }
        fn enabled(&self) -> bool {
            true
        }
        fn models(&self) -> &[String] {
            &[]
        }
        fn breaker(&self) -> &CircuitBreaker {
            &self.breaker
        }

        async fn complete(
            &self,
            _req: &ChatCompletionRequest,
            _upstream_model: &str,
        ) -> Result<OAIChatResponse, ProviderError> {
            Err(canonical::make_error("unused", Some(500), Some(true)))
        }

        async fn stream(
            &self,
            _req: &ChatCompletionRequest,
            _upstream_model: &str,
        ) -> Result<Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>>, ProviderError> {
            Ok(futures_util::stream::empty().boxed())
        }
    }

    /// A provider whose `stream()` fails before yielding anything (like mock-fail).
    struct ErrStreamProvider {
        id: &'static str,
        breaker: CircuitBreaker,
    }

    impl ErrStreamProvider {
        fn new(id: &'static str) -> Self {
            Self {
                id,
                breaker: CircuitBreaker::new(empty_cb()),
            }
        }
    }

    #[async_trait]
    impl Provider for ErrStreamProvider {
        fn id(&self) -> &str {
            self.id
        }
        fn enabled(&self) -> bool {
            true
        }
        fn models(&self) -> &[String] {
            &[]
        }
        fn breaker(&self) -> &CircuitBreaker {
            &self.breaker
        }

        async fn complete(
            &self,
            _req: &ChatCompletionRequest,
            _upstream_model: &str,
        ) -> Result<OAIChatResponse, ProviderError> {
            Err(canonical::make_error("unused", Some(500), Some(true)))
        }

        async fn stream(
            &self,
            _req: &ChatCompletionRequest,
            _upstream_model: &str,
        ) -> Result<Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>>, ProviderError> {
            Err(canonical::make_error(
                "forced failure",
                Some(503),
                Some(true),
            ))
        }
    }

    /// A real streaming provider that yields one role chunk then a finish chunk.
    struct WorkingProvider {
        id: &'static str,
        breaker: CircuitBreaker,
    }

    impl WorkingProvider {
        fn new(id: &'static str) -> Self {
            Self {
                id,
                breaker: CircuitBreaker::new(empty_cb()),
            }
        }
    }

    #[async_trait]
    impl Provider for WorkingProvider {
        fn id(&self) -> &str {
            self.id
        }
        fn enabled(&self) -> bool {
            true
        }
        fn models(&self) -> &[String] {
            &[]
        }
        fn breaker(&self) -> &CircuitBreaker {
            &self.breaker
        }

        async fn complete(
            &self,
            _req: &ChatCompletionRequest,
            upstream_model: &str,
        ) -> Result<OAIChatResponse, ProviderError> {
            Ok(OAIChatResponse {
                id: "working".into(),
                object: "chat.completion".into(),
                created: 0,
                model: upstream_model.into(),
                choices: vec![crate::proxy::canonical::OAIChoice {
                    index: 0,
                    message: crate::proxy::canonical::OAIMessage {
                        role: "assistant".into(),
                        content: Some("working".into()),
                        tool_calls: None,
                        refusal: None,
                        extra: Default::default(),
                    },
                    finish_reason: Some("stop".into()),
                    extra: Default::default(),
                }],
                usage: None,
                extra: Default::default(),
                source_anthropic_response: None,
                source_responses_response: None,
                source_gemini_response: None,
            })
        }

        async fn stream(
            &self,
            _req: &ChatCompletionRequest,
            upstream_model: &str,
        ) -> Result<Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>>, ProviderError> {
            let model = upstream_model.to_string();
            let stream = futures_util::stream::iter(vec![
                crate::proxy::converter::make_stream_chunk(
                    "id",
                    &model,
                    crate::proxy::canonical::StreamDelta {
                        role: Some("assistant".into()),
                        content: None,
                        tool_calls: None,
                        reasoning: None,
                        extra: Default::default(),
                    },
                    None,
                ),
                crate::proxy::converter::make_stream_chunk(
                    "id",
                    &model,
                    crate::proxy::canonical::StreamDelta {
                        role: None,
                        content: None,
                        tool_calls: None,
                        reasoning: None,
                        extra: Default::default(),
                    },
                    Some("stop".into()),
                ),
            ]);
            Ok(Box::pin(stream))
        }
    }

    /// A passthrough-shaped provider for hop-skip coverage: advertises
    /// `requires_client_credential`, records invocations, and serves a
    /// marker response so the caller can tell which hop answered.
    struct PassthroughProbeProvider {
        breaker: CircuitBreaker,
        called: std::sync::atomic::AtomicBool,
    }

    impl PassthroughProbeProvider {
        fn shared() -> (Arc<Self>, Arc<Self>) {
            let probe = Arc::new(Self {
                breaker: CircuitBreaker::new(empty_cb()),
                called: std::sync::atomic::AtomicBool::new(false),
            });
            (probe.clone(), probe)
        }
    }

    #[async_trait]
    impl Provider for PassthroughProbeProvider {
        fn id(&self) -> &str {
            "sub"
        }
        fn enabled(&self) -> bool {
            true
        }
        fn models(&self) -> &[String] {
            &[]
        }
        fn breaker(&self) -> &CircuitBreaker {
            &self.breaker
        }
        fn requires_client_credential(&self) -> bool {
            true
        }

        async fn complete(
            &self,
            _req: &ChatCompletionRequest,
            upstream_model: &str,
        ) -> Result<OAIChatResponse, ProviderError> {
            self.called.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(OAIChatResponse {
                id: "passthrough".into(),
                object: "chat.completion".into(),
                created: 0,
                model: upstream_model.into(),
                choices: vec![crate::proxy::canonical::OAIChoice {
                    index: 0,
                    message: crate::proxy::canonical::OAIMessage {
                        role: "assistant".into(),
                        content: Some("passthrough_served".into()),
                        tool_calls: None,
                        refusal: None,
                        extra: Default::default(),
                    },
                    finish_reason: Some("stop".into()),
                    extra: Default::default(),
                }],
                usage: None,
                extra: Default::default(),
                source_anthropic_response: None,
                source_responses_response: None,
                source_gemini_response: None,
            })
        }

        async fn stream(
            &self,
            _req: &ChatCompletionRequest,
            _upstream_model: &str,
        ) -> Result<Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>>, ProviderError> {
            self.called.store(true, std::sync::atomic::Ordering::SeqCst);
            let stream =
                futures_util::stream::iter(vec![crate::proxy::converter::make_stream_chunk(
                    "id",
                    "m",
                    crate::proxy::canonical::StreamDelta {
                        role: Some("assistant".into()),
                        content: None,
                        tool_calls: None,
                        reasoning: None,
                        extra: Default::default(),
                    },
                    Some("stop".into()),
                )]);
            Ok(Box::pin(stream))
        }

        async fn count_tokens(
            &self,
            _body: serde_json::Value,
            _client_credential: Option<&canonical::Client_credential>,
        ) -> Result<u32, ProviderError> {
            self.called.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(111)
        }
    }

    #[allow(non_camel_case_types)]
    struct Credential_scope_probe {
        working: WorkingProvider,
        models: Vec<String>,
        passthrough: bool,
        calls: std::sync::atomic::AtomicUsize,
    }

    impl Credential_scope_probe {
        fn record_attempt(&self) -> Result<(), ProviderError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.passthrough {
                // Match a rejected relayed bearer: retriable, but never
                // permission to dispatch to a stored-key provider.
                Err(canonical::make_error(
                    "upstream_http_error",
                    Some(401),
                    Some(true),
                ))
            } else {
                Ok(())
            }
        }
    }

    #[async_trait]
    impl Provider for Credential_scope_probe {
        fn id(&self) -> &str {
            self.working.id()
        }
        fn enabled(&self) -> bool {
            true
        }
        fn models(&self) -> &[String] {
            &self.models
        }
        fn breaker(&self) -> &CircuitBreaker {
            self.working.breaker()
        }
        fn requires_client_credential(&self) -> bool {
            self.passthrough
        }

        async fn complete(
            &self,
            request: &ChatCompletionRequest,
            upstream_model: &str,
        ) -> Result<OAIChatResponse, ProviderError> {
            self.record_attempt()?;
            self.working.complete(request, upstream_model).await
        }

        async fn stream(
            &self,
            request: &ChatCompletionRequest,
            upstream_model: &str,
        ) -> Result<Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>>, ProviderError> {
            self.record_attempt()?;
            self.working.stream(request, upstream_model).await
        }

        async fn count_tokens(
            &self,
            _body: serde_json::Value,
            _credential: Option<&canonical::Client_credential>,
        ) -> Result<u32, ProviderError> {
            self.record_attempt()?;
            Ok(42)
        }
    }

    fn credential_scope_fixture(
        include_relay: bool,
    ) -> (
        Router,
        Arc<Credential_scope_probe>,
        Arc<Credential_scope_probe>,
    ) {
        let pool = Arc::new(Credential_scope_probe {
            working: WorkingProvider::new("pool"),
            models: vec!["m".into()],
            passthrough: false,
            calls: std::sync::atomic::AtomicUsize::new(0),
        });
        let relay = Arc::new(Credential_scope_probe {
            working: WorkingProvider::new("relay"),
            models: vec![],
            passthrough: true,
            calls: std::sync::atomic::AtomicUsize::new(0),
        });
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        providers.insert("pool".into(), pool.clone());
        providers.insert("relay".into(), relay.clone());
        let mut routes = HashMap::new();
        if include_relay {
            routes.insert(
                "m".into(),
                vec![
                    RouteHop {
                        provider: "relay".into(),
                        model: "m".into(),
                    },
                    RouteHop {
                        provider: "pool".into(),
                        model: "m".into(),
                    },
                ],
            );
        }
        (Router::new(providers, routes), pool, relay)
    }

    fn scoped_request(passthrough_only: bool) -> ChatCompletionRequest {
        let mut request = req_for("m");
        request.client_credential = Some(canonical::Client_credential {
            passthrough_only,
            authorization: Some("Bearer synthetic-rejected-credential".into()),
            ..Default::default()
        });
        request
    }

    #[tokio::test]
    async fn passthrough_only_cannot_dispatch_static_auto_routes_or_pins() {
        let (router, pool, relay) = credential_scope_fixture(false);
        let request = scoped_request(true);
        assert!(router.complete(&request).await.is_err());
        assert!(router.stream(&request).await.is_err());
        assert!(router
            .complete_for(SURFACE_CLAUDE, "_test", &request)
            .await
            .is_err());
        assert!(router
            .stream_for(SURFACE_CLAUDE, "_test", &request)
            .await
            .is_err());
        assert!(router
            .complete_with_provider(SURFACE_CLAUDE, "_test", "pool", &request)
            .await
            .is_err());
        assert!(router
            .stream_with_provider(SURFACE_CLAUDE, "_test", "pool", &request)
            .await
            .is_err());
        assert!(router
            .count_tokens_for(
                SURFACE_CLAUDE,
                "m",
                &serde_json::json!({"model": "m"}),
                request.client_credential.as_ref()
            )
            .await
            .is_err());
        assert_eq!(pool.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(relay.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn passthrough_only_rejected_bearer_cannot_fall_back_to_static_key() {
        let (router, pool, relay) = credential_scope_fixture(true);
        let request = scoped_request(true);
        assert_eq!(
            router.complete(&request).await.unwrap_err().status,
            Some(401)
        );
        assert_eq!(
            router.stream(&request).await.err().unwrap().status,
            Some(401)
        );
        assert_eq!(
            router
                .count_tokens_for(
                    SURFACE_CLAUDE,
                    "m",
                    &serde_json::json!({"model": "m"}),
                    request.client_credential.as_ref()
                )
                .await
                .unwrap_err()
                .status,
            Some(401)
        );
        assert_eq!(pool.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(relay.calls.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn gateway_authenticated_rejected_relay_retains_static_fallback() {
        let (router, pool, relay) = credential_scope_fixture(true);
        let request = scoped_request(false);
        assert!(router.complete(&request).await.is_ok());
        let chunks: Vec<_> = router.stream(&request).await.unwrap().collect().await;
        assert!(!chunks.is_empty());
        assert_eq!(
            router
                .count_tokens_for(
                    SURFACE_CLAUDE,
                    "m",
                    &serde_json::json!({"model": "m"}),
                    request.client_credential.as_ref()
                )
                .await
                .unwrap(),
            42
        );
        assert_eq!(pool.calls.load(std::sync::atomic::Ordering::SeqCst), 3);
        assert_eq!(relay.calls.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[test]
    fn request_json_cannot_supply_internal_credential_scope() {
        let request: ChatCompletionRequest = serde_json::from_value(serde_json::json!({
            "model": "m", "messages": [],
            "client_credential": {"authorization": "Bearer forged", "passthrough_only": false}
        }))
        .unwrap();
        assert!(request.client_credential.is_none());
        let request = scoped_request(true);
        let wire = serde_json::to_value(&request).unwrap();
        assert!(wire.get("client_credential").is_none());
        assert!(wire.get("passthrough_only").is_none());
    }

    /// count_tokens follows the same hop-skip contract as messages: a
    /// passthrough hop without a client credential is skipped (the static
    /// hop behind it answers), a credentialed request reaches it.
    #[tokio::test]
    async fn count_tokens_skips_credentialless_passthrough_hops() {
        let body = serde_json::json!({"model": "m", "messages": []});
        {
            let (handle, probe) = PassthroughProbeProvider::shared();
            let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
            providers.insert("sub".into(), handle);
            providers.insert("pool".into(), Arc::new(WorkingProvider::new("pool")));
            let hops = Arc::new(vec![
                RouteHop {
                    provider: "sub".into(),
                    model: "m".into(),
                },
                RouteHop {
                    provider: "pool".into(),
                    model: "m".into(),
                },
            ]);
            let mut routes: HashMap<String, Vec<RouteHop>> = HashMap::new();
            routes.insert("m".into(), (*hops).clone());
            let router = Router::new(providers, routes);
            let tokens = router
                .count_tokens_for("", "m", &body, None)
                .await
                .expect("the static hop must serve the credentialless count");
            assert_ne!(tokens, 111, "the passthrough probe must not have answered");
            assert!(
                !probe.called.load(std::sync::atomic::Ordering::SeqCst),
                "a credentialless passthrough hop must be skipped"
            );
        }
        {
            let (handle, probe) = PassthroughProbeProvider::shared();
            let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
            providers.insert("sub".into(), handle);
            let hops = Arc::new(vec![RouteHop {
                provider: "sub".into(),
                model: "upstream-model".into(),
            }]);
            let mut routes: HashMap<String, Vec<RouteHop>> = HashMap::new();
            routes.insert("m".into(), (*hops).clone());
            let router = Router::new(providers, routes);
            let credential = canonical::Client_credential {
                passthrough_only: false,
                authorization: Some("Bearer oauth-relay-9".into()),
                x_api_key: None,
                anthropic_beta: Some("context-1m".into()),
            };
            let tokens = router
                .count_tokens_for("", "m", &body, Some(&credential))
                .await
                .expect("the credentialed hop must serve the count");
            assert_eq!(tokens, 111);
            assert!(probe.called.load(std::sync::atomic::Ordering::SeqCst));
        }
    }

    /// A passthrough hop must be skipped when the request carries no client
    /// credential (only /v1/messages attaches one): chat/completions and
    /// responses surfaces would otherwise burn an upstream 401 and terminate
    /// the route before the static-key hops behind it can serve.
    #[tokio::test]
    async fn passthrough_hop_skipped_without_client_credential() {
        let (handle, probe) = PassthroughProbeProvider::shared();
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        providers.insert("sub".into(), handle);
        providers.insert("pool".into(), Arc::new(WorkingProvider::new("pool")));
        let hops = Arc::new(vec![
            RouteHop {
                provider: "sub".into(),
                model: "m".into(),
            },
            RouteHop {
                provider: "pool".into(),
                model: "m".into(),
            },
        ]);
        let router = Router::new(providers, HashMap::new());
        let response = router
            .complete_hops("", "_test", &req_for("m"), hops)
            .await
            .expect("static-key hop must still serve the request");
        assert_eq!(
            response.choices[0].message.content.as_deref(),
            Some("working"),
            "the static-key hop behind the skipped passthrough hop must answer"
        );
        assert!(
            !probe.called.load(std::sync::atomic::Ordering::SeqCst),
            "a passthrough hop must not be dispatched without a client credential"
        );
    }

    /// With a client credential present the passthrough hop is dispatched
    /// normally — the skip must not starve /v1/messages subscription calls.
    #[tokio::test]
    async fn passthrough_hop_dispatched_with_client_credential() {
        let (handle, probe) = PassthroughProbeProvider::shared();
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        providers.insert("sub".into(), handle);
        providers.insert("pool".into(), Arc::new(WorkingProvider::new("pool")));
        let hops = Arc::new(vec![RouteHop {
            provider: "sub".into(),
            model: "m".into(),
        }]);
        let router = Router::new(providers, HashMap::new());
        let mut request = req_for("m");
        request.client_credential = Some(canonical::Client_credential {
            passthrough_only: false,
            authorization: Some("Bearer oauth-relay-7".into()),
            x_api_key: None,
            anthropic_beta: None,
        });
        let response = router
            .complete_hops("", "_test", &request, hops)
            .await
            .expect("the passthrough hop must serve the credentialed request");
        assert_eq!(
            response.choices[0].message.content.as_deref(),
            Some("passthrough_served")
        );
        assert!(probe.called.load(std::sync::atomic::Ordering::SeqCst));
    }

    /// Same guard on the streaming path: no credential → skip the hop, the
    /// stream comes from the static-key hop behind it.
    #[tokio::test]
    async fn passthrough_stream_hop_skipped_without_client_credential() {
        let (handle, probe) = PassthroughProbeProvider::shared();
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        providers.insert("sub".into(), handle);
        providers.insert("pool".into(), Arc::new(WorkingProvider::new("pool")));
        let hops = Arc::new(vec![
            RouteHop {
                provider: "sub".into(),
                model: "m".into(),
            },
            RouteHop {
                provider: "pool".into(),
                model: "m".into(),
            },
        ]);
        let router = Router::new(providers, HashMap::new());
        let stream = router
            .stream_hops("", "_test", &req_for("m"), hops)
            .await
            .expect("static-key hop must still stream");
        let chunks: Vec<OAIStreamChunk> = stream.collect().await;
        assert!(!chunks.is_empty());
        assert!(
            !probe.called.load(std::sync::atomic::Ordering::SeqCst),
            "a passthrough stream hop must not be dispatched without a client credential"
        );
    }

    fn req_for(model: &str) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: model.into(),
            messages: vec![],
            stream: true,
            temperature: None,
            max_tokens: None,
            tools: None,
            tool_choice: None,
            extra: Default::default(),
            source_anthropic_request: None,
            source_responses_request: None,
            source_gemini_request: None,
            request_id: None,
            client_credential: None,
        }
    }

    fn req_with_tool(model: &str) -> ChatCompletionRequest {
        let mut request = req_for(model);
        request.tools = Some(vec![serde_json::json!({
            "type": "function",
            "function": {"name": "probe", "parameters": {"type": "object"}},
        })]);
        request
    }

    struct ToolRejectProvider {
        id: &'static str,
        breaker: CircuitBreaker,
        /// Optional upstream-4xx classification. The router's helper
        /// gates failover on `kind == ToolCapability`; auth-expired and
        /// unclassified 400s must remain terminal.
        kind: Option<canonical::UpstreamErrorKind>,
    }

    impl ToolRejectProvider {
        fn new(id: &'static str) -> Self {
            Self {
                id,
                breaker: CircuitBreaker::new(empty_cb()),
                kind: None,
            }
        }
        fn with_kind(id: &'static str, kind: canonical::UpstreamErrorKind) -> Self {
            Self {
                id,
                breaker: CircuitBreaker::new(empty_cb()),
                kind: Some(kind),
            }
        }
    }

    #[async_trait]
    impl Provider for ToolRejectProvider {
        fn id(&self) -> &str {
            self.id
        }
        fn enabled(&self) -> bool {
            true
        }
        fn models(&self) -> &[String] {
            &[]
        }
        fn breaker(&self) -> &CircuitBreaker {
            &self.breaker
        }

        async fn complete(
            &self,
            _req: &ChatCompletionRequest,
            _upstream_model: &str,
        ) -> Result<OAIChatResponse, ProviderError> {
            Err(canonical::make_provider_error_with_kind(
                "upstream_http_error",
                "response_status",
                Some(400),
                Some(false),
                Some(84),
                self.kind,
            ))
        }

        async fn stream(
            &self,
            _req: &ChatCompletionRequest,
            _upstream_model: &str,
        ) -> Result<Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>>, ProviderError> {
            Err(canonical::make_provider_error_with_kind(
                "upstream_http_error",
                "response_status",
                Some(400),
                Some(false),
                Some(84),
                self.kind,
            ))
        }
    }

    /// A provider whose `stream()` returns a stream that **never** yields a
    /// chunk — simulating an upstream that opened the connection but stalled
    /// before its first byte. Used to verify the router's first-byte timeout
    /// (FIRST_BYTE_TIMEOUT_MS) fires and the router advances to the next hop
    /// instead of hanging indefinitely.
    struct StallProvider {
        id: &'static str,
        breaker: CircuitBreaker,
    }

    impl StallProvider {
        fn new(id: &'static str) -> Self {
            Self {
                id,
                breaker: CircuitBreaker::new(empty_cb()),
            }
        }
    }

    #[async_trait]
    impl Provider for StallProvider {
        fn id(&self) -> &str {
            self.id
        }
        fn enabled(&self) -> bool {
            true
        }
        fn models(&self) -> &[String] {
            &[]
        }
        fn breaker(&self) -> &CircuitBreaker {
            &self.breaker
        }

        async fn complete(
            &self,
            _req: &ChatCompletionRequest,
            _upstream_model: &str,
        ) -> Result<OAIChatResponse, ProviderError> {
            Err(canonical::make_error("unused", Some(500), Some(true)))
        }

        async fn stream(
            &self,
            _req: &ChatCompletionRequest,
            _upstream_model: &str,
        ) -> Result<Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>>, ProviderError> {
            // A stream that polls Pending forever — exercises the
            // first-byte-timeout race in Router::stream_hops.
            let s = futures_util::stream::pending::<OAIStreamChunk>();
            Ok(Box::pin(s))
        }
    }

    /// First-byte timeout (FIRST_BYTE_TIMEOUT_MS) must fire on a stalled
    /// upstream and the router must fail over to the next hop. Without the
    /// race, this test would hang for the full upstream timeout (60 s) or
    /// the test's own timeout. Bound the assertion with a 3 s test deadline
    /// so a regression is caught immediately.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stalled_stream_fails_over_via_first_byte_timeout() {
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        providers.insert("stall".into(), Arc::new(StallProvider::new("stall")));
        providers.insert("working".into(), Arc::new(WorkingProvider::new("working")));
        let mut routes: HashMap<String, Vec<RouteHop>> = HashMap::new();
        routes.insert(
            "m".into(),
            vec![
                RouteHop {
                    provider: "stall".into(),
                    model: "m".into(),
                },
                RouteHop {
                    provider: "working".into(),
                    model: "m".into(),
                },
            ],
        );
        let mut router = Router::new(providers, routes);
        // Use a short first-byte timeout so the stall test finishes fast.
        router.first_byte_timeout_ms = 1_500;

        // FIRST_BYTE_TIMEOUT_MS is 1500 ms; the test must complete well
        // under 3 s — otherwise the cap regressed.
        let stream = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            router.stream(&req_for("m")),
        )
        .await
        .expect("stream_hops should not exceed 3 s for a stalled hop")
        .expect("stream should succeed via fallback");

        use futures_util::StreamExt;
        let chunks: Vec<OAIStreamChunk> = stream.collect().await;
        assert!(
            !chunks.is_empty(),
            "should fail over to the working provider"
        );
        assert_eq!(
            chunks[0].choices[0].delta.role.as_deref(),
            Some("assistant")
        );
        assert_eq!(
            chunks.last().unwrap().choices[0].finish_reason.as_deref(),
            Some("stop")
        );
        // The stalled hop's breaker should have recorded exactly one failure.
        let breaker = router
            .health()
            .into_iter()
            .find(|h| h.id == "stall")
            .expect("stall provider present in health");
        assert_eq!(
            breaker.breaker.failures, 1,
            "stalled hop must record exactly one breaker failure"
        );
        assert!(router.model_health("stall", "m").unavailable);
        assert_eq!(router.model_health("working", "m").success_rate, Some(1.0));
    }

    /// When every hop stalls, the router must return an error within
    /// FIRST_BYTE_TIMEOUT_MS × hops — not the upstream's 60 s request timeout.
    /// This is the case where the breaker hasn't opened yet but every
    /// provider is consistently slow-to-first-byte.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn all_stalled_hops_return_quickly() {
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        providers.insert("s1".into(), Arc::new(StallProvider::new("s1")));
        providers.insert("s2".into(), Arc::new(StallProvider::new("s2")));
        providers.insert("s3".into(), Arc::new(StallProvider::new("s3")));
        let mut routes: HashMap<String, Vec<RouteHop>> = HashMap::new();
        routes.insert(
            "m".into(),
            vec![
                RouteHop {
                    provider: "s1".into(),
                    model: "m".into(),
                },
                RouteHop {
                    provider: "s2".into(),
                    model: "m".into(),
                },
                RouteHop {
                    provider: "s3".into(),
                    model: "m".into(),
                },
            ],
        );
        let mut router = Router::new(providers, routes);
        // Use a short first-byte timeout so the stall test finishes fast.
        router.first_byte_timeout_ms = 1_500;

        // 3 hops × 1500 ms = 4500 ms — plus headroom. Without the cap this
        // would hang for 60 s per hop.
        let res = tokio::time::timeout(
            std::time::Duration::from_secs(6),
            router.stream(&req_for("m")),
        )
        .await
        .expect("all-stalled walk should complete within 6 s (3 × 1.5 s budget)");
        assert!(res.is_err(), "all hops stalled -> should error");
        let err = res.err().unwrap();
        assert!(err.retriable, "all-stalled error is retriable");
        // Every hop's breaker should have one failure.
        for h in router.health() {
            assert_eq!(h.breaker.failures, 1, "{} should record one failure", h.id);
        }
    }

    #[tokio::test]
    async fn empty_stream_fails_over_to_next_hop() {
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        providers.insert("empty".into(), Arc::new(EmptyStreamProvider::new("empty")));
        providers.insert("working".into(), Arc::new(WorkingProvider::new("working")));
        let mut routes: HashMap<String, Vec<RouteHop>> = HashMap::new();
        routes.insert(
            "m".into(),
            vec![
                RouteHop {
                    provider: "empty".into(),
                    model: "m".into(),
                },
                RouteHop {
                    provider: "working".into(),
                    model: "m".into(),
                },
            ],
        );
        let router = Router::new(providers, routes);

        let mut s = router
            .stream(&req_for("m"))
            .await
            .expect("stream should succeed");
        use futures_util::StreamExt;
        let chunks: Vec<OAIStreamChunk> = s.by_ref().collect().await;
        assert!(
            !chunks.is_empty(),
            "should fail over to the working provider"
        );
        assert_eq!(
            chunks[0].choices[0].delta.role.as_deref(),
            Some("assistant")
        );
        assert_eq!(
            chunks.last().unwrap().choices[0].finish_reason.as_deref(),
            Some("stop")
        );
    }

    #[tokio::test]
    async fn empty_stream_with_no_fallback_returns_error() {
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        providers.insert("empty".into(), Arc::new(EmptyStreamProvider::new("empty")));
        let mut routes: HashMap<String, Vec<RouteHop>> = HashMap::new();
        routes.insert(
            "m".into(),
            vec![RouteHop {
                provider: "empty".into(),
                model: "m".into(),
            }],
        );
        let router = Router::new(providers, routes);

        let err = match router.stream(&req_for("m")).await {
            Ok(_) => panic!("empty stream with no fallback should error"),
            Err(e) => e,
        };
        assert_eq!(err.status, Some(502));
        assert!(err.retriable);
    }

    #[tokio::test]
    async fn error_stream_fails_over_to_next_hop() {
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        providers.insert("err".into(), Arc::new(ErrStreamProvider::new("err")));
        providers.insert("working".into(), Arc::new(WorkingProvider::new("working")));
        let mut routes: HashMap<String, Vec<RouteHop>> = HashMap::new();
        routes.insert(
            "m".into(),
            vec![
                RouteHop {
                    provider: "err".into(),
                    model: "m".into(),
                },
                RouteHop {
                    provider: "working".into(),
                    model: "m".into(),
                },
            ],
        );
        let router = Router::new(providers, routes);

        let mut s = router
            .stream(&req_for("m"))
            .await
            .expect("stream should succeed");
        use futures_util::StreamExt;
        let chunks: Vec<OAIStreamChunk> = s.by_ref().collect().await;
        assert!(
            !chunks.is_empty(),
            "should fail over to the working provider"
        );
        assert_eq!(
            chunks.last().unwrap().choices[0].finish_reason.as_deref(),
            Some("stop")
        );
    }

    #[tokio::test]
    async fn tool_capability_400_fails_over_for_complete_and_stream() {
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        providers.insert(
            "reject".into(),
            Arc::new(ToolRejectProvider::with_kind(
                "reject",
                canonical::UpstreamErrorKind::ToolCapability,
            )),
        );
        providers.insert("working".into(), Arc::new(WorkingProvider::new("working")));
        let mut routes: HashMap<String, Vec<RouteHop>> = HashMap::new();
        routes.insert(
            "m".into(),
            vec![
                RouteHop {
                    provider: "reject".into(),
                    model: "reject-model".into(),
                },
                RouteHop {
                    provider: "working".into(),
                    model: "working-model".into(),
                },
            ],
        );
        let router = Router::new(providers, routes);

        let complete = router
            .complete(&req_with_tool("m"))
            .await
            .expect("complete fallback");
        assert_eq!(
            complete.choices[0].message.content.as_deref(),
            Some("working")
        );
        let mut stream = router
            .stream(&req_with_tool("m"))
            .await
            .expect("stream fallback");
        let chunks: Vec<OAIStreamChunk> = stream.by_ref().collect().await;
        assert_eq!(
            chunks
                .last()
                .and_then(|chunk| chunk.choices[0].finish_reason.as_deref()),
            Some("stop")
        );
        let reject_health = router
            .health()
            .into_iter()
            .find(|health| health.id == "reject")
            .unwrap();
        assert_eq!(reject_health.breaker.failures, 2);
    }

    #[tokio::test]
    async fn ordinary_400_without_tools_remains_terminal() {
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        providers.insert("reject".into(), Arc::new(ToolRejectProvider::new("reject")));
        providers.insert("working".into(), Arc::new(WorkingProvider::new("working")));
        let mut routes: HashMap<String, Vec<RouteHop>> = HashMap::new();
        routes.insert(
            "m".into(),
            vec![
                RouteHop {
                    provider: "reject".into(),
                    model: "reject-model".into(),
                },
                RouteHop {
                    provider: "working".into(),
                    model: "working-model".into(),
                },
            ],
        );
        let router = Router::new(providers, routes);
        let error = router
            .complete(&req_for("m"))
            .await
            .expect_err("ordinary 400 must remain terminal");
        assert_eq!(error.status, Some(400));
        assert!(!error.retriable);
    }

    /// The regression case: Codex CLI always sends tools; OpenAI returns 400
    /// with an OAuth refresh-token body; the router must NOT fail over. The
    /// breaker failures counter must stay at 0 (non-retriable errors are
    /// recorded as Cancelled, not Failure).
    #[tokio::test]
    async fn auth_expired_400_with_tools_is_terminal() {
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        providers.insert(
            "reject".into(),
            Arc::new(ToolRejectProvider::with_kind(
                "reject",
                canonical::UpstreamErrorKind::AuthExpired,
            )),
        );
        providers.insert("working".into(), Arc::new(WorkingProvider::new("working")));
        let mut routes: HashMap<String, Vec<RouteHop>> = HashMap::new();
        routes.insert(
            "m".into(),
            vec![
                RouteHop {
                    provider: "reject".into(),
                    model: "reject-model".into(),
                },
                RouteHop {
                    provider: "working".into(),
                    model: "working-model".into(),
                },
            ],
        );
        let router = Router::new(providers, routes);

        let error = router
            .complete(&req_with_tool("m"))
            .await
            .expect_err("auth-expired 400 with tools must remain terminal");
        assert_eq!(error.status, Some(400));
        assert!(!error.retriable);
        assert_eq!(error.kind, Some(canonical::UpstreamErrorKind::AuthExpired));
        let reject_health = router
            .health()
            .into_iter()
            .find(|health| health.id == "reject")
            .unwrap();
        assert_eq!(
            reject_health.breaker.failures, 0,
            "non-retriable 400 must not increment the breaker"
        );
    }

    /// An unclassified 400 with tools must also remain terminal — the
    /// legacy default behaviour, restored after the regression. Only an
    /// explicit `ToolCapability` classification opens the failover gate.
    #[tokio::test]
    async fn unclassified_400_with_tools_is_terminal() {
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        providers.insert("reject".into(), Arc::new(ToolRejectProvider::new("reject")));
        providers.insert("working".into(), Arc::new(WorkingProvider::new("working")));
        let mut routes: HashMap<String, Vec<RouteHop>> = HashMap::new();
        routes.insert(
            "m".into(),
            vec![
                RouteHop {
                    provider: "reject".into(),
                    model: "reject-model".into(),
                },
                RouteHop {
                    provider: "working".into(),
                    model: "working-model".into(),
                },
            ],
        );
        let router = Router::new(providers, routes);

        let error = router
            .complete(&req_with_tool("m"))
            .await
            .expect_err("unclassified 400 with tools must remain terminal");
        assert_eq!(error.status, Some(400));
        assert!(!error.retriable);
        assert_eq!(error.kind, None);
    }

    // ---- Phase 17: per-surface failover chains ----

    /// A provider whose responses carry its own id, so tests can tell which
    /// surface's chain served the request.
    struct TaggedProvider {
        id: &'static str,
        breaker: CircuitBreaker,
    }

    impl TaggedProvider {
        fn new(id: &'static str) -> Self {
            Self {
                id,
                breaker: CircuitBreaker::new(empty_cb()),
            }
        }
    }

    #[async_trait]
    impl Provider for TaggedProvider {
        fn id(&self) -> &str {
            self.id
        }
        fn enabled(&self) -> bool {
            true
        }
        fn models(&self) -> &[String] {
            &[]
        }
        fn breaker(&self) -> &CircuitBreaker {
            &self.breaker
        }

        async fn complete(
            &self,
            _req: &ChatCompletionRequest,
            _upstream_model: &str,
        ) -> Result<OAIChatResponse, ProviderError> {
            Err(canonical::make_error(
                format!("fail:{}", self.id),
                Some(500),
                Some(true),
            ))
        }

        async fn stream(
            &self,
            _req: &ChatCompletionRequest,
            upstream_model: &str,
        ) -> Result<Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>>, ProviderError> {
            // First TaggedProvider in a chain fails (retriable) so the next
            // hop is tried; a provider tagged "ok" yields a chunk carrying
            // its id in the finish_reason slot for assertion.
            if self.id.starts_with("ok") {
                let tag = self.id.to_string();
                let stream =
                    futures_util::stream::iter(vec![crate::proxy::converter::make_stream_chunk(
                        "id",
                        upstream_model,
                        crate::proxy::canonical::StreamDelta {
                            role: Some("assistant".into()),
                            content: Some(tag),
                            tool_calls: None,
                            reasoning: None,
                            extra: Default::default(),
                        },
                        None,
                    )]);
                Ok(Box::pin(stream))
            } else {
                Err(canonical::make_error(
                    format!("fail:{}", self.id),
                    Some(500),
                    Some(true),
                ))
            }
        }
    }

    #[tokio::test]
    async fn surface_chains_route_to_their_own_queue() {
        // Two-hop chains (bad -> ok-<surface>) per surface; the bad head
        // fails with a retriable error so the chain falls through to ok.
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        providers.insert(
            "claude-bad".into(),
            Arc::new(TaggedProvider::new("claude-bad")),
        );
        providers.insert(
            "ok-claude".into(),
            Arc::new(TaggedProvider::new("ok-claude")),
        );
        providers.insert(
            "codex-bad".into(),
            Arc::new(TaggedProvider::new("codex-bad")),
        );
        providers.insert("ok-codex".into(), Arc::new(TaggedProvider::new("ok-codex")));
        providers.insert("chat-bad".into(), Arc::new(TaggedProvider::new("chat-bad")));
        providers.insert("ok-chat".into(), Arc::new(TaggedProvider::new("ok-chat")));

        let mut queues: HashMap<String, Vec<String>> = HashMap::new();
        queues.insert(
            SURFACE_CLAUDE.into(),
            vec!["claude-bad".into(), "ok-claude".into()],
        );
        queues.insert(
            SURFACE_CODEX.into(),
            vec!["codex-bad".into(), "ok-codex".into()],
        );
        queues.insert(
            SURFACE_CHAT.into(),
            vec!["chat-bad".into(), "ok-chat".into()],
        );

        let router = Router::new(providers, HashMap::new()).with_queues(queues);

        // A model with no explicit route and no auto-route: each surface's
        // chain must serve it, failing over bad -> ok.
        for (surface, expected_tag) in [
            (SURFACE_CLAUDE, "ok-claude"),
            (SURFACE_CODEX, "ok-codex"),
            (SURFACE_CHAT, "ok-chat"),
        ] {
            let mut s = router
                .stream_for(surface, "_test", &req_for("unknown-model"))
                .await
                .expect("stream should succeed via surface chain");
            let first = s.next().await.expect("first chunk");
            assert_eq!(
                first.choices[0].delta.content.as_deref(),
                Some(expected_tag),
                "surface {surface} should be served by its own chain"
            );
        }
    }

    #[tokio::test]
    async fn explicit_route_still_wins_over_surface_chain() {
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        providers.insert("ok-chat".into(), Arc::new(TaggedProvider::new("ok-chat")));
        providers.insert("ok-route".into(), Arc::new(TaggedProvider::new("ok-route")));
        let mut routes: HashMap<String, Vec<RouteHop>> = HashMap::new();
        routes.insert(
            "special-model".into(),
            vec![RouteHop {
                provider: "ok-route".into(),
                model: "special-model".into(),
            }],
        );
        let mut queues: HashMap<String, Vec<String>> = HashMap::new();
        queues.insert(SURFACE_CHAT.into(), vec!["ok-chat".into()]);
        let router = Router::new(providers, routes).with_queues(queues);

        let mut s = router
            .stream_for(SURFACE_CHAT, "_test", &req_for("special-model"))
            .await
            .expect("stream should succeed");
        let first = s.next().await.expect("first chunk");
        assert_eq!(
            first.choices[0].delta.content.as_deref(),
            Some("ok-route"),
            "explicit route must win over the surface chain"
        );
    }

    #[tokio::test]
    async fn no_chain_anywhere_yields_404() {
        let router = Router::new(HashMap::new(), HashMap::new()).with_queues(HashMap::new());
        let err = match router
            .stream_for(SURFACE_CHAT, "_test", &req_for("unknown-model"))
            .await
        {
            Ok(_) => panic!("no providers at all should 404"),
            Err(e) => e,
        };
        assert_eq!(err.status, Some(404));
    }

    /// The queue head is always tried first. When it fails, requests fall
    /// through to the tail; when its breaker recovers (cooldown → half-open
    /// → closed), the head serves again — the failover never "sticks" to a
    /// fallback once the head is back.
    #[tokio::test]
    async fn recovered_queue_head_regains_requests() {
        use crate::proxy::providers::mock::MockProvider;
        let p1 = Arc::new(MockProvider::new(
            "p1".into(),
            vec!["mock-fast".into()],
            true,
            CircuitBreakerConfig {
                failure_threshold: 2,
                cooldown_ms: 50,
                half_open_retries: 1,
                success_threshold: 2,
            },
        )) as Arc<dyn Provider>;
        let p2 = Arc::new(MockProvider::new(
            "p2".into(),
            vec!["mock-fast".into()],
            true,
            CircuitBreakerConfig {
                failure_threshold: 5,
                cooldown_ms: 30_000,
                half_open_retries: 1,
                success_threshold: 2,
            },
        )) as Arc<dyn Provider>;

        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        providers.insert("p1".into(), p1.clone());
        providers.insert("p2".into(), p2.clone());
        let mut routes: HashMap<String, Vec<RouteHop>> = HashMap::new();
        routes.insert(
            "m".into(),
            vec![
                RouteHop {
                    provider: "p1".into(),
                    model: "mock-fast".into(),
                },
                RouteHop {
                    provider: "p2".into(),
                    model: "mock-fast".into(),
                },
            ],
        );
        let router = Router::new(providers, routes);

        let served_by = |c: &OAIChatResponse| -> String {
            c.choices[0]
                .message
                .content
                .as_deref()
                .unwrap_or("")
                .to_string()
        };

        // 1. Head (p1) healthy → served by p1.
        let c1 = router.complete(&req_for("m")).await.expect("req 1");
        assert!(
            served_by(&c1).contains("MOCK[p1]"),
            "head should serve first: {}",
            served_by(&c1)
        );

        // 2. Trip p1's breaker (threshold 2) → p1 OPEN.
        p1.breaker().record_failure();
        p1.breaker().record_failure();

        // 3. During cooldown p1 is skipped → falls to p2.
        let c2 = router.complete(&req_for("m")).await.expect("req 2");
        assert!(
            served_by(&c2).contains("MOCK[p2]"),
            "open head must fall to tail: {}",
            served_by(&c2)
        );

        // 4. Cooldown expires → p1 half-open → serves the probe request.
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        let c3 = router
            .complete(&req_for("m"))
            .await
            .expect("req 3 (half-open probe)");
        assert!(
            served_by(&c3).contains("MOCK[p1]"),
            "half-open head should probe: {}",
            served_by(&c3)
        );

        // 5. Second success in half-open closes p1's breaker.
        let c4 = router
            .complete(&req_for("m"))
            .await
            .expect("req 4 (close breaker)");
        assert!(
            served_by(&c4).contains("MOCK[p1]"),
            "head should keep serving while recovering"
        );

        // 6. Fully recovered head serves again — request returns to the queue head.
        let c5 = router
            .complete(&req_for("m"))
            .await
            .expect("req 5 (recovered head)");
        assert!(
            served_by(&c5).contains("MOCK[p1]"),
            "recovered head must regain requests: {}",
            served_by(&c5)
        );
    }

    // ---- capability-alias planner resolution ----

    use crate::proxy::planner::{Model_planner, Planned_hop, Request_features, Resolution};

    fn planned(provider: &str, model: &str) -> Planned_hop {
        Planned_hop {
            provider: provider.into(),
            model: model.into(),
            tier: Some(3),
            priority: 0,
            basis: "manual_override".into(),
            downgraded: false,
            reason: "stub".into(),
        }
    }

    /// Name-driven planner: each `stub-*` name exercises one resolution arm.
    #[allow(non_camel_case_types)]
    struct Stub_planner;

    impl Model_planner for Stub_planner {
        fn resolve(
            &self,
            raw_model: &str,
            _features: Request_features,
        ) -> Result<Resolution, ProviderError> {
            match raw_model {
                "stub-precise-ok" => Ok(Resolution::Precise {
                    model: raw_model.into(),
                    hops: vec![planned("p1", "real-model")],
                }),
                "stub-precise-empty" => Ok(Resolution::Precise {
                    model: raw_model.into(),
                    hops: vec![],
                }),
                "stub-alias-ok" => Ok(Resolution::Alias {
                    alias: raw_model.into(),
                    tier: 3,
                    hops: vec![planned("p1", "strong-model"), planned("p2", "backup-model")],
                }),
                "stub-alias-empty" => Ok(Resolution::Alias {
                    alias: raw_model.into(),
                    tier: 2,
                    hops: vec![],
                }),
                "stub-exact-route" => Ok(Resolution::Exact("route-target".into())),
                "stub-exact-legacy" => Ok(Resolution::Exact("legacy-model".into())),
                other => Ok(Resolution::Exact(other.into())),
            }
        }
    }

    /// Minimal provider advertising one model so the legacy chain can match.
    struct ModelledProvider {
        id: &'static str,
        models: Vec<String>,
        breaker: CircuitBreaker,
    }

    #[async_trait]
    impl Provider for ModelledProvider {
        fn id(&self) -> &str {
            self.id
        }
        fn enabled(&self) -> bool {
            true
        }
        fn models(&self) -> &[String] {
            &self.models
        }
        fn breaker(&self) -> &CircuitBreaker {
            &self.breaker
        }

        async fn complete(
            &self,
            _req: &ChatCompletionRequest,
            _upstream_model: &str,
        ) -> Result<OAIChatResponse, ProviderError> {
            Err(canonical::make_error("unused", Some(500), Some(true)))
        }

        async fn stream(
            &self,
            _req: &ChatCompletionRequest,
            _upstream_model: &str,
        ) -> Result<Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>>, ProviderError> {
            Err(canonical::make_error("unused", Some(500), Some(true)))
        }
    }

    fn planner_router(routes: HashMap<String, Vec<RouteHop>>) -> Router {
        Router::new(HashMap::new(), routes).with_planner(Arc::new(Stub_planner))
    }

    #[test]
    fn alias_preview_matches_health_selected_queue_without_changing_precise_routes() {
        let router = planner_router(HashMap::new());
        router.model_observations.lock().unwrap().record_terminal(
            &Health_identity::Proxy { id: "p1".into() },
            "strong-model",
            false,
            Some("provider_error"),
            20.0,
        );
        let preview = router.preview_model(SURFACE_CHAT, "stub-alias-ok", Default::default(), None).unwrap();
        let runtime = router.resolve_model(SURFACE_CHAT, "stub-alias-ok", Default::default()).unwrap();
        assert_eq!(preview.iter().map(|hop| (&hop.provider, &hop.model)).collect::<Vec<_>>(), runtime.iter().map(|hop| (&hop.provider, &hop.model)).collect::<Vec<_>>());
        assert_eq!(runtime[0].provider, "p2");
        assert_eq!(router.model_health("p1", "strong-model").samples, 1);
    }

    fn hop_models(hops: &[RouteHop]) -> Vec<(String, String)> {
        hops.iter()
            .map(|hop| (hop.provider.clone(), hop.model.clone()))
            .collect()
    }

    #[test]
    fn credentialless_models_respect_oauth_routes_pins_and_configured_legacy_targets() {
        use crate::proxy::providers::mock::MockProvider;
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        for (id, models) in [
            ("a", vec!["model-a".into(), "blocked-model".into()]),
            ("b", vec!["model-b".into()]),
            ("route-only", vec![]),
        ] {
            providers.insert(
                id.into(),
                Arc::new(MockProvider::new(id.into(), models, true, empty_cb())),
            );
        }
        let (passthrough, _) = PassthroughProbeProvider::shared();
        providers.insert("sub".into(), passthrough);
        let routes = [
            (
                "blocked-model".into(),
                vec![RouteHop {
                    provider: "sub".into(),
                    model: "blocked-model".into(),
                }],
            ),
            (
                "configured-alias".into(),
                vec![RouteHop {
                    provider: "route-only".into(),
                    model: "configured-target".into(),
                }],
            ),
            (
                "empty-target".into(),
                vec![RouteHop {
                    provider: "route-only".into(),
                    model: String::new(),
                }],
            ),
        ]
        .into_iter()
        .collect();
        let router = Router::new(providers, routes).with_planner(Arc::new(Stub_planner));
        assert_eq!(
            router.credentialless_models(SURFACE_CODEX, None),
            vec!["configured-alias", "model-a", "model-b"]
        );
        assert_eq!(
            router.credentialless_models(SURFACE_CODEX, Some("b")),
            vec!["model-b"]
        );
        assert_eq!(
            router.credentialless_models(SURFACE_CODEX, Some("route-only")),
            vec!["configured-alias"]
        );
        assert!(router
            .credentialless_models(SURFACE_CODEX, Some("sub"))
            .is_empty());
        assert_eq!(router.desktop_models(None), vec!["model-a", "model-b"]);
    }

    #[test]
    fn credentialless_models_do_not_advertise_a_substitution_for_an_offered_name() {
        use crate::proxy::providers::mock::MockProvider;
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(
            "static".into(),
            vec!["offered-model".into(), "replacement-model".into()],
            true,
            empty_cb(),
        ));
        let router = Router::new(
            [("static".into(), provider)].into_iter().collect(),
            [(
                "offered-model".into(),
                vec![RouteHop {
                    provider: "static".into(),
                    model: "replacement-model".into(),
                }],
            )]
            .into_iter()
            .collect(),
        );
        assert_eq!(
            router.credentialless_models(SURFACE_CODEX, None),
            vec!["replacement-model"]
        );
    }

    #[test]
    fn explicit_route_cannot_replace_a_planned_exact_model() {
        let mut routes: HashMap<String, Vec<RouteHop>> = HashMap::new();
        routes.insert(
            "stub-precise-ok".into(),
            vec![RouteHop {
                provider: "manual".into(),
                model: "manual-model".into(),
            }],
        );
        let router = planner_router(routes);
        let error = router
            .resolve_model("claude", "stub-precise-ok", Request_features::default())
            .expect_err("manual substitution must not bypass an exact plan");
        assert_eq!(error.code, "model_precise_unavailable");
    }

    #[test]
    fn planner_precise_hops_are_used_verbatim() {
        let router = planner_router(HashMap::new());
        let hops = router
            .resolve_model("claude", "stub-precise-ok", Request_features::default())
            .expect("precise plan resolves");
        assert_eq!(hop_models(&hops), vec![("p1".into(), "real-model".into())]);
        // The request-aware entry point shares the same resolution.
        let hops = router
            .resolve_request("claude", &req_with_tool("stub-precise-ok"))
            .expect("precise plan resolves via request");
        assert_eq!(hop_models(&hops), vec![("p1".into(), "real-model".into())]);
    }

    #[test]
    fn planner_precise_with_no_offer_surfaces_model_precise_unavailable() {
        let router = planner_router(HashMap::new());
        let err = router
            .resolve_model("claude", "stub-precise-empty", Request_features::default())
            .expect_err("an exact model with no supply must not be substituted");
        assert_eq!(err.code, "model_precise_unavailable");
        assert_eq!(err.stage, "route_resolution");
        assert!(!err.retriable);
    }

    #[test]
    fn planner_alias_hops_keep_the_planned_model_chain() {
        let router = planner_router(HashMap::new());
        let hops = router
            .resolve_model("claude", "stub-alias-ok", Request_features::default())
            .expect("alias plan resolves");
        assert_eq!(
            hop_models(&hops),
            vec![
                ("p1".into(), "strong-model".into()),
                ("p2".into(), "backup-model".into()),
            ]
        );
    }

    #[test]
    fn planner_alias_with_no_candidates_surfaces_model_alias_no_candidates() {
        let router = planner_router(HashMap::new());
        let err = router
            .resolve_model("claude", "stub-alias-empty", Request_features::default())
            .expect_err("an alias with no eligible candidate must not fall into a blind queue");
        assert_eq!(err.code, "model_alias_no_candidates");
        assert_eq!(err.stage, "route_resolution");
        assert!(!err.retriable);
    }

    /// An `Exact` name keeps the legacy precedence: a route alias for the
    /// resolved name first, then model-matching providers.
    #[test]
    fn planner_exact_name_reuses_a_matching_route_alias() {
        let mut routes: HashMap<String, Vec<RouteHop>> = HashMap::new();
        routes.insert(
            "route-target".into(),
            vec![RouteHop {
                provider: "p9".into(),
                model: "mapped".into(),
            }],
        );
        let router = planner_router(routes);
        let hops = router
            .resolve_model("claude", "stub-exact-route", Request_features::default())
            .expect("exact name resolves through the route alias");
        assert_eq!(hop_models(&hops), vec![("p9".into(), "mapped".into())]);
    }

    #[test]
    fn planner_exact_name_falls_through_to_legacy_matching() {
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        providers.insert(
            "legacy".into(),
            Arc::new(ModelledProvider {
                id: "legacy",
                models: vec!["legacy-model".into()],
                breaker: CircuitBreaker::new(empty_cb()),
            }),
        );
        let router = Router::new(providers, HashMap::new()).with_planner(Arc::new(Stub_planner));
        let hops = router
            .resolve_model("claude", "stub-exact-legacy", Request_features::default())
            .expect("exact name resolves on the legacy chain");
        assert_eq!(
            hop_models(&hops),
            vec![("legacy".into(), "legacy-model".into())]
        );
    }

    #[test]
    fn without_a_planner_names_resolve_on_the_legacy_chain() {
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        providers.insert(
            "legacy".into(),
            Arc::new(ModelledProvider {
                id: "legacy",
                models: vec!["legacy-model".into()],
                breaker: CircuitBreaker::new(empty_cb()),
            }),
        );
        let router = Router::new(providers, HashMap::new());
        let hops = router
            .resolve_model("claude", "legacy-model", Request_features::default())
            .expect("legacy matching still works without a planner");
        assert_eq!(
            hop_models(&hops),
            vec![("legacy".into(), "legacy-model".into())]
        );
        let err = router
            .resolve_model("claude", "unknown-model", Request_features::default())
            .expect_err("unknown names still fail");
        assert_eq!(err.code, "no_route");
    }

    // ---- session affinity (stable conversations) ----

    /// Provider that counts calls and can be forced to fail.
    #[allow(non_camel_case_types)]
    struct SessionProvider {
        id: &'static str,
        breaker: CircuitBreaker,
        calls: std::sync::atomic::AtomicUsize,
        fail: std::sync::atomic::AtomicBool,
    }

    impl SessionProvider {
        fn new(id: &'static str) -> Arc<Self> {
            Arc::new(Self {
                id,
                breaker: CircuitBreaker::new(empty_cb()),
                calls: std::sync::atomic::AtomicUsize::new(0),
                fail: std::sync::atomic::AtomicBool::new(false),
            })
        }
    }

    #[async_trait]
    impl Provider for SessionProvider {
        fn id(&self) -> &str {
            self.id
        }
        fn enabled(&self) -> bool {
            true
        }
        fn models(&self) -> &[String] {
            &[]
        }
        fn breaker(&self) -> &CircuitBreaker {
            &self.breaker
        }

        async fn complete(
            &self,
            _req: &ChatCompletionRequest,
            upstream_model: &str,
        ) -> Result<OAIChatResponse, ProviderError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(canonical::make_error(
                    "upstream_http_error",
                    Some(500),
                    Some(true),
                ));
            }
            Ok(OAIChatResponse {
                id: "session".into(),
                object: "chat.completion".into(),
                created: 0,
                model: upstream_model.into(),
                choices: vec![crate::proxy::canonical::OAIChoice {
                    index: 0,
                    message: crate::proxy::canonical::OAIMessage {
                        role: "assistant".into(),
                        content: Some("ok".into()),
                        tool_calls: None,
                        refusal: None,
                        extra: Default::default(),
                    },
                    finish_reason: Some("stop".into()),
                    extra: Default::default(),
                }],
                usage: None,
                extra: Default::default(),
                source_anthropic_response: None,
                source_responses_response: None,
                source_gemini_response: None,
            })
        }

        async fn stream(
            &self,
            _req: &ChatCompletionRequest,
            _upstream_model: &str,
        ) -> Result<Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>>, ProviderError> {
            Err(canonical::make_error("unused", Some(500), Some(true)))
        }
    }

    fn session_request(model: &str, user: Option<&str>) -> ChatCompletionRequest {
        let mut req = req_for(model);
        if let Some(user) = user {
            req.extra
                .insert("user".into(), serde_json::Value::String(user.into()));
        }
        req
    }

    /// An explicit session marker keeps a conversation on its serving
    /// provider (rotated to the front of the plan), a failure rebinds the
    /// session to the failover winner, and a request without a marker makes
    /// no stickiness claim at all.
    #[tokio::test]
    async fn session_affinity_prefers_and_rebinds_the_serving_provider() {
        let p1 = SessionProvider::new("p1");
        let p2 = SessionProvider::new("p2");
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        providers.insert("p1".into(), Arc::clone(&p1) as Arc<dyn Provider>);
        providers.insert("p2".into(), Arc::clone(&p2) as Arc<dyn Provider>);
        let mut routes: HashMap<String, Vec<RouteHop>> = HashMap::new();
        routes.insert(
            "m".into(),
            vec![
                RouteHop {
                    provider: "p1".into(),
                    model: "m".into(),
                },
                RouteHop {
                    provider: "p2".into(),
                    model: "m".into(),
                },
            ],
        );
        let router = Router::new(providers, routes);

        // Bind the session to the chain tail out-of-band.
        let key = (
            "local".to_string(),
            crate::diagnostics::safe_reference("session-1"),
        );
        router.affinity.record_winner(key, "p2", "m");

        // The bound (tail) provider serves first: rotation, not plan order.
        let req = session_request("m", Some("session-1"));
        router.complete(&req).await.expect("bound request");
        assert_eq!(p1.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(p2.calls.load(std::sync::atomic::Ordering::SeqCst), 1);

        // No explicit marker: no stickiness claim, the plan head serves.
        let anonymous = session_request("m", None);
        router
            .complete(&anonymous)
            .await
            .expect("anonymous request");
        assert_eq!(p1.calls.load(std::sync::atomic::Ordering::SeqCst), 1);

        // Failure-only switching: when the bound provider starts failing,
        // the session rebinds to the failover winner.
        p2.fail.store(true, std::sync::atomic::Ordering::SeqCst);
        router.complete(&req).await.expect("failover request");
        assert_eq!(p1.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
        let rebound = router
            .affinity
            .bound_provider(&(
                "local".to_string(),
                crate::diagnostics::safe_reference("session-1"),
            ))
            .expect("session stays bound after failover");
        assert_eq!(rebound, "p1");
    }

    /// Provider whose stream ends with a usage-bearing chunk.
    #[allow(non_camel_case_types)]
    struct UsageStreamProvider {
        id: &'static str,
        breaker: CircuitBreaker,
    }

    #[async_trait]
    impl Provider for UsageStreamProvider {
        fn id(&self) -> &str {
            self.id
        }
        fn enabled(&self) -> bool {
            true
        }
        fn models(&self) -> &[String] {
            &[]
        }
        fn breaker(&self) -> &CircuitBreaker {
            &self.breaker
        }

        async fn complete(
            &self,
            _req: &ChatCompletionRequest,
            _upstream_model: &str,
        ) -> Result<OAIChatResponse, ProviderError> {
            Err(canonical::make_error("unused", Some(500), Some(true)))
        }

        async fn stream(
            &self,
            _req: &ChatCompletionRequest,
            upstream_model: &str,
        ) -> Result<Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>>, ProviderError> {
            let mut chunks = vec![OAIStreamChunk {
                id: "s".into(),
                object: "chat.completion.chunk".into(),
                created: 0,
                model: upstream_model.into(),
                choices: vec![crate::proxy::canonical::StreamChoice {
                    index: 0,
                    delta: crate::proxy::canonical::StreamDelta {
                        role: Some("assistant".into()),
                        content: Some("ok".into()),
                        ..Default::default()
                    },
                    finish_reason: None,
                    extra: Default::default(),
                }],
                extra: Default::default(),
                source_anthropic_event: None,
                source_responses_event: None,
                source_gemini_event: None,
            }];
            chunks.push(OAIStreamChunk {
                id: "s".into(),
                object: "chat.completion.chunk".into(),
                created: 0,
                model: upstream_model.into(),
                choices: vec![crate::proxy::canonical::StreamChoice {
                    index: 0,
                    delta: Default::default(),
                    finish_reason: Some("stop".into()),
                    extra: Default::default(),
                }],
                extra: std::collections::HashMap::from([(
                    "usage".into(),
                    serde_json::json!({"prompt_tokens": 11, "completion_tokens": 7}),
                )]),
                source_anthropic_event: None,
                source_responses_event: None,
                source_gemini_event: None,
            });
            Ok(futures_util::stream::iter(chunks).boxed())
        }
    }

    /// A completed stream settles usage exactly once, with the tokens the
    /// upstream reported and the hop's concrete model on the row.
    #[tokio::test]
    async fn streaming_success_settles_usage_with_reported_tokens() {
        let provider = Arc::new(UsageStreamProvider {
            id: "u1",
            breaker: CircuitBreaker::new(empty_cb()),
        });
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        providers.insert("u1".into(), provider as Arc<dyn Provider>);
        let mut routes: HashMap<String, Vec<RouteHop>> = HashMap::new();
        routes.insert(
            "m".into(),
            vec![RouteHop {
                provider: "u1".into(),
                model: "upstream-model".into(),
            }],
        );
        let db = crate::database::Database::memory().expect("usage database");
        let router = Router::new(providers, routes).with_usage_db(db.clone());

        let stream = router
            .stream(&req_for("m"))
            .await
            .expect("stream should commit");
        use futures_util::StreamExt;
        let chunks: Vec<OAIStreamChunk> = stream.collect().await;
        assert_eq!(chunks.len(), 2);
        assert_eq!(db.count_usage().expect("usage count"), 1);
        let by_model = db
            .get_usage_by_model(None, None, None)
            .expect("usage by model");
        assert_eq!(by_model.len(), 1);
        assert_eq!(by_model[0].model, "upstream-model");
        assert_eq!(by_model[0].total_prompt_tokens, 11);
        assert_eq!(by_model[0].total_completion_tokens, 7);
        // The same settle path feeds the Routing_mode::Usage ledger.
        assert_eq!(
            router.usage_ledger.lock().unwrap().tokens("u1"),
            18,
            "streamed usage must land in the routing ledger as prompt + completion"
        );
    }

    // ---- Routing modes (queue-built chain spread) ----

    /// A provider that advertises fixed models and never serves: these tests
    /// only exercise resolution order, never dispatch.
    struct SpreadProvider {
        id: &'static str,
        models: Vec<String>,
        breaker: CircuitBreaker,
    }

    impl SpreadProvider {
        fn new(id: &'static str, models: &[&str]) -> Self {
            Self {
                id,
                models: models.iter().map(|model| (*model).to_string()).collect(),
                breaker: CircuitBreaker::new(empty_cb()),
            }
        }
    }

    #[async_trait]
    impl Provider for SpreadProvider {
        fn id(&self) -> &str {
            self.id
        }
        fn enabled(&self) -> bool {
            true
        }
        fn models(&self) -> &[String] {
            &self.models
        }
        fn breaker(&self) -> &CircuitBreaker {
            &self.breaker
        }

        async fn complete(
            &self,
            _req: &ChatCompletionRequest,
            _upstream_model: &str,
        ) -> Result<OAIChatResponse, ProviderError> {
            Err(canonical::make_error("unused", Some(500), Some(true)))
        }

        async fn stream(
            &self,
            _req: &ChatCompletionRequest,
            _upstream_model: &str,
        ) -> Result<Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>>, ProviderError> {
            Err(canonical::make_error("unused", Some(500), Some(true)))
        }
    }

    /// Three providers a, b, c all serving "m", queued in that order.
    fn spread_router(mode: Routing_mode, weights: &[(&'static str, u32)]) -> Router {
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        for (id, _) in weights {
            providers.insert(
                (*id).to_string(),
                Arc::new(SpreadProvider::new(id, &["m", "n"])) as Arc<dyn Provider>,
            );
        }
        let queue: Vec<String> = weights.iter().map(|(id, _)| (*id).to_string()).collect();
        let mut provider_weights = HashMap::new();
        for (id, weight) in weights {
            provider_weights.insert((*id).to_string(), *weight);
        }
        Router::new(providers, HashMap::new())
            .with_queue(queue)
            .with_routing_mode(mode)
            .with_provider_weights(provider_weights)
    }

    fn first_hops(router: &Router, model: &str, rounds: usize) -> Vec<String> {
        (0..rounds)
            .map(|_| {
                router
                    .resolve_route(model)
                    .expect("chain resolves")
                    .first()
                    .expect("non-empty chain")
                    .provider
                    .clone()
            })
            .collect()
    }

    #[test]
    fn order_mode_keeps_the_configured_chain_order() {
        let router = spread_router(Routing_mode::Order, &[("a", 1), ("b", 1), ("c", 1)]);
        assert_eq!(
            first_hops(&router, "m", 5),
            vec!["a", "a", "a", "a", "a"],
            "default order must be byte-for-byte the historical behavior"
        );
        let hops = router.resolve_route("m").expect("chain resolves");
        assert_eq!(
            hop_models(&hops),
            vec![
                ("a".into(), "m".into()),
                ("b".into(), "m".into()),
                ("c".into(), "m".into())
            ]
        );
    }

    /// Minimal provider whose only configurable fact is its wire protocol.
    struct ProtocolProvider {
        id: &'static str,
        protocol: &'static str,
        breaker: CircuitBreaker,
    }

    impl ProtocolProvider {
        fn new(id: &'static str, protocol: &'static str) -> Self {
            Self {
                id,
                protocol,
                breaker: CircuitBreaker::new(empty_cb()),
            }
        }
    }

    #[async_trait]
    impl Provider for ProtocolProvider {
        fn id(&self) -> &str {
            self.id
        }
        fn enabled(&self) -> bool {
            true
        }
        fn models(&self) -> &[String] {
            &[]
        }
        fn breaker(&self) -> &CircuitBreaker {
            &self.breaker
        }
        fn wire_protocol(&self) -> &'static str {
            self.protocol
        }

        async fn complete(
            &self,
            _req: &ChatCompletionRequest,
            _upstream_model: &str,
        ) -> Result<OAIChatResponse, ProviderError> {
            Err(canonical::make_error("unused", Some(500), Some(true)))
        }

        async fn stream(
            &self,
            _req: &ChatCompletionRequest,
            _upstream_model: &str,
        ) -> Result<Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>>, ProviderError> {
            Err(canonical::make_error("unused", Some(500), Some(true)))
        }
    }

    fn planned_hop(provider: &str, priority: i64, tier: Option<u8>) -> crate::proxy::planner::Planned_hop {
        crate::proxy::planner::Planned_hop {
            provider: provider.into(),
            model: "m".into(),
            tier,
            priority,
            basis: String::new(),
            downgraded: false,
            reason: String::new(),
        }
    }

    #[test]
    fn native_protocol_hops_lead_their_tier_over_configured_priority() {
        let providers: HashMap<String, Arc<dyn Provider>> = [
            ("native", "anthropic"),
            ("foreign", "openai-chat"),
        ]
        .into_iter()
        .map(|(id, protocol)| {
            (
                id.to_string(),
                Arc::new(ProtocolProvider::new(id, protocol)) as Arc<dyn Provider>,
            )
        })
        .collect();
        let router = Router::new(providers, HashMap::new());
        // Foreign has the better (lower) priority; the native-protocol hop
        // still leads its tier on the Claude surface.
        let planned = vec![planned_hop("foreign", 1, Some(2)), planned_hop("native", 9, Some(2))];
        let hops = router.restrict_planned_route(SURFACE_CLAUDE, "m", planned);
        assert_eq!(
            hops.iter().map(|hop| hop.provider.as_str()).collect::<Vec<_>>(),
            vec!["native", "foreign"]
        );
        // The capability-tier boundary stays intact: tiers never mix.
        let planned = vec![planned_hop("foreign", 1, Some(3)), planned_hop("native", 9, Some(2))];
        let hops = router.restrict_planned_route(SURFACE_CLAUDE, "m", planned);
        assert_eq!(
            hops.iter().map(|hop| hop.provider.as_str()).collect::<Vec<_>>(),
            vec!["foreign", "native"]
        );
        // Explicit opt-out restores priority ordering inside the tier.
        let router = router.with_native_protocol_preference(false);
        let planned = vec![planned_hop("foreign", 1, Some(2)), planned_hop("native", 9, Some(2))];
        let hops = router.restrict_planned_route(SURFACE_CLAUDE, "m", planned);
        assert_eq!(
            hops.iter().map(|hop| hop.provider.as_str()).collect::<Vec<_>>(),
            vec!["foreign", "native"]
        );
    }

    #[test]
    fn arena_preview_does_not_advance_live_rotation_and_retains_pin() {
        let router = spread_router(Routing_mode::Rotate, &[("a",1),("b",1),("c",1)]);
        let features = crate::proxy::planner::Request_features::default();
        for _ in 0..3 {
            assert_eq!(router.preview_model(SURFACE_CHAT,"m",features,None).unwrap()[0].provider,"a");
        }
        assert_eq!(first_hops(&router,"m",2),vec!["a","b"]);
        let pinned=router.preview_model(SURFACE_CHAT,"m",features,Some("b")).unwrap();
        assert_eq!(pinned.len(),1); assert_eq!(pinned[0].provider,"b");
    }

    #[test]
    fn rotate_mode_cycles_the_first_hop_each_in_turn() {
        let router = spread_router(Routing_mode::Rotate, &[("a", 1), ("b", 1), ("c", 1)]);
        assert_eq!(
            first_hops(&router, "m", 4),
            vec!["a", "b", "c", "a"],
            "rotate must hand the first position to each member in turn"
        );
        // Failover stays intact: every resolved chain is a rotation of all
        // three members, so a dead first hop still falls through.
        for _ in 0..3 {
            let hops = router.resolve_route("m").expect("chain resolves");
            let mut providers: Vec<String> = hops.iter().map(|hop| hop.provider.clone()).collect();
            providers.sort();
            assert_eq!(providers, vec!["a", "b", "c"]);
        }
    }

    #[test]
    fn rotate_shares_one_counter_per_chain_not_per_model() {
        // Two models over the same chain share the rotation: consecutive
        // requests for different models advance the same spread.
        let router = spread_router(Routing_mode::Rotate, &[("a", 1), ("b", 1)]);
        assert_eq!(
            first_hops(&router, "m", 1),
            vec!["a"],
            "first request opens on a"
        );
        assert_eq!(
            first_hops(&router, "n", 1),
            vec!["b"],
            "the second model continues the same chain's rotation"
        );
        assert_eq!(first_hops(&router, "m", 1), vec!["a"]);
    }

    #[test]
    fn weight_mode_is_smooth_weighted_round_robin() {
        // nginx-style SWRR: weights 3 and 1 go a, a, b, a…
        let router = spread_router(Routing_mode::Weight, &[("a", 3), ("b", 1)]);
        assert_eq!(
            first_hops(&router, "m", 4),
            vec!["a", "a", "b", "a"],
            "3:1 must spread smoothly, not burst a three then b"
        );
    }

    #[test]
    fn weight_zero_is_standby_only_but_still_in_the_chain() {
        let router = spread_router(Routing_mode::Weight, &[("a", 0), ("b", 1), ("c", 1)]);
        let heads = first_hops(&router, "m", 6);
        assert!(
            heads.iter().all(|id| id != "a"),
            "a zero-weight member must never be picked first"
        );
        // Even spread between the positive members.
        assert_eq!(
            heads.iter().filter(|id| *id == "b").count(),
            3,
            "b and c share the first position evenly"
        );
        let hops = router.resolve_route("m").expect("chain resolves");
        assert_eq!(hops.len(), 3, "standby members stay available as failover");
    }

    #[test]
    fn explicit_routes_are_never_reordered() {
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        providers.insert("a".into(), Arc::new(SpreadProvider::new("a", &["m"])));
        providers.insert("b".into(), Arc::new(SpreadProvider::new("b", &["m"])));
        let mut routes: HashMap<String, Vec<RouteHop>> = HashMap::new();
        routes.insert(
            "m".into(),
            vec![
                RouteHop {
                    provider: "a".into(),
                    model: "m".into(),
                },
                RouteHop {
                    provider: "b".into(),
                    model: "m".into(),
                },
            ],
        );
        let router = Router::new(providers, routes).with_routing_mode(Routing_mode::Rotate);
        for _ in 0..4 {
            let hops = router.resolve_route("m").expect("route resolves");
            assert_eq!(
                hop_models(&hops),
                vec![("a".into(), "m".into()), ("b".into(), "m".into())],
                "a hand-ordered route is a priority statement; spread modes must not touch it"
            );
        }
    }

    #[test]
    fn routing_mode_parses_leniently_and_round_trips() {
        assert_eq!(Routing_mode::parse(None), Routing_mode::Order);
        assert_eq!(Routing_mode::parse(Some("nonsense")), Routing_mode::Order);
        assert_eq!(Routing_mode::parse(Some(" rotate ")), Routing_mode::Rotate);
        assert_eq!(Routing_mode::parse(Some("weight")), Routing_mode::Weight);
        assert_eq!(Routing_mode::parse(Some("usage")), Routing_mode::Usage);
        for mode in [
            Routing_mode::Order,
            Routing_mode::Rotate,
            Routing_mode::Weight,
            Routing_mode::Usage,
        ] {
            assert_eq!(Routing_mode::parse(Some(mode.as_str())), mode);
        }
    }

    // ---- Usage mode (least recorded traffic first) ----

    fn usage_router(providers: &[(&'static str, u32)]) -> Router {
        spread_router(Routing_mode::Usage, providers)
    }

    fn provider_order(router: &Router, model: &str) -> Vec<String> {
        router
            .resolve_route(model)
            .expect("chain resolves")
            .iter()
            .map(|hop| hop.provider.clone())
            .collect()
    }

    #[test]
    fn usage_mode_puts_the_least_used_member_first() {
        let router = usage_router(&[("a", 1), ("b", 1), ("c", 1)]);
        {
            let mut ledger = router.usage_ledger.lock().unwrap();
            ledger.record("a", 900, 100);
            ledger.record("b", 10, 0);
            ledger.record("c", 100, 50);
        }
        assert_eq!(
            provider_order(&router, "m"),
            vec!["b", "c", "a"],
            "the chain must open on the least recorded traffic, not the configured head"
        );
        // Ordering is a spread, not a filter: every member stays reachable.
        let mut all = provider_order(&router, "m");
        all.sort();
        assert_eq!(all, vec!["a", "b", "c"]);
    }

    #[test]
    fn usage_mode_ties_keep_the_configured_order() {
        let router = usage_router(&[("a", 1), ("b", 1), ("c", 1)]);
        {
            let mut ledger = router.usage_ledger.lock().unwrap();
            ledger.record("a", 100, 0);
            ledger.record("b", 50, 0);
            ledger.record("c", 100, 0);
        }
        assert_eq!(
            provider_order(&router, "m"),
            vec!["b", "a", "c"],
            "a and c tie at 100 tokens; the configured order must break the tie"
        );
    }

    #[test]
    fn usage_mode_empty_ledger_keeps_the_configured_order() {
        let router = usage_router(&[("a", 1), ("b", 1), ("c", 1)]);
        assert_eq!(
            provider_order(&router, "m"),
            vec!["a", "b", "c"],
            "no observed traffic is a tie, so a cold chain keeps its order"
        );
        assert_eq!(provider_order(&router, "n"), vec!["a", "b", "c"]);
    }

    #[test]
    fn usage_ledger_window_rolls_over_per_provider() {
        let mut ledger = Usage_ledger::with_window(std::time::Duration::from_secs(60));
        let base = Instant::now();
        ledger.record_at("a", 100, base);
        assert_eq!(
            ledger.tokens_at("a", base + std::time::Duration::from_secs(59)),
            100,
            "traffic inside the window ranks"
        );
        assert_eq!(
            ledger.tokens_at("a", base + std::time::Duration::from_secs(61)),
            0,
            "traffic older than the window must be forgotten"
        );
        ledger.record_at("a", 7, base + std::time::Duration::from_secs(61));
        assert_eq!(
            ledger.tokens_at("a", base + std::time::Duration::from_secs(61)),
            7,
            "the first request after expiry starts a fresh bucket"
        );
        assert_eq!(
            ledger.tokens_at("a", base + std::time::Duration::from_secs(122)),
            0
        );
    }

    #[test]
    fn usage_mode_window_rollover_restores_the_configured_order() {
        let router = usage_router(&[("a", 1), ("b", 1)]);
        // A one-nanosecond window makes the recorded bucket already stale by
        // the time resolution reads it — no sleep, no wall-clock flake.
        *router.usage_ledger.lock().unwrap() =
            Usage_ledger::with_window(std::time::Duration::from_nanos(1));
        router.usage_ledger.lock().unwrap().record("a", 5_000, 0);
        assert_eq!(
            first_hops(&router, "m", 1),
            vec!["a"],
            "traffic that aged out of the window must not rank the chain"
        );
        // A live window re-ranks the same observation.
        *router.usage_ledger.lock().unwrap() =
            Usage_ledger::with_window(std::time::Duration::from_secs(60));
        router.usage_ledger.lock().unwrap().record("a", 5_000, 0);
        assert_eq!(
            first_hops(&router, "m", 1),
            vec!["b"],
            "fresh traffic must still move the busy provider behind its idle peer"
        );
    }

    #[test]
    fn usage_ledger_is_bounded_by_a_wholesale_reset() {
        let mut ledger = Usage_ledger::default();
        let now = Instant::now();
        for index in 0..=USAGE_LEDGER_MAX_PROVIDERS {
            ledger.record_at(&format!("p{index}"), 1, now);
        }
        assert!(
            ledger.buckets.len() <= USAGE_LEDGER_MAX_PROVIDERS,
            "the ledger must never grow past its provider cap"
        );
        // Losing the old buckets is benign: the newest observation survives.
        ledger.record_at("workload", 5, now);
        assert_eq!(ledger.tokens_at("workload", now), 5);
    }

    #[test]
    fn usage_mode_rebuild_carries_the_ledger() {
        let first = usage_router(&[("a", 1), ("b", 1)]);
        first.usage_ledger.lock().unwrap().record("a", 5_000, 0);
        assert_eq!(first_hops(&first, "m", 1), vec!["b"]);
        // Snapshot rebuilds hand the same Arc to the next generation (see
        // `build_runtime_snapshot_from`); simulating that must keep traffic.
        let mut second = usage_router(&[("a", 1), ("b", 1)]);
        second.usage_ledger = Arc::clone(&first.usage_ledger);
        assert_eq!(
            first_hops(&second, "m", 1),
            vec!["b"],
            "a hot-apply rebuild must not reset usage accounting"
        );
    }

    #[test]
    fn usage_mode_never_reorders_explicit_routes_or_planner_plans() {
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        providers.insert("a".into(), Arc::new(SpreadProvider::new("a", &["m"])));
        providers.insert("b".into(), Arc::new(SpreadProvider::new("b", &["m"])));
        let mut routes: HashMap<String, Vec<RouteHop>> = HashMap::new();
        routes.insert(
            "m".into(),
            vec![
                RouteHop {
                    provider: "a".into(),
                    model: "m".into(),
                },
                RouteHop {
                    provider: "b".into(),
                    model: "m".into(),
                },
            ],
        );
        let router = Router::new(providers, routes)
            .with_routing_mode(Routing_mode::Usage)
            .with_planner(Arc::new(Stub_planner));
        {
            let mut ledger = router.usage_ledger.lock().unwrap();
            // Skew both chains toward the member that is *not* first, so a
            // wrong spread would visibly reorder them.
            ledger.record("a", 10_000, 0);
            ledger.record("b", 0, 0);
            ledger.record("p1", 0, 0);
            ledger.record("p2", 10_000, 0);
        }
        assert_eq!(
            hop_models(&router.resolve_route("m").expect("route resolves")),
            vec![("a".into(), "m".into()), ("b".into(), "m".into())],
            "a hand-ordered route is a priority statement; usage mode must not touch it"
        );
        // Planner alias plan p1 -> p2, ledger skewed toward p2.
        let planned = router
            .resolve_request("claude", &req_for("stub-alias-ok"))
            .expect("alias resolves");
        assert_eq!(
            hop_models(&planned),
            vec![
                ("p1".into(), "strong-model".into()),
                ("p2".into(), "backup-model".into())
            ],
            "the planner's tier order is an assessment; usage mode must not touch it"
        );
    }

    /// A successful non-streaming answer feeds the routing ledger on the
    /// same success path as usage persistence.
    #[tokio::test]
    async fn complete_success_feeds_the_usage_ledger() {
        use crate::proxy::providers::mock::MockProvider;
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        providers.insert(
            "mock".into(),
            Arc::new(MockProvider::new(
                "mock".into(),
                vec!["m".into()],
                true,
                CircuitBreakerConfig::default(),
            )),
        );
        let router = Router::new(providers, HashMap::new())
            .with_queue(vec!["mock".to_string()])
            .with_routing_mode(Routing_mode::Usage);
        let resp = router.complete(&req_for("m")).await.expect("mock answers");
        let spent = resp
            .usage
            .as_ref()
            .map(|usage| usage.prompt_tokens as u64 + usage.completion_tokens as u64)
            .expect("mock reports usage");
        assert_eq!(
            router.usage_ledger.lock().unwrap().tokens("mock"),
            spent,
            "the recorder call site must mirror prompt + completion into the ledger"
        );
    }

    // ---- provider terminal-attempt health observation and deferral ----

    /// A provider whose `complete` outcome is scripted and switchable at
    /// runtime, so one member of a failover chain can fail terminally while
    /// a peer serves, and can recover or degrade across test phases.
    struct Outcome_provider {
        id: String,
        models: Vec<String>,
        breaker: CircuitBreaker,
        fail: std::sync::atomic::AtomicBool,
        error: (&'static str, Option<u16>, bool),
        calls: std::sync::atomic::AtomicUsize,
    }

    impl Outcome_provider {
        fn working(id: &str, models: &[&str]) -> Arc<Self> {
            Arc::new(Self {
                id: id.into(),
                models: models.iter().map(|model| model.to_string()).collect(),
                breaker: CircuitBreaker::new(empty_cb()),
                fail: std::sync::atomic::AtomicBool::new(false),
                error: ("provider_error", Some(500), true),
                calls: std::sync::atomic::AtomicUsize::new(0),
            })
        }

        fn failing(id: &str, models: &[&str], code: &'static str, status: Option<u16>, retriable: bool) -> Arc<Self> {
            Arc::new(Self {
                id: id.into(),
                models: models.iter().map(|model| model.to_string()).collect(),
                breaker: CircuitBreaker::new(empty_cb()),
                fail: std::sync::atomic::AtomicBool::new(true),
                error: (code, status, retriable),
                calls: std::sync::atomic::AtomicUsize::new(0),
            })
        }

        fn set_fail(&self, fail: bool) {
            self.fail.store(fail, std::sync::atomic::Ordering::SeqCst);
        }

        fn calls(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl Provider for Outcome_provider {
        fn id(&self) -> &str {
            &self.id
        }
        fn enabled(&self) -> bool {
            true
        }
        fn models(&self) -> &[String] {
            &self.models
        }
        fn breaker(&self) -> &CircuitBreaker {
            &self.breaker
        }

        async fn complete(
            &self,
            _req: &ChatCompletionRequest,
            upstream_model: &str,
        ) -> Result<OAIChatResponse, ProviderError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
                let (code, status, retriable) = self.error;
                return Err(canonical::make_provider_error(
                    code,
                    "response_status",
                    status,
                    Some(retriable),
                    None,
                ));
            }
            Ok(OAIChatResponse {
                id: "outcome".into(),
                object: "chat.completion".into(),
                created: 0,
                model: upstream_model.into(),
                choices: vec![crate::proxy::canonical::OAIChoice {
                    index: 0,
                    message: crate::proxy::canonical::OAIMessage {
                        role: "assistant".into(),
                        content: Some(format!("ok:{}", self.id)),
                        tool_calls: None,
                        refusal: None,
                        extra: Default::default(),
                    },
                    finish_reason: Some("stop".into()),
                    extra: Default::default(),
                }],
                usage: None,
                extra: Default::default(),
                source_anthropic_response: None,
                source_responses_response: None,
                source_gemini_response: None,
            })
        }

        async fn stream(
            &self,
            _req: &ChatCompletionRequest,
            upstream_model: &str,
        ) -> Result<Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>>, ProviderError> {
            let model = upstream_model.to_string();
            let first = crate::proxy::converter::make_stream_chunk(
                "id",
                &model,
                crate::proxy::canonical::StreamDelta {
                    role: Some("assistant".into()),
                    content: Some(format!("ok:{}", self.id)),
                    tool_calls: None,
                    reasoning: None,
                    extra: Default::default(),
                },
                None,
            );
            let last = crate::proxy::converter::make_stream_chunk(
                "id",
                &model,
                crate::proxy::canonical::StreamDelta {
                    role: None,
                    content: None,
                    tool_calls: None,
                    reasoning: None,
                    extra: Default::default(),
                },
                Some("stop".into()),
            );
            Ok(Box::pin(futures_util::stream::iter(vec![first, last])))
        }

        async fn count_tokens(
            &self,
            _body: serde_json::Value,
            _client_credential: Option<&crate::proxy::canonical::Client_credential>,
        ) -> Result<u32, ProviderError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
                let (code, status, retriable) = self.error;
                return Err(canonical::make_provider_error(
                    code,
                    "response_status",
                    status,
                    Some(retriable),
                    None,
                ));
            }
            Ok(7)
        }
    }

    /// A provider whose latest terminal inference attempt failed must defer
    /// behind a ready peer in the same queue chain while its failure cooldown
    /// lasts. The failed provider stays a fallback; it just no longer leads.
    #[tokio::test]
    async fn failed_provider_defers_behind_ready_peer_in_queue_chain() {
        let a = Outcome_provider::failing("a", &["m"], "upstream_http_error", Some(503), true);
        let b = Outcome_provider::working("b", &["m"]);
        let providers: HashMap<String, Arc<dyn Provider>> = HashMap::from([
            ("a".into(), a as Arc<dyn Provider>),
            ("b".into(), b),
        ]);
        let router = Router::new(providers, HashMap::new())
            .with_queue(vec!["a".to_string(), "b".to_string()]);
        let first = router
            .complete_for(SURFACE_CHAT, "_test", &req_for("m"))
            .await
            .expect("the healthy peer must serve after the failed hop");
        assert_eq!(
            first.choices[0].message.content.as_deref(),
            Some("ok:b"),
            "failover must reach the healthy peer"
        );
        let resolved = router
            .resolve_request(SURFACE_CHAT, &req_for("m"))
            .expect("resolve after the failure");
        assert_eq!(
            resolved
                .iter()
                .map(|hop| hop.provider.as_str())
                .collect::<Vec<_>>(),
            ["b", "a"],
            "the failed provider is deferred but retained as fallback"
        );
    }

    /// The provider health snapshot must carry the derived terminal-outcome
    /// failure view (serialized seam used by /health and the Arena preview).
    #[test]
    fn health_snapshot_exposes_derived_provider_failure_data() {
        let providers: HashMap<String, Arc<dyn Provider>> = HashMap::from([(
            "b".into(),
            Outcome_provider::working("b", &["m"]) as Arc<dyn Provider>,
        )]);
        let router = Router::new(providers, HashMap::new());
        let serialized = serde_json::to_value(router.health()).expect("serialize health");
        let entry = &serialized[0];
        assert!(
            entry.get("failure").is_some(),
            "provider health must carry the derived failure view, got {entry}"
        );
    }

    /// A streaming provider with a fixed body scenario for the terminal
    /// outcome matrix.
    struct Stream_provider {
        breaker: CircuitBreaker,
        midstream_error: bool,
        omit_terminal: bool,
    }

    impl Stream_provider {
        fn new(midstream_error: bool, omit_terminal: bool) -> Arc<Self> {
            Arc::new(Self {
                breaker: CircuitBreaker::new(empty_cb()),
                midstream_error,
                omit_terminal,
            })
        }
    }

    #[async_trait]
    impl Provider for Stream_provider {
        fn id(&self) -> &str {
            "s"
        }
        fn enabled(&self) -> bool {
            true
        }
        fn models(&self) -> &[String] {
            &[]
        }
        fn breaker(&self) -> &CircuitBreaker {
            &self.breaker
        }

        async fn complete(
            &self,
            _req: &ChatCompletionRequest,
            _upstream_model: &str,
        ) -> Result<OAIChatResponse, ProviderError> {
            Err(canonical::make_error("unused", Some(500), Some(true)))
        }

        async fn stream(
            &self,
            _req: &ChatCompletionRequest,
            _upstream_model: &str,
        ) -> Result<Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>>, ProviderError> {
            let first = crate::proxy::converter::make_stream_chunk(
                "id",
                "m",
                crate::proxy::canonical::StreamDelta {
                    role: Some("assistant".into()),
                    content: Some("ok:s".into()),
                    tool_calls: None,
                    reasoning: None,
                    extra: Default::default(),
                },
                None,
            );
            let chunks = if self.midstream_error {
                vec![
                    first,
                    canonical::stream_error_chunk("m", 503, "upstream_http_error"),
                ]
            } else if self.omit_terminal {
                vec![first]
            } else {
                vec![
                    first,
                    crate::proxy::converter::make_stream_chunk(
                        "id",
                        "m",
                        crate::proxy::canonical::StreamDelta {
                            role: None,
                            content: None,
                            tool_calls: None,
                            reasoning: None,
                            extra: Default::default(),
                        },
                        Some("stop".into()),
                    ),
                ]
            };
            Ok(Box::pin(futures_util::stream::iter(chunks)))
        }
    }

    fn single_hop_router(provider: Arc<dyn Provider>) -> Router {
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        providers.insert("s".into(), provider);
        Router::new(providers, HashMap::new())
    }

    async fn stream_hops_direct(router: &Router, req: &ChatCompletionRequest) -> Result<futures_util::stream::BoxStream<'static, OAIStreamChunk>, ProviderError> {
        router
            .stream_hops(
                "",
                "_test",
                req,
                Arc::new(vec![RouteHop {
                    provider: "s".into(),
                    model: "m".into(),
                }]),
            )
            .await
    }

    fn stream_failure_view(router: &Router) -> Provider_failure_health {
        router.provider_failure_health("s")
    }

    #[tokio::test]
    async fn stream_terminal_success_records_ok_exactly_once() {
        let router = single_hop_router(Stream_provider::new(false, false));
        let stream = stream_hops_direct(&router, &req_for("m")).await.expect("stream");
        let chunks: Vec<OAIStreamChunk> = stream.collect().await;
        assert_eq!(chunks.len(), 2);
        let view = stream_failure_view(&router);
        assert!(view.observed);
        assert_eq!(view.latest_outcome, Some("ok"));
        assert_eq!(view.failure_count, 0, "terminal success is not a failure");
        assert_eq!(router.model_health("s", "m").samples, 1, "exactly one sample");
    }

    #[tokio::test]
    async fn stream_midstream_error_records_failure_exactly_once() {
        let router = single_hop_router(Stream_provider::new(true, false));
        let stream = stream_hops_direct(&router, &req_for("m")).await.expect("stream");
        let chunks: Vec<OAIStreamChunk> = stream.collect().await;
        assert_eq!(chunks.len(), 2, "the error chunk surfaces to the client");
        let view = stream_failure_view(&router);
        assert_eq!(view.latest_outcome, Some("failed"));
        assert_eq!(
            view.last_failure.as_ref().expect("failure facts").reason,
            "upstream_stream_error"
        );
        assert_eq!(view.failure_count, 1, "exactly one terminal record");
    }

    #[tokio::test]
    async fn stream_missing_terminal_records_failure_exactly_once() {
        let router = single_hop_router(Stream_provider::new(false, true));
        let stream = stream_hops_direct(&router, &req_for("m")).await.expect("stream");
        let chunks: Vec<OAIStreamChunk> = stream.collect().await;
        assert_eq!(chunks.len(), 2, "a synthesized terminal error chunk closes the body");
        let view = stream_failure_view(&router);
        assert_eq!(view.latest_outcome, Some("failed"));
        assert_eq!(view.failure_count, 1);
        assert_eq!(
            view.last_failure.as_ref().expect("failure facts").reason,
            "upstream_stream_error",
            "a drained stream without terminal evidence is not a success"
        );
    }

    #[tokio::test]
    async fn dropped_stream_records_cancelled_failure_exactly_once() {
        let router = single_hop_router(Stream_provider::new(false, false));
        let mut stream = stream_hops_direct(&router, &req_for("m")).await.expect("stream");
        let first = stream.next().await.expect("first chunk");
        assert!(canonical::stream_chunk_error(&first).is_none());
        drop(stream);
        let view = stream_failure_view(&router);
        assert_eq!(view.latest_outcome, Some("failed"));
        assert_eq!(view.failure_count, 1, "drop records the terminal outcome once");
        assert_eq!(
            view.last_failure.as_ref().expect("failure facts").reason,
            "cancelled"
        );
    }

    fn unpreparable_request(model: &str) -> ChatCompletionRequest {
        let mut req = req_for(model);
        // A Gemini-native source body with an unsupported top-level control
        // fails hop preparation before any provider method runs.
        req.source_gemini_request = Some(serde_json::json!({
            "contents": [],
            "unsupported_native_control": {"x": 1}
        }));
        req
    }

    #[tokio::test]
    async fn prep_rejection_records_no_terminal_sample_and_never_calls_the_provider() {
        let provider = Outcome_provider::working("a", &["m"]);
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        providers.insert("a".into(), provider.clone() as Arc<dyn Provider>);
        let router = Router::new(providers, HashMap::new())
            .with_queue(vec!["a".to_string()]);
        let result = router
            .complete_for(SURFACE_CHAT, "_test", &unpreparable_request("m"))
            .await;
        assert!(result.is_err(), "the unsupported native control must fail admission");
        assert_eq!(provider.calls(), 0, "no provider method may run");
        let view = router.provider_failure_health("a");
        assert!(!view.observed, "a rejected preparation is not an inference attempt");
        assert_eq!(view.failure_count, 0);
        assert_eq!(router.model_health("a", "m").samples, 0);
    }

    #[tokio::test]
    async fn prep_rejection_on_stream_records_no_terminal_sample_and_never_calls_the_provider() {
        let provider = Outcome_provider::working("a", &["m"]);
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        providers.insert("a".into(), provider.clone() as Arc<dyn Provider>);
        let router = Router::new(providers, HashMap::new())
            .with_queue(vec!["a".to_string()]);
        let result = router
            .stream_for(SURFACE_CHAT, "_test", &unpreparable_request("m"))
            .await;
        assert!(result.is_err(), "the unsupported native control must fail admission");
        assert_eq!(provider.calls(), 0, "no provider method may run");
        let view = router.provider_failure_health("a");
        assert!(!view.observed);
        assert_eq!(router.model_health("a", "m").samples, 0);
    }

    #[tokio::test]
    async fn count_tokens_neither_records_nor_clears_inference_health() {
        let provider = Outcome_provider::working("a", &["m"]);
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        providers.insert("a".into(), provider.clone() as Arc<dyn Provider>);
        let router = Router::new(providers, HashMap::new())
            .with_queue(vec!["a".to_string()]);
        let tokens = router
            .count_tokens_for("", "m", &serde_json::json!({"model": "m"}), None)
            .await
            .expect("token count succeeds");
        assert_eq!(tokens, 7);
        assert!(
            !router.provider_failure_health("a").observed,
            "a token count is not an inference outcome"
        );
        provider.set_fail(true);
        assert!(router
            .complete_for(SURFACE_CHAT, "_test", &req_for("m"))
            .await
            .is_err());
        let after_failure = router.provider_failure_health("a");
        assert_eq!(after_failure.failure_count, 1);
        assert_eq!(after_failure.latest_outcome, Some("failed"));
        assert!(router
            .count_tokens_for("", "m", &serde_json::json!({"model": "m"}), None)
            .await
            .is_err());
        let unchanged = router.provider_failure_health("a");
        assert_eq!(unchanged.failure_count, 1, "count attempts never touch inference health");
        assert_eq!(unchanged.latest_outcome, Some("failed"));
    }

    #[tokio::test]
    async fn cooldown_expiry_restores_the_configured_order_without_probes() {
        let a = Outcome_provider::failing("a", &["m"], "upstream_http_error", Some(503), true);
        let b = Outcome_provider::working("b", &["m"]);
        let providers: HashMap<String, Arc<dyn Provider>> = HashMap::from([
            ("a".into(), a.clone() as Arc<dyn Provider>),
            ("b".into(), b),
        ]);
        let router = Router::new(providers, HashMap::new())
            .with_queue(vec!["a".to_string(), "b".to_string()]);
        let response = router
            .complete_for(SURFACE_CHAT, "_test", &req_for("m"))
            .await
            .expect("failover serves the healthy peer");
        assert_eq!(response.choices[0].message.content.as_deref(), Some("ok:b"));
        let deferred = router
            .preview_model(SURFACE_CHAT, "m", Default::default(), None)
            .expect("preview while cooling");
        assert_eq!(
            deferred.iter().map(|hop| hop.provider.as_str()).collect::<Vec<_>>(),
            ["b", "a"]
        );
        router
            .model_observations
            .lock()
            .unwrap()
            .expire_failure_cooldown(&Health_identity::Proxy { id: "a".into() }, "m");
        let restored = router
            .preview_model(SURFACE_CHAT, "m", Default::default(), None)
            .expect("preview after cooldown");
        assert_eq!(
            restored.iter().map(|hop| hop.provider.as_str()).collect::<Vec<_>>(),
            ["a", "b"],
            "cooldown expiry restores the configured order"
        );
        assert_eq!(a.calls(), 1, "recovery needs no probe: nothing new was dispatched");
    }

    /// Exactly one SWRR advance over `bucket`: every member gains its weight
    /// once and the winner additionally pays the bucket total. Returns the
    /// winner derived from the deltas.
    fn assert_one_swrr_advance(
        before: &HashMap<String, i64>,
        after: &HashMap<String, i64>,
        bucket: &[(&str, i64)],
    ) -> String {
        let total: i64 = bucket.iter().map(|(_, weight)| *weight).sum();
        let mut winner = None;
        for (id, weight) in bucket {
            let prior = before.get(*id).copied().unwrap_or(0);
            let delta = after.get(*id).copied().unwrap_or(0) - prior;
            if delta == weight - total {
                winner = Some((*id).to_string());
            } else {
                assert_eq!(delta, *weight, "provider {id} must advance exactly once");
            }
        }
        winner.expect("exactly one winner per advance")
    }

    fn spread_currents(router: &Router) -> HashMap<String, i64> {
        router
            .chain_spread
            .lock()
            .unwrap()
            .currents
            .clone()
    }

    fn spread_delta(
        before: &HashMap<String, i64>,
        after: &HashMap<String, i64>,
        id: &str,
    ) -> i64 {
        after.get(id).copied().unwrap_or(0) - before.get(id).copied().unwrap_or(0)
    }

    fn preview_order(router: &Router) -> Vec<String> {
        router
            .preview_model(SURFACE_CHAT, "m", Default::default(), None)
            .expect("preview")
            .iter()
            .map(|hop| hop.provider.clone())
            .collect()
    }

    fn served_by(response: &OAIChatResponse) -> &str {
        response.choices[0].message.content.as_deref().unwrap_or_default()
    }

    #[tokio::test]
    async fn weight_spread_runs_once_on_the_ready_bucket_and_appends_deferred_members() {
        let a = Outcome_provider::working("a", &["m"]);
        let b = Outcome_provider::working("b", &["m"]);
        let c = Outcome_provider::working("c", &["m"]);
        let providers: HashMap<String, Arc<dyn Provider>> = HashMap::from([
            ("a".into(), a.clone() as Arc<dyn Provider>),
            ("b".into(), b.clone()),
            ("c".into(), c.clone()),
        ]);
        let router = Router::new(providers, HashMap::new())
            .with_queue(vec!["a".to_string(), "b".to_string(), "c".to_string()])
            .with_routing_mode(Routing_mode::Weight)
            .with_provider_weights(HashMap::from([
                ("a".to_string(), 5),
                ("b".to_string(), 1),
                ("c".to_string(), 1),
            ]));
        let full_bucket = [("a", 5_i64), ("b", 1), ("c", 1)];
        let ready_bucket = [("b", 1_i64), ("c", 1)];

        // Request 1 (all healthy): one full-chain advance, exactly as before.
        let before = spread_currents(&router);
        let first = router
            .complete_for(SURFACE_CHAT, "_test", &req_for("m"))
            .await
            .expect("chain head serves");
        assert_eq!(served_by(&first), format!("ok:{}", assert_one_swrr_advance(&before, &spread_currents(&router), &full_bucket)));

        // Request 2: a fails terminally and a ready peer serves; the failure
        // puts a into the deferral bucket for every later resolve.
        a.set_fail(true);
        let second = router
            .complete_for(SURFACE_CHAT, "_test", &req_for("m"))
            .await
            .expect("a ready peer serves");
        assert!(matches!(served_by(&second), "ok:b" | "ok:c"));

        // Request 3: only the ready bucket spreads (once) and a is appended
        // as the deferred fallback; a consumes no spread turn.
        let before = spread_currents(&router);
        let third = router
            .complete_for(SURFACE_CHAT, "_test", &req_for("m"))
            .await
            .expect("a ready peer serves again");
        let after = spread_currents(&router);
        let ready_winner = assert_one_swrr_advance(&before, &after, &ready_bucket);
        assert_eq!(served_by(&third), format!("ok:{ready_winner}"));
        assert_eq!(spread_delta(&before, &after, "a"), 0, "the deferred member consumes no spread turn");
        let order = preview_order(&router);
        assert_eq!(order.last().map(String::as_str), Some("a"), "the cooling member is deferred last");
        assert!(
            !order.iter().take(2).any(|id| id == "a"),
            "both ready members precede the deferred member"
        );

        // Requests 4-5 drive the ready peers into their own failure cooldown;
        // with every member cooling, the resolve spreads the FULL chain once
        // and keeps every fallback present.
        b.set_fail(true);
        c.set_fail(true);
        assert!(router
            .complete_for(SURFACE_CHAT, "_test", &req_for("m"))
            .await
            .is_err());
        let before = spread_currents(&router);
        assert!(router
            .complete_for(SURFACE_CHAT, "_test", &req_for("m"))
            .await
            .is_err());
        let after = spread_currents(&router);
        assert_one_swrr_advance(&before, &after, &full_bucket);
        let order = preview_order(&router);
        assert_eq!(order.len(), 3, "an all-cooling chain keeps every fallback");
        let mut sorted_order = order.clone();
        sorted_order.sort();
        let mut expected = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        expected.sort();
        assert_eq!(sorted_order, expected);
    }

    #[tokio::test]
    async fn affinity_promoted_cooldown_member_is_redeferred_after_promotion() {
        let a = Outcome_provider::working("a", &["m"]);
        let b = Outcome_provider::working("b", &["m"]);
        let providers: HashMap<String, Arc<dyn Provider>> = HashMap::from([
            ("a".into(), a.clone() as Arc<dyn Provider>),
            ("b".into(), b.clone()),
        ]);
        let router = Router::new(providers, HashMap::new())
            .with_queue(vec!["a".to_string(), "b".to_string()]);
        let mut session_req = req_for("m");
        session_req
            .extra
            .insert("user".into(), serde_json::json!("session-1"));

        // The session binds to a while it serves.
        let first = router
            .complete_for(SURFACE_CHAT, "_test", &session_req)
            .await
            .expect("a serves the first session turn");
        assert_eq!(first.choices[0].message.content.as_deref(), Some("ok:a"));

        // a fails terminally and failover reaches b through a SESSION-LESS
        // request: without an affinity key no winner is recorded, so the
        // session stays bound to a while a enters its failure cooldown and
        // b's own terminal success leaves b a ready peer.
        a.set_fail(true);
        let second = router
            .complete_for(SURFACE_CHAT, "_test", &req_for("m"))
            .await
            .expect("b serves the failed turn");
        assert_eq!(second.choices[0].message.content.as_deref(), Some("ok:b"));

        // The session must still be bound to the cooling a, and affinity
        // must still promote it — this is exactly what the post-affinity
        // partition has to undo in the walker.
        let key =
            crate::proxy::session_affinity::affinity_key(&session_req).expect("session key");
        let bound = router.affinity.bound_hop(&key).expect("session binding");
        assert_eq!(
            bound.provider, "a",
            "the session must still be bound to the cooling member"
        );
        let resolved = router
            .resolve_request(SURFACE_CHAT, &session_req)
            .expect("resolve");
        let promoted = router.affinity.preferred_hops(Some(&key), resolved);
        assert_eq!(
            promoted.first().map(|hop| hop.provider.as_str()),
            Some("a"),
            "affinity promotes the cooling member ahead of the ready peer"
        );

        // Affinity still promotes the bound a to the front; the post-affinity
        // stable partition must put it back behind the ready peer without a
        // second dispatch of a.
        let third = router
            .complete_for(SURFACE_CHAT, "_test", &session_req)
            .await
            .expect("the ready peer serves the next turn");
        assert_eq!(third.choices[0].message.content.as_deref(), Some("ok:b"));
        assert_eq!(a.calls(), 2, "a was dispatched only for its two real attempts");
        assert_eq!(b.calls(), 2);
    }

    #[test]
    fn queue_health_projection_counts_disjoint_outcomes_and_unknown_ids() {
        let providers: HashMap<String, Arc<dyn Provider>> = HashMap::from([
            (
                "queue-x".into(),
                Outcome_provider::working("queue-x", &["m"]) as Arc<dyn Provider>,
            ),
            (
                "queue-claude_code-s".into(),
                Outcome_provider::working("queue-claude_code-s", &["m"]),
            ),
            (
                "queue-codex-s".into(),
                Outcome_provider::working("queue-codex-s", &["m"]),
            ),
        ]);
        let router = Router::new(providers, HashMap::new())
            .with_queue(vec!["queue-x".to_string()])
            .with_queues(HashMap::from([(
                SURFACE_CHAT.to_string(),
                vec![
                    "queue-claude_code-s".to_string(),
                    "queue-codex-s".to_string(),
                    "ghost".to_string(),
                ],
            )]))
            .with_queue_sources(HashMap::from([
                (
                    "queue-claude_code-s".to_string(),
                    Queue_source {
                        app_type: "claude_code".into(),
                        source_provider_id: "s".into(),
                    },
                ),
                (
                    "queue-codex-s".to_string(),
                    Queue_source {
                        app_type: "codex".into(),
                        source_provider_id: "s".into(),
                    },
                ),
                (
                    "queue-x".to_string(),
                    Queue_source {
                        app_type: "claude_code".into(),
                        source_provider_id: "x".into(),
                    },
                ),
            ]));
        // Same source row id "s" in two apps: distinct tagged identities, so
        // one can be failed and the other ok without merging.
        router.model_observations.lock().unwrap().record_terminal(
            &Health_identity::Queue {
                app_type: "claude_code".into(),
                source_provider_id: "s".into(),
            },
            "m",
            false,
            Some("upstream_http_error"),
            3.0,
        );
        router.model_observations.lock().unwrap().record_terminal(
            &Health_identity::Queue {
                app_type: "codex".into(),
                source_provider_id: "s".into(),
            },
            "m",
            true,
            None,
            4.0,
        );
        let projection = router.queue_health_projection();
        assert_eq!(projection.totals.total, 4);
        assert_eq!(projection.totals.ok, 1);
        assert_eq!(projection.totals.failed, 1);
        assert_eq!(projection.totals.unobserved, 1);
        assert_eq!(projection.totals.unresolved, 1);

        let by_id = |id: &str| {
            projection
                .candidates
                .iter()
                .find(|candidate| candidate.runtime_id == id)
                .unwrap_or_else(|| panic!("candidate {id} missing"))
        };
        let claude = by_id("queue-claude_code-s");
        assert_eq!(claude.failure.as_ref().unwrap().latest_outcome, Some("failed"));
        assert_eq!(
            claude.failure.as_ref().unwrap().last_failure.as_ref().unwrap().reason,
            "upstream_http_error"
        );
        let codex = by_id("queue-codex-s");
        assert_eq!(codex.failure.as_ref().unwrap().latest_outcome, Some("ok"));
        let direct = by_id("queue-x");
        assert!(direct.resolved);
        assert_eq!(
            direct.failure.as_ref().unwrap().latest_outcome,
            None,
            "resolved-but-unmeasured is absent outcome, not zero"
        );
        let ghost = by_id("ghost");
        assert!(!ghost.resolved, "configured unknown ids stay visible");
        assert!(ghost.failure.is_none(), "unresolved ids carry no measured health");

        let serialized = serde_json::to_value(&projection).expect("serialize projection");
        assert_eq!(serialized["totals"]["total"], 4);
        assert_eq!(serialized["candidates"][1]["runtimeId"], "queue-claude_code-s");
        assert_eq!(serialized["candidates"][1]["source"]["kind"], "queue");
        assert_eq!(serialized["candidates"][3]["resolved"], false);
    }

    #[tokio::test]
    async fn all_healthy_queue_keeps_exact_configured_behavior() {
        let a = Outcome_provider::working("a", &["m"]);
        let b = Outcome_provider::working("b", &["m"]);
        let c = Outcome_provider::working("c", &["m"]);
        let providers: HashMap<String, Arc<dyn Provider>> = HashMap::from([
            ("a".into(), a.clone() as Arc<dyn Provider>),
            ("b".into(), b),
            ("c".into(), c),
        ]);
        let router = Router::new(providers, HashMap::new())
            .with_queue(vec!["a".to_string(), "b".to_string(), "c".to_string()]);
        for _ in 0..2 {
            let response = router
                .complete_for(SURFACE_CHAT, "_test", &req_for("m"))
                .await
                .expect("configured head serves");
            assert_eq!(
                response.choices[0].message.content.as_deref(),
                Some("ok:a"),
                "a healthy chain head keeps serving in configured order"
            );
        }
        assert_eq!(a.calls(), 2);
        let resolved = router
            .resolve_request(SURFACE_CHAT, &req_for("m"))
            .expect("resolve");
        assert_eq!(
            resolved.iter().map(|hop| hop.provider.as_str()).collect::<Vec<_>>(),
            ["a", "b", "c"],
            "no deferral without failures"
        );
        let view = router.provider_failure_health("a");
        assert_eq!(view.latest_outcome, Some("ok"));
        assert!(!view.in_cooldown);
    }
}
