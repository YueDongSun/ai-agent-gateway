# `src-tauri/src/` — Tauri app entry, native lifecycle, and tests

The Rust source tree for the desktop application. `lib.rs::run()` is the
Tauri setup that registers plugins, managed state, the IPC `invoke_handler`
list, and the post-setup hooks (tray, autostart, deep links, proxy
auto-start). Everything else in this directory either wires that entry
point to native lifecycle, defines the `AppError` type, or runs the
full-stack end-to-end test.

## Anchor files

- [`lib.rs`](lib.rs) — `run()` function. Registers `tauri-plugin-log`,
  `tauri-plugin-single-instance`, `tauri-plugin-deep-link`,
  `tauri-plugin-updater`, and (on desktop targets)
  `tauri-plugin-autostart`. Manages `Database` and the running proxy
  handle. Installs the tray, enables autostart, registers the deep-link
  listener, and arms the proxy when `proxy_config.enabled = true`. Defines
  the top-level Tauri commands `app_info` and `app_check_update`.
- [`main.rs`](main.rs) — binary entry point. Forwards to
  `ai_agent_gateway_lib::run()`.
- [`startup.rs`](startup.rs) — autostart argument detection, login-launch
  detection, and autostart enable / disable. The private argument
  `--aag-autostart` is the only signal a tray-only launch uses.
- [`diagnostics.rs`](diagnostics.rs) — bounded log configuration, temporary
  debug deadlines, panic/startup evidence, request ids, and safe summaries.
- [`tray.rs`](tray.rs) — system tray. Fixed Show / Hide / Quit menu,
  left-click focuses the main window, close-to-tray intercept for the
  `main` window.
- [`deeplink.rs`](deeplink.rs) — forwards every `aag://` URL the OS hands
  us to the frontend through the `"aag://deeplink"` Tauri event.
- [`deeplink_parser.rs`](deeplink_parser.rs) — parses
  `aag://switch/<id>`, `aag://provider/<id>`, `aag://mcp/<id>`,
  `aag://skill/<id>`, `aag://deeplink` into typed `DeeplinkAction`
  variants.
- [`config.rs`](config.rs) — application config directory resolution
  (`AAG_CONFIG_DIR`, default `~/.aag`), atomic file I/O, backup helpers,
  and per-app live-config path resolution.
- [`error.rs`](error.rs) — `AppError` enum (`Config`, `InvalidInput`, `Io`,
  `Json`, `Database`, `Lock`, `ConflictWithCcSwitch`, `Message`).
  Implements `From<AppError> for String` so Tauri commands can return
  `Result<T, String>`.
- [`provider.rs`](provider.rs) — `Provider` data model plus
  `ProviderMeta` / `CustomEndpoint`. Mirrors cc-switch's provider shape
  trimmed to the fields AAG needs.
- [`e2e.rs`](e2e.rs) — full-stack end-to-end test. Isolates
  `AAG_CONFIG_DIR` / `USERPROFILE`, opens the database, writes a live
  agent config, builds the router, serves on a real TCP port, sends real
  HTTP requests through `reqwest`, and shuts down gracefully.
- [`commands/`](commands/README.md) — Tauri IPC orchestration grouped by
  responsibility.
- [`database/`](database/README.md) — SQLite, schema, migrations, seed,
  and DAOs.
- [`proxy/`](proxy/README.md) — Axum HTTP server, canonical types,
  conversion, routing, providers, circuit breakers.
- [`services/`](services/README.md) — agent adapters, cc-switch import and
  detection, and S3 / WebDAV sync.

## Key contracts

- Public modules of the library: `config`, `database`, `deeplink`,
  `deeplink_parser`, `diagnostics`, `error`, `provider`, `proxy`, `services`,
  `startup`, `tray`. These are exposed so helper binaries (`examples/aag-setup.rs`)
  can drive the database without going through Tauri commands.
- Tauri commands return `Result<T, String>`; the `From<AppError> for String`
  impl does the conversion.
- The tray icon is a required startup dependency: `tray::install` errors
  abort startup, so a hidden window can never be left without a recovery
  entry point.

## See also

- [docs/architecture.md](../../docs/architecture.md) — desktop module
  ownership.
- [Root README](../../README.md).
