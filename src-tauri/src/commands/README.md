# `src-tauri/src/commands/` — Tauri IPC orchestration

Every `#[tauri::command]` function lives here. The `mod.rs` re-exports five
submodules; each command body is `pub(crate)`, registered through
`tauri::generate_handler!` in [`src/lib.rs`](../README.md). Commands are
grouped by responsibility (provider / proxy / launcher / settings) so a
reader can locate the IPC surface for a feature without grepping `lib.rs`.

## Anchor files

- [`mod.rs`](mod.rs) — module declarations: `diagnostics`, `launcher`,
  `provider`, `proxy`, `settings`. Pure plumbing.
- [`diagnostics.rs`](diagnostics.rs) — diagnostics state, temporary Debug
  enable/disable, log-directory opening, safe summary, and frontend error IPC.
- [`provider.rs`](provider.rs) — `provider_list`, `provider_current`,
  `provider_get`, `provider_save`, `provider_delete`, `provider_switch`,
  `provider_reorder`, `provider_set_failover_queue`. Switching writes
  per-app live config (`codex` → isolated `CODEX_HOME`, `copilot` → merged
  `~/.copilot/settings.json`, `pi` → upserted models / settings). Claude
  Code does not receive a live write because cc-switch owns
  `~/.claude/settings.json`.
- [`proxy.rs`](proxy.rs) — `proxy_status`, `proxy_start`, `proxy_stop`,
  `proxy_config_get`, `proxy_config_save`, `proxy_get_pinned_provider`,
  `proxy_set_pinned_provider`, `proxy_test_provider`, `proxy_fetch_models`,
  `proxy_provider_list`, `proxy_provider_save`, `proxy_provider_delete`,
  `route_list`, `route_save`, `route_delete`, `route_rename`,
  `import_cc_switch_providers`. Owns `RunningProxy` (the managed state for
  the live Axum task).
- [`launcher.rs`](launcher.rs) — Codex CLI / Codex Desktop / Claude Desktop / Copilot CLI
  and editor launcher IPC adapters. Blocking work runs on Tauri's worker pool
  through [`services/launcher/`](../services/launcher/). `codex_cli_status`, `codex_open_cli_terminal`,
  `codex_open_desktop_window`, `copilot_open_cli_terminal`, mirror status/mode
  and session operations, and editor status/launch commands. Each returns a
  result DTO that the UI can render directly. Registered commands must also
  appear in `build.rs` and the main window's `capabilities/default.json`;
  `test:dsh_transport` checks this contract without granting commands to market
  or runtime web content.
- [`settings.rs`](settings.rs) — `settings_get` / `settings_set`. Thin
  wrappers around the `app_meta` key/value store for device-level
  preferences (Codex CLI isolation directory, outbound proxy,
  cc-switch coexistence toggles, proxy pinned provider).

## Key contracts

- Every command returns `Result<T, String>` and maps `AppError` to
  `String` via the `From` impl.
- `proxy_start` snapshots providers and routes from the database; the
  Rust proxy uses that snapshot until the next start. Edits to providers
  or routes require a restart.
- `proxy_set_pinned_provider` writes through to `app_meta` so the pin
  survives restarts, then updates the live `RunningProxy` state if the
  proxy is already up.
- `import_cc_switch_providers` is idempotent: rows already in AAG are
  skipped, not overwritten.

## See also

- [`src-tauri/src/database/`](../database/README.md) — DAOs these commands
  call.
- [`src-tauri/src/services/`](../services/README.md) — business logic the
  provider commands invoke.
- [Root README](../../README.md).
