# Native launcher and platform contracts

`src-tauri/src/commands/launcher.rs` schedules blocking work on Tauri's worker
pool. `src-tauri/src/services/launcher/` owns executable discovery, terminal
adapters, editor integration, child lifetime and the existing Codex mirror
operations. Public IPC command names and frontend camelCase status fields are
preserved. Launch receipts retain environment variable names but redact values.

## Platform behavior

| Surface | Windows | macOS | Linux |
| --- | --- | --- | --- |
| CLI discovery | PATH, npm/pnpm and known user/system installs; exe and cmd support | PATH, Homebrew, nvm/fnm/asdf/mise and known user bins | PATH, Linuxbrew, nvm/fnm/asdf/mise and known user bins |
| Codex/Copilot terminal | PowerShell in a new console, values inherited via the environment | Launch Services opens a private executable `.command` in Terminal | Terminal-specific arguments for x-terminal-emulator, GNOME, Konsole, Xfce and xterm |
| Editor discovery | Stable, Insiders and Cursor executables | CLI inside system/user application bundles as well as PATH | PATH and standard/system/Snap locations |
| Codex Desktop mirror | Supported existing integration | Explicit unsupported error | Explicit unsupported error |
| Claude Desktop mirror | Supported existing integration | Explicit unsupported error | Explicit unsupported error |

Discovery is passive: it does not execute shell startup files, `where`,
`command -v`, or discovered CLI programs. Directories and non-executable Unix
files do not qualify. Search is bounded and ignores relative PATH entries.
VS Code extension status uses the directory belonging to the selected editor,
so Insiders and Cursor are not accidentally checked against stable VS Code.

## Client configuration discovery and activation

Every provider page shows the detected operating system, configuration mode and
source, plus target paths for file-backed integrations. Provider selection
applies the selected provider to those files and updates the current-provider
marker only after the write succeeds. Selecting the current provider again
reapplies its configuration. Starting Claude Code, Copilot CLI, the Gemini CLI,
or a VS Code extension refreshes its current configuration first and then
launches it. Codex CLI prepares its isolated home and passes `CODEX_HOME`, but
launching it does not reapply provider configuration; select or reapply the
provider to refresh the mirror file. Pi, OpenCode, and Kilo use their discovered
global files when the user activates their provider; the Gemini CLI launcher
does not inject credentials through the environment — the CLI reads the applied
`.gemini/.env` at the discovered home itself. Backups are private under AAG's
backup directory.

Kilo CLI and Cline CLI pages expose passive executable availability and an
explicit terminal launcher. Missing executables show the official installation
link; status checks never execute the CLI or write client configuration. Opening
Kilo reapplies its selected provider to the discovered file and injects
`KILO_CONFIG` pointing to that same file. Opening Cline reapplies the selected
provider to AAG's isolated `~/.cline_cli/data/settings/providers.json` and
adjacent `models.json`, sets `CLINE_DATA_DIR` and `CLINE_PROVIDER_SETTINGS_PATH`,
then launches Cline with `-i --auto-approve false --config ~/.cline_cli`
and `--provider aag --model <model>`.
Native `~/.cline` configuration and credentials are not read or replaced.
Cline uses its source-verified `openai-chat` / `openai-compatible` contract;
the adapter validates endpoint, key and model before any write, preserves
other providers, and keeps the previous database selection on write failure.
Credentials remain in private configuration files and never enter arguments
or launch receipts. CLI arguments use literal PowerShell array splatting on
Windows and individually quoted arguments in Unix terminal scripts.

Configuration detection checks documented client environment overrides, then
existing conventional files in their documented order, then the platform's
default. Supported overrides are `COPILOT_HOME`, `PI_CODING_AGENT_DIR`,
`GEMINI_CLI_HOME`, `OPENCODE_CONFIG` / `OPENCODE_CONFIG_DIR`,
`KILO_CONFIG` / `KILO_CONFIG_DIR`, and XDG configuration directories. File and
directory overrides must be absolute. A portable `AAG_HOME_DIR` ignores
inherited client overrides outside its isolated home.

Codex CLI and Claude Code use separate mirror homes; AAG never redirects writes
into native `~/.codex` or `~/.claude`. Configuration and discovery run on
Windows, macOS and Linux. Desktop mirror launching remains available on Windows;
the cross-platform launcher runs the Claude CLI and Codex CLI. Antigravity,
OpenClaw, Hermes and Grok Build remain detect-only or unsupported because their
managed provider-file formats are not implemented.

The existing Antigravity adapter describes the desktop IDE. The separate
official `agy` CLI has a documented Gemini API-key mode and requires a distinct
integration contract.

On macOS, Finder/Dock launches need not inherit a configured shell PATH. Passive
fallback discovery covers both Homebrew prefixes, versioned Node formulae,
nvm, fnm (including its Application Support directory), asdf and mise. Absolute
manager environment overrides are honored. Version directories are searched in
numeric descending order, with bounded enumeration and candidate counts; shell
startup files are never sourced. The selected CLI's own Node installation is
preferred in its child PATH, including npm global symlinks. Custom installations
outside these conventions still need an absolute PATH or manager-root setting;
discovery does not recursively search the entire disk.

## Environment and process lifetime

