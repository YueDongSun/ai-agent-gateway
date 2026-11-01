# `src-tauri/examples/` — Helper binaries

Standalone Rust binaries that link the `ai_agent_gateway_lib` library and
perform a single setup action. They live under `examples/` rather than
`src/bin/` so the explicit `[[bin]]` for the Tauri app is unambiguous.

## Anchor files

- [`provider_screening.rs`](provider_screening.rs) — explicit real-provider
  screening followed by an admission-filtered CC Switch import. Run with
  `cargo run --locked --offline --example provider_screening -- --help`.
  `preview [--database PATH] [--source PATH]` performs local reads only and lists
  the bound plan, source references, provider names and configured models.
  Omitted paths use AAG's normal configuration and CC Switch path resolvers.
  A preview lists candidates; it does not establish subscription validity.

  `apply --plan PLAN_ID --backup NEW_ABSOLUTE_PATH [--database PATH] [--source PATH]`
  requires the current preview and creates a complete private snapshot before
  any screening/cache/import writes. It queries only supported configured status
  sources, sends `Reply with exactly: ok` only after source admission, measures latency,
  and imports admitted connections. `--subscription-free-source SOURCE_REF` is
  repeatable and explicitly designates exact CC Switch source identities for
  subscription-free loopback providers; the owning service validates the binding.
  No provider-name inference or arbitrary stored script execution is used.

  `recheck --provider ID [--provider ID]... --backup NEW_ABSOLUTE_PATH [--database PATH]`
  rechecks only the selected registered providers whose previous screening result
  was `abnormal_response`. It validates the full selection, deduplicates IDs and
  creates one new private snapshot before sequential owner rechecks. This mode
  rejects source, plan and subscription-free flags; it uses each provider's
  existing bound source policy. Receipts include reasons, latency and admitted
  models, without captured response bodies. Runtime reload remains separate.

  Output contains eligibility, stages, reasons, timings and import counts.
  Credentials, endpoint URLs, scripts and actual probe responses stay out of
  terminal receipts. A screening failure can leave protected admission records
  changed while the import remains unapplied; the receipt distinguishes these
  states and retains the backup. Runtime reload and client configuration are
  separate owner actions. No recurring screening job is created.

- [`supplier_import.rs`](supplier_import.rs) — explicit operator preview/apply
  for the current supplier importer. Run with
  `cargo run --locked --offline --example supplier_import -- --help`.
  `preview DATABASE SOURCE` opens an existing schema-compatible AAG database
  read-only, performs no initialization/migration, and emits aggregate counts,
  a bound plan ID and an exact `contentCurrent` comparison. It prints no
  credentials, provider names or endpoint URLs.
  `apply DATABASE SOURCE PLAN_ID BACKUP` requires the current plan ID and a new
  absolute backup path. It creates a full private SQLite snapshot before calling
  the importer, refuses plans targeting non-CC-Switch suppliers, and checks route
  preservation. The backup is retained on failure. This helper does not delete
  stale rows, run broad maintenance, write client configuration or reload another
  running process. A changed import reports `runtimeReloadRequired` explicitly.
  Same-connection metadata refreshes preserve discovered/manual models and local
  admission settings. Existing connection URL, credential or header changes are
  refused with `supplier_import_endpoint_change_requires_review`; they need
  dedicated revalidation rather than inheriting old catalog availability.

  For an explicitly authorized full replacement, `reset-preview DATABASE SOURCE`
  derives fresh static supply against an empty inventory and binds a distinct
  reset plan to the source and current provider state. `reset-apply DATABASE SOURCE
  PLAN_ID BACKUP` uses the existing full private backup boundary, then clears and
  rebuilds providers, suppliers and dependent routes in one transaction. An empty
  source, stale plan, concurrent write or insertion failure leaves the old supply
  intact. Unrelated settings, catalogs, sessions, usage, MCP/skills and listener
  authentication are retained. Stop the running AAG instance before applying;
  relaunch it afterward so its normal startup reconciles managed clients and
  reloads the runtime. Never use ordinary import plan IDs for reset mode.

- [`aag-setup.rs`](aag-setup.rs) — one-shot setup helper. Run with
  `cargo run --bin aag-setup`. Opens (creates if missing) the AAG
  database, applies the schema seed, imports every provider from
  `~/.cc-switch/cc-switch.db` into AAG, counts the imported
  `claude_code` rows for sanity-check output, flips
  `proxy_config.enabled = true` so the next Tauri launch arms the
  proxy, and explicitly clears `sync_with_cc_switch_enabled` so the
  auto-sync path stays off.

  Does **not** start the proxy — the Tauri app owns start / stop so the
  UI can surface the lifecycle.

## Key contracts

- Helper binaries must drive the database through the public `lib.rs`
  modules. They must not embed SQL.
- Helper binaries must not register Tauri plugins or invoke Tauri
  commands.

## See also

- [`../src/`](../src/README.md) — the public
  modules a helper can call into.
- [Root README](../../README.md).
