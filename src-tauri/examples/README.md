# `src-tauri/examples/` — Helper binaries

Standalone Rust binaries that link the `ai_agent_gateway_lib` library and
perform a single setup action. They live under `examples/` rather than
`src/bin/` so the explicit `[[bin]]` for the Tauri app is unambiguous.

## Anchor files

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
