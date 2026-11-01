/// Shared wire DTOs for bounded provider terminal-outcome health.
/// Mirrors `Health_identity` in `src-tauri/src/proxy/model_health.rs` and
/// `Provider_failure_health`, `Provider_last_failure`,
/// `Queue_candidate_health`, `Queue_health_totals`,
/// `Queue_health_projection` in `src-tauri/src/proxy/router.rs`.
/// Field names preserve the serde wire contract exactly: camelCase from
/// `rename_all`, except `Health_identity`'s queue variant fields, which stay
/// snake_case on the wire.

/// What a runtime provider id refers to in observation storage. A direct
/// runtime provider is `proxy`; failover-queue members carry the exact
/// `(app_type, source_provider_id)` provenance emitted by the queue builder,
/// never a guessed `queue-*` prefix strip.
export type Health_identity =
  | { kind: 'proxy'; id: string }
  | { kind: 'queue'; app_type: string; source_provider_id: string };

/// Bounded facts about the most recent terminal failure. No raw errors,
/// response content, URLs, or credentials.
export interface Provider_last_failure {
  /** Unix epoch milliseconds of the failure. */
  wallMs: number;
  /** Owner sequence that ordered the terminal outcome. */
  sequence: number;
  /** One fixed safe reason code. */
  reason: string;
}

/// Derived terminal-attempt failure view for one provider. `latestOutcome`
/// uses the confirmed vocabulary: `ok` for a terminal inference success,
/// `failed` for any terminal non-success. `null` means unobserved (never a
/// measured zero).
export interface Provider_failure_health {
  observed: boolean;
  latestOutcome: 'ok' | 'failed' | null;
  failureCount: number;
  inCooldown: boolean;
  lastFailure: Provider_last_failure | null;
}

/// One configured failover-queue candidate in the read-only health
/// projection: the runtime id, its explicit source association, whether a
/// live provider instance backs it in this snapshot, and the derived failure
/// view (`null`, not zeroed, while unresolved).
export interface Queue_candidate_health {
  runtimeId: string;
  source: Health_identity;
  resolved: boolean;
  failure: Provider_failure_health | null;
}

/// Disjoint candidate outcome counts over the configured queue members.
/// `ok` + `failed` + `unobserved` partition the resolved candidates;
/// `unresolved` counts configured ids with no live provider instance.
export interface Queue_health_totals {
  total: number;
  ok: number;
  failed: number;
  unobserved: number;
  unresolved: number;
}

/// Read-only projection of the configured legacy queue and per-surface
/// failover queues, including configured ids the runtime cannot resolve.
export interface Queue_health_projection {
  candidates: Queue_candidate_health[];
  totals: Queue_health_totals;
}
