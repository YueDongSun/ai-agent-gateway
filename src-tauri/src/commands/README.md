# `src-tauri/src/commands/` — Tauri IPC orchestration

Feature IPC orchestration lives here, grouped by responsibility in `mod.rs`.
The application entry point registers command handlers through
`tauri::generate_handler!` in [`lib.rs`](../lib.rs). The shell-level
`app_info` and `app_check_update` commands are defined in that entry point,
not in this directory. Module declarations and helper visibility do not by
themselves register an IPC command.

## Module map

[`mod.rs`](mod.rs) declares the modules below. Each contains Tauri command
functions; ordinary helpers in the same files are not separate IPC surfaces.

- [`client_config.rs`](client_config.rs) — configuration-path status and the
  scoped Kilo JSONC editor: `provider_config_status`, `client_config_read`,
  `client_config_save`.
- [`diagnostics.rs`](diagnostics.rs) — diagnostics state, temporary Debug
  enable/disable, log-directory opening, safe summaries, frontend error IPC,
  and the metadata-only request timeline.
- [`dsh.rs`](dsh.rs) — DSH snapshots, settings, operation preview/execution,
  cancellation, upstream listing, runtime start/stop, and runtime-window IPC.
- [`dsh_market.rs`](dsh_market.rs) — marketplace catalog reads, refresh,
  source opening, and the restricted marketplace window:
  `dsh_market_get_catalog`, `dsh_market_refresh`, `dsh_market_open_source`,
  `dsh_open_marketplace`.
- [`dsh_onboarding.rs`](dsh_onboarding.rs) — runtime discovery and setup
  preview/apply: `dsh_discover_runtime`, `dsh_preview_setup`, `dsh_apply_setup`.
- [`launcher.rs`](launcher.rs) — CLI, desktop, and editor launcher adapters,
  managed-client status, and mirror/session operations. Blocking launcher
  work runs on Tauri's worker pool through
  [`services/launcher/`](../services/launcher/). Representative commands include
  `codex_cli_status`, `codex_open_cli_terminal`, `claude_open_desktop`, and
  `vscode_extension_status`.
- [`model_discovery.rs`](model_discovery.rs) — model-discovery status,
  manual refresh, and per-provider automatic tracking:
  `model_discovery_status`, `proxy_refresh_models`, `proxy_set_auto_track`.
- [`mcp.rs`](mcp.rs) — manager-only MCP registry CRUD, enablement, and Claude
  synchronization review: `mcp_list_servers`, `mcp_preview_sync`,
  `mcp_apply_sync`, `mcp_discard_review`.
- [`prompt.rs`](prompt.rs) — prompt-enhancement orchestration over the local
  model pool: `prompt_enhance_run`, `prompt_enhance_defaults`.
- [`provider.rs`](provider.rs) — `provider_list`, `provider_current`,
  `provider_get`, `provider_save`, `provider_delete`, `provider_switch`,
  `provider_reorder`, `provider_set_failover_queue`. Switching delegates
  per-client file or isolated-mirror writes to
  [`services/provider/`](../services/provider/) and updates the current
  provider selection. Claude and Codex use managed mirrors rather than
  overwriting their native client homes.
- [`proxy.rs`](proxy.rs) — `proxy_status`, `proxy_start`, `proxy_stop`,
  `proxy_config_get`, `proxy_config_save`, `proxy_get_pinned_provider`,
  `proxy_set_pinned_provider`, `proxy_get_responses_cot`,
  `proxy_set_responses_cot`, `proxy_test_provider`, `proxy_fetch_models`,
  `proxy_provider_list`, `proxy_provider_save`, `proxy_provider_delete`,
  `route_list`, `route_save`, `route_delete`, `route_rename`,
  `import_cc_switch_providers`. Owns `RunningProxy`, the managed state for
  the live Axum task.
- [`settings.rs`](settings.rs) — `settings_get` / `settings_set`, wrappers
  around the `app_meta` key/value store for device-level preferences such as
  managed-client paths, outbound proxy, and cc-switch coexistence settings.
- [`workflow_registry.rs`](workflow_registry.rs) — capability inventory,
  health, route preview, bounded probes, and route evaluation:
  `workflow_list_capabilities`, `workflow_get_health`,
  `workflow_route_preview`, `workflow_probe_capability`,
  `workflow_evaluate_route`.

## Key contracts

- Fallible IPC commands expose `Result<T, String>`. Conversion from
  `AppError` emits stable external error messages rather than internal paths
  or payloads.
- Command registration must stay aligned with [`build.rs`](../../build.rs)
  and the main window's [`default.json`](../../capabilities/default.json).
  `npm run test:dsh_transport` checks this boundary without granting manager
  commands to marketplace or runtime web content.
- `proxy_start` starts the Axum proxy; provider, route, supplier, model, pin,
  and catalog changes hot-apply to new requests without restarting the listener,
  while in-flight requests keep their admitted snapshot. Listener host/port and
  gateway-auth changes still require a restart.
- `proxy_set_pinned_provider` and `proxy_set_responses_cot` persist their
  values in `app_meta` and update the running proxy without a restart.
- `provider_reorder` requires unique supplied IDs belonging to the target
  app and applies their indexes atomically. Partial lists are allowed;
  omitted rows keep their existing indexes and can interleave under normal
  query ordering.
- `import_cc_switch_providers` is idempotent: rows already in AAG are
  skipped, not overwritten.

## See also

- [`src-tauri/src/database/`](../database/README.md) — DAOs these commands call.
- [`src-tauri/src/services/`](../services/README.md) — business logic behind IPC.
- [Root README](../../../README.md).
