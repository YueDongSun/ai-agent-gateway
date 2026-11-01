# Copilot Instructions for AAG

Read `AGENTS.md` first. Use `docs/architecture.md` for current module
ownership, `docs/development.md` for commands, and `PROGRESS.md` for the
current phase and verification boundary.

## Runtime map

- `src/`: independently runnable Node gateway, CLI/TUI, JSON config, and legacy
  agent adapters.
- `src-tauri/src/`: Rust/Tauri backend, SQLite DAOs, IPC commands, services, and
  Axum proxy.
- `ui/src/`: React desktop UI and synthetic browser demo.
- `scripts/ui_smoke/`: owned browser smoke and regression checks.

Keep these runtimes independently runnable. Put SQL in DAOs, orchestration in
Tauri command modules, agent file formats in provider services, and UI state in
its owning page/context or hook.

## Rules

- Use `snake_case` for new project-controlled names.
- Preserve public IPC commands, npm scripts, protocol fields, and generated
  filenames.
- Validate boundaries, preserve errors, and never silently drop invalid data.
- Never log or commit credentials, databases, or live agent configuration.
- Use atomic writes and backup-before-overwrite for live agent files.
- Do not use browser-demo output as proof of native or real-provider behavior.
- Do not use stale React callbacks as a rendering optimization.

## Verification

```text
npm run typecheck
npm test
npm run test:desktop_runner
npm run ui:build
npm run ui:build_demo
npm run ui:smoke
npm run ui:smoke_demo
npm run release:verify
cd src-tauri
cargo check --locked --offline
cargo test --locked --offline --lib -- --test-threads=1
```

Run the smallest relevant subset first. Expand to the full gate for persistence,
cross-tree, or release changes. Report local, browser-demo, native, and
external evidence separately.

## Known contracts

- Rust routing hot-applies provider, route, supplier, model, pin, and catalog
  changes to new requests; in-flight requests keep their admitted snapshot.
  Listener host/port and gateway-auth changes still require a restart.
- Streaming failover probes the first chunk only.
- The current release is the unsigned `v0.2.0` preview.
