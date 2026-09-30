# Cross-platform client configuration discovery — 2026-09-23

AAG now resolves the same client configuration path for status display, provider
replacement and launch. Provider pages show the host OS, path source and target
files. Selecting or reapplying a provider writes those files before updating the
current-provider marker. Launching Claude Code, Codex CLI, Copilot CLI, or a VS
Code extension refreshes the same selected configuration before starting it.

The resolver follows client contracts on Windows, macOS and Linux:

- `COPILOT_HOME` → `settings.json`; otherwise `~/.copilot/settings.json`.
- `PI_CODING_AGENT_DIR` → `models.json` + `settings.json`; otherwise
  `~/.pi/agent/`.
- `GEMINI_CLI_HOME` → `.gemini/.env` + `settings.json`; otherwise
  `~/.gemini/`.
- `OPENCODE_CONFIG` or `OPENCODE_CONFIG_DIR`, then existing XDG/global files.
- `KILO_CONFIG` or `KILO_CONFIG_DIR`, then existing XDG/global/legacy files.
- `CODEX_CLI_HOME` and `CLAUDE_CLI_HOME` settings point at AAG-isolated mirrors.

All overrides are absolute and checked before reads or writes. Symlink/reparse
components are refused. `AAG_HOME_DIR` isolates a portable/test run and ignores
client overrides outside it. AAG preserves the native Claude and Codex homes.
Multi-file updates validate every target and contents first, save private
backups, write atomically, and roll back earlier files if a later write fails.
Repeated identical settings are left untouched and do not create redundant
backups. Invalid user JSON is retained and blocks the switch instead of being
silently replaced.

## Validation

Local host: macOS arm64. Cross-system path behavior uses injected platform and
environment fixtures. Only macOS native process launch was executed on a native
host; Windows and Linux use their own OS-specific filesystem, editor and
terminal launch behavior and still need native-host acceptance.

| Command | Result | Evidence |
| --- | --- | --- |
| `npm run verify:features -- --scope all --run` | Passed: 15 environment checks and all 15 stages; runtime state removed | Complete unattended gate with canonical private config/home/temp directories |
| `npm run typecheck` | Passed | Node surface |
| `npm test` | 77 passed | Local fixtures |
| `npm run test:automation` | 37 passed, 2 skipped | Verification, cleanup, MCP fixtures |
| `npm run test:desktop_runner` | 16 passed | Prerequisite discovery and desktop build config |
| `npm run test:dsh_transport` | 2 passed | Native command permission/authority boundary |
| `npm run test:dsh_market_catalog` | 17 passed | Catalog fixtures |
| `npm run ui:build` + `npm run ui:build_demo` | Passed | UI typecheck and both bundles |
| `npm run ui:smoke` + `npm run ui:smoke_demo` | 121/121 each | Synthetic IPC in source and built-demo modes |
| `cargo check --locked --offline` | Passed | macOS target |
| `cargo test --locked --offline --lib -- --test-threads=1` | 474 passed, 6 ignored | Isolated config, home and canonical temp directories; local fixtures and loopback tests |
| `npm run release:verify` / `npm run privacy:audit` | Passed, zero blockers/warnings | Local preflight and tree scan; no release |
| `git diff --check` | Passed | Patch integrity |

The complete gate uses synthetic IPC for browser smoke; the Rust fixtures use
real local processes and loopback ports but no real client configuration or
upstream provider.

Desktop mirror launchers for Claude and Codex remain Windows-only. Claude Code
and Codex CLI mirrors, provider switching, config-path discovery, Copilot, Pi,
Gemini, OpenCode, Kilo and VS Code extension paths are integrated across the
three desktop OS path contracts. Antigravity, OpenClaw, Hermes and Grok Build
remain unsupported for live provider configuration because their managed file
formats are not implemented. Project-specific configs and enterprise policy
may override a client's global user config according to that client’s own
precedence rules.
