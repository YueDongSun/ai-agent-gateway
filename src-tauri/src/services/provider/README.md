# `src-tauri/src/services/provider/` — Per-app live configuration adapters

Every supported AI coding tool stores its configuration in a different file
shape. This subtree owns the per-app adapter that reads the live file,
translates AAG's `Provider` into that shape, and writes it atomically with
a backup of the previous content. The split mirrors cc-switch's
`services/provider/live.rs::write_live_snapshot`.

`config_paths.rs` resolves the current client configuration before any write.
`client_config.rs` exposes the same read-only result to the UI. Provider
selection and client launch reuse that resolver, so an environment-selected
file is the one actually written and loaded. The environment variables follow
each client's documented file or directory contract and must be absolute. When
`AAG_HOME_DIR` isolates a portable run or test, external overrides outside that
home are ignored. Detection checks documented overrides, then existing
conventional files, then the platform default.

`config_writes.rs` checks every path component before reading files, skips
unchanged content, preserves a private backup before replacement, writes
atomically, and restores earlier files if a later file in the same client
update fails.

## Anchor files

- [`mod.rs`](mod.rs) — `AppType` enum, `from_db_str` / `to_db_str` /
  `display_name`. `write_live_snapshot(app_type, provider)` dispatches to
  the matching adapter and returns a `LiveWriteResult`.
- [`claude_mirror.rs`](claude_mirror.rs) — Claude Code and Claude Desktop
  mirror adapter. The native `~/.claude` stays untouched; provider selection
  writes the proxy contract into the isolated `~/.claude-cli` directory (or
  the user-selected mirror), and the Claude CLI launcher sets
  `CLAUDE_CONFIG_DIR` to that same mirror on Windows/macOS/Linux. cc-switch can
  continue owning the native Claude Code settings file.
- [`codex.rs`](codex.rs) — Codex adapter. Config lives at
  `~/.codex/config.toml` (TOML) + `~/.codex/auth.json` (JSON). Provider
  switching and CLI launching write only to the separate mirror
  (`~/.codex-cli` by default, or the `codex_cli_home` setting); native
  `~/.codex` is never replaced.
- [`copilot.rs`](copilot.rs) — Copilot CLI adapter. Persists `model` and
  an optional `env` block into `$COPILOT_HOME/settings.json` (default
  `~/.copilot`) (JSONC), merging
  with any existing user settings (never clobbers unrelated keys). The
  actual `COPILOT_PROVIDER_*` env vars are injected only by the launcher
  command in [`../commands/launcher.rs`](../../commands/launcher.rs).
- [`opencode.rs`](opencode.rs) — OpenCode adapter. Merges an AAG provider entry
  into `$OPENCODE_CONFIG`, `$OPENCODE_CONFIG_DIR`, or the global
  XDG/default `opencode.json` or JSONC file used by
  OpenCode and compatible Kilo clients, preserving unrelated providers and
  top-level settings such as `small_model`.
- [`kilo.rs`](kilo.rs) — Kilo CLI adapter. Mirrors the Node-side
  `KiloAdapter` at `src/agents/adapters.ts:48-60, 250-297`. Resolves the
  write target from `$KILO_CONFIG`, `$KILO_CONFIG_DIR`, XDG/default candidates,
  or `~/.kilo/config.json`; creates `kilo.jsonc` in the selected configuration directory if none
  exist. Strips JSONC comments, validates root + `provider` shape, merges
  per-provider-id, sets top-level `model`. Atomic write + timestamped
  backup.
- [`pi.rs`](pi.rs) — Pi adapter. Stores custom model providers in
  `$PI_CODING_AGENT_DIR/models.json` and the default model in
  `$PI_CODING_AGENT_DIR/settings.json` (default `~/.pi/agent`). The `settings_config` JSON blob carries
  `baseUrl`, `apiKey`, `api` (openai-completions | anthropic-messages),
  and a list of model entries.
- [`cline.rs`](cline.rs) — Cline CLI adapter. Writes the documented provider
  settings envelope and model registry only under AAG's `~/.cline_cli/data`.
  The launcher selects that private store through `CLINE_DATA_DIR` and
  `CLINE_PROVIDER_SETTINGS_PATH`, passes provider/model as literal arguments,
  and requires tool approval. Native Cline state remains separate. Provider
  activation uses the existing private, atomic, backup-preserving batch writer.
- [`antigravity.rs`](antigravity.rs) — Antigravity desktop adapter.
  **Detect-only.** The on-disk schema for custom providers is not
  publicly documented and much of the state lives in Chromium LevelDB,
  so `write_live` returns an error guiding the user to configure
  Antigravity in-app.
- [`vscode_extension.rs`](vscode_extension.rs) — VS Code extension
  adapter for `anthropic.claude-code` (`claude_vscode`) and
  `kilocode.kilo-code` (`kilo_vscode`). AAG is the unified upstream
  ("auto-forward"): provider switching happens inside AAG's
  route/failover system while the extension keeps pointing at the proxy.
  - `kilo_vscode` delegates to [`kilo.rs`](kilo.rs) — the Kilo Code
    extension's `globalFiles()` reads the same OpenCode-compatible JSONC
    projection (`~/.config/kilo/{kilo,opencode}.json(c)` plus legacy
    `~/.kilo`, `~/.kilocode`, `~/.opencode` dirs), verified in the
    installed `kilocode.kilo-code-7.5.x` bundle.
  - `claude_vscode` performs no live write (same contract as
    `claude_code.rs`: cc-switch owns `~/.claude/settings.json`); the
    extension's bundled CLI reads `ANTHROPIC_BASE_URL`, injected by the
    `vscode_open_with_extension` launcher (`../../commands/launcher.rs`).
  - VS Code's own `User/settings.json` is never written — its extension
    key namespaces are not source-verifiable (cc-switch removed its VS
    Code settings.json write path in 3.4.x).

## Key contracts

- Every write is atomic: write to a temp file in the same directory,
  then `rename` into place.
- A backup of the previous file is preserved at the path returned by
  `get_backups_dir()` before the rename.
- `LiveWriteResult.app_type` / `provider_id` / `written[]` /
  `backup` are surfaced to the UI so a toast can confirm which files
  were touched and where the backup is.
- Writes are gated by the cc-switch coexistence detector (unless
  `ccswitch_override_enabled = "1"` or `ccswitch_block_enabled = "0"`).

## See also

- [`../commands/provider.rs`](../../commands/provider.rs) — IPC that
  invokes these adapters.
- [`../detect/`](../detect/README.md) — coexistence checks.
- [Root README](../../README.md).