VS Code inherits the first running instance's environment unless given a
separate user-data directory, as described in its
[official CLI documentation](https://code.visualstudio.com/docs/configure/command-line#_isolating-vs-code-instances).
AAG uses a private `editor_profiles/` directory below its configuration root,
separated by editor and agent, with a hash of the selected executable and injected
environment. Changed proxy/token values in that environment select a separate
profile instead of silently reusing an older environment. When Kilo uses an
existing configuration file, only its `KILO_CONFIG` path enters the environment;
changes to that file's contents alone do not select a new profile. These profiles
persist; user settings and logins from the native editor are not copied. The
selected editor's installed extension directory is supplied separately. Portable
VS Code installations, whose `data` directory overrides CLI profile options,
require separate native acceptance.

Authentication comes from client configuration files or the child environment,
depending on the integration. Kilo's editor launch uses `KILO_CONFIG` for an
existing file and falls back to inline `KILO_CONFIG_CONTENT` when it is absent.
Injected environment values are redacted in launch receipts and are not logged.
Windows terminal launch source is fixed and does not interpolate credential
values. Unix terminal scripts contain shell-quoted environment assignments in a
private random directory, use private atomic writes, and unlink themselves before
starting the agent. Failed dispatch removes the directory. macOS uses
`/usr/bin/open` with Terminal's bundle identifier and an executable document
argument, so dispatch does not require AppleEvents automation authorization.
Unexecuted scripts expire after 60 seconds while AAG is running. Terminal
preferences may retain the window after the child exits.

Working directories must be absolute existing directories. A failed `cd` cannot
continue into the agent. Metadata probes have a five-second timeout, bounded
captured output and child kill/wait on failure. GUI dispatch detects immediate
process errors and reaps long-lived children when they exit. Dispatch success
does not prove that an external application's window or extension is ready.

The Windows Codex desktop mirror preserves its existing profile on ordinary
launch. Its explicit reset command retains the existing guarded reset boundary.
Process-query errors are propagated instead of being reported as zero processes;
the process filter uses the configured mirror profile, including custom paths.
Codex exclusive/coexist and Claude CLI token/passthrough modes retain their
contracts. Claude Desktop uses a separate `desktop_local/Claude-3p` profile with
the local AAG gateway bearer; it does not reuse the native Claude OAuth login.
The process-local `LOCALAPPDATA` and Electron data-root arguments point to this
isolated profile. Desktop gateway writes require a nonempty token and loopback
client endpoint. Exact Store package discovery resolves the real application
executable, and absolute Windows command paths use native separators.
Both desktop mirror launchers remain Windows-only; passive macOS
CLI/editor detection does not imply support for those desktop mirror surfaces.

When `mirror_model_roles_enabled` is enabled, Claude Desktop advertises Fable,
Opus, Sonnet, and Haiku routing roles. Codex advertises `gpt-5.6-sol`,
`gpt-5.6-terra`, and `gpt-5.6-luna` routing roles. These are client selectors:
their labels identify AAG routing, and the admitted queue supplies the actual
upstream model ID. The selected unified model comes first, followed by eligible
configured supplier priority. These role names do not assert the identity or
relative capability of the serving upstream model. Literal offered models and
`precise:` selectors retain their exact-model contract.

HTTP 429, network failures, and server errors can advance the queue. Explicit
structured quota errors also qualify, including MiniMax's documented five-hour
exhaustion code `2056`. Authentication and invalid-input errors remain terminal.
Failover occurs before response output starts; a partially emitted stream or an
opaque Responses continuation cannot be replayed transparently across suppliers.
The circuit breaker does not infer a five-hour reset time or poll account quota.

Windows instruction synchronization is explicitly enabled with
`mirror_instruction_sync_enabled`. One worker reconciles while AAG is running,
with a five-second interval after each pass, and launcher calls share its gate.
Codex mirrors receive `AGENTS.md`, `AGENTS.override.md`, `agent_policy/`,
`rules/`, `skills/`, `agents/`, and `prompts/`. Claude mirrors receive `CLAUDE.md`,
`CLAUDE.local.md`, `skills/`, `agents/`, `commands/`, `rules/`, and `prompts/`.
Known top-level skill links into the same user's shared skill stores are
materialized. Codex's shared user `.agents/skills` location is consumed directly
by the client; it is not overlaid into the mirror skill tree.

Within those allowed roots, original content is authoritative: additions and
edits propagate, independent mirror edits are corrected, and obsolete mirror-only
files are backed up and removed. The worker inventories the complete available
source before admitting deletions, compares bytes, and records ownership before
mutating files so interrupted work can reconcile. Missing or unreadable sources
block deletion. Recovery copies live under AAG's private
`backups/mirror_instructions/` directory, outside instruction discovery.
Authentication, model settings, sessions, native runtime state, and vendor-owned
`skills/.system` remain outside this synchronization contract.

## Configuration directories

VS Code and Antigravity use APPDATA on Windows, Application Support on macOS,
and an absolute XDG_CONFIG_HOME (or the home .config directory) on Linux.
An explicit AAG_HOME_DIR takes priority over inherited APPDATA/XDG_CONFIG_HOME,
so isolated tests and portable runs cannot write to a native application profile.
The AAG SQLite location and provider schemas are unchanged.

## Verification

The automatic `verify:features -- --scope all --run` runner detects prerequisites
and supplies private canonical temporary directories without manual environment
setup. For direct Node/Rust tests, use disposable private `AAG_CONFIG_DIR` and
`AAG_HOME_DIR` and leave `AAG_TEST_CCSWITCH_DB` unset so optional real-database and
real-provider probes do not run. Run Rust tests serially and set `TMPDIR` to its
real path on macOS, for example `/private/tmp`. The credential-storage boundary
deliberately rejects symbolic-link components.

Launcher regressions execute local fixture programs to check literal environment
delivery, argument boundaries, working-directory failure, script deletion, output
limits and timeout reaping. Platform tables cover installation candidates and
terminal arguments. These fixtures do not launch a real coding agent or submit
provider requests. Windows console/Store behavior and Linux desktop terminals
still require execution on their own native hosts. Browser smoke detects the
host's installed Chrome/Edge/Chromium and remains synthetic IPC evidence only.
