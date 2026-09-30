# `src-tauri/` — Tauri 2 desktop shell

The Rust/Tauri backend. Owns the SQLite store at `~/.aag/aag.db`, every
Tauri IPC command, the Axum proxy, the per-agent live config writers, and
native lifecycle (tray, autostart, deep links, single instance, updater).
The React UI in [`ui/`](../ui/README.md) runs inside this shell when
launched through `npm run desktop:dev` or `npm run desktop:build`.

## Anchor files

- [`Cargo.toml`](Cargo.toml) — crate `ai-agent-gateway` version `0.2.0`.
  Declares the explicit `[[bin]]` for the Tauri app so `cargo run` does not
  pick up the `aag-setup` helper, and pulls in the `axum`, `rusqlite`,
  `reqwest`, `tauri-plugin-*`, and sync / hash dependencies.
- [`tauri.conf.json`](tauri.conf.json) — product metadata, window geometry,
  bundle icon list, `frontendDist = ../ui/dist`, and `deep-link` /
  `updater` plugin config.
- `tauri.dev.json` — generated development-mode overlay consumed by
  `scripts/tauri-dev.mjs`.
- [`capabilities/default.json`](capabilities/README.md) — Tauri capability
  granted to the main window.
- [`build.rs`](build.rs) — `tauri_build::build()` for the Tauri build hook.
- [`generate_icons.py`](generate_icons.py) — one-shot icon regenerator that
  mirrors `scripts/generate_icons.mjs` in Python.
- [`icons/`](icons) — RGBA PNG / ICO / ICNS assets required by the Tauri
  bundler. The tray icon is embedded into the binary via
  `tauri::include_image!` in [`src/tray.rs`](src/tray.rs).
- [`examples/aag-setup.rs`](examples/README.md) — one-shot setup helper that
  imports providers from cc-switch and primes the proxy config.
- [`src/`](src/README.md) — Rust source tree.

## Key contracts

- The `[[bin]] ai-agent-gateway` binary is the desktop app. It runs from
  `src/main.rs` and calls `ai_agent_gateway_lib::run()` from `src/lib.rs`.
- Helper binaries (`aag-setup`) live under `examples/` so the explicit bin
  declaration is unambiguous.
- The `ui/dist` directory must exist before `cargo check` or `cargo build`,
  because `tauri.conf.json` reads it as `frontendDist`.

## See also

- [docs/architecture.md](../docs/architecture.md) — desktop module ownership.
- [docs/desktop_release.md](../docs/desktop_release.md) — release pipeline.
- [Root README](../README.md).
