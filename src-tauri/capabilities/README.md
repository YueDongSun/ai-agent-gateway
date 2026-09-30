# `src-tauri/capabilities/` — Tauri permissions

Tauri 2 capabilities declare which plugin APIs and core APIs each window
can call. Every capability file in this directory is referenced by
`tauri.conf.json` and consumed at build time by the Tauri build hook.

## Anchor files

- [`default.json`](default.json) — the only capability currently shipped.
  Applied to the `main` window. Grants `core:default` (the core
  window / event APIs the desktop app needs).

## Key contracts

- Capabilities are scoped to a list of `windows`. Adding a new window
  requires a matching entry.
- Permissions follow the `core:<set>` and `<plugin>:<permission>` naming
  convention. New permissions belong in `Cargo.toml` (the plugin's
  feature flag) and in the capability JSON.
- Anything the desktop UI invokes through `@tauri-apps/api/core::invoke`
  must be reachable through this capability set; otherwise the call
  rejects with a permission error.

## See also

- [`../tauri.conf.json`](../tauri.conf.json) — the bundle config that consumes
  these capabilities.
- [`../src/`](../src/README.md) — `run()` and the native lifecycle which
  registers the plugins gated by these permissions.
- [Root README](../../README.md).
