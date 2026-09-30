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

Every provider page shows the detected operating system, configuration source,
and exact files the next activation will use. Provider selection applies the
selected provider to those files and updates the current-provider marker only
after the write succeeds. Selecting the current provider again reapplies its
configuration. Starting Claude Code, Codex CLI, Copilot CLI, or a VS Code
extension refreshes its current configuration first and then launches it.
Gemini, Pi, OpenCode, and Kilo use their discovered global files when the user
activates their provider. Backups are private under AAG's backup directory.

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
separated by editor, agent and a hash of the injected environment. An explicit
proxy/token change therefore selects a separate profile instead of silently
reusing an older environment. These profiles persist; user settings and logins
from the native editor are not copied. The selected editor's installed extension
directory is supplied separately. Portable VS Code installations, whose `data`
directory overrides CLI profile options, require separate native acceptance.

Credentials are passed as environment values, never logged or returned in launch
receipts. Windows terminal launch source is fixed and does not interpolate
credential values. Unix terminal scripts use a private random directory, private
atomic file writes and shell quoting; they unlink themselves before starting the
agent. Failed dispatch removes the directory. macOS uses `/usr/bin/open` with
Terminal's bundle identifier and an executable document argument, so dispatch
does not require AppleEvents automation authorization. Unexecuted scripts expire
after 60 seconds while AAG is running. Terminal preferences may retain the window
after the child exits.

Working directories must be absolute existing directories. A failed `cd` cannot
continue into the agent. Metadata probes have a five-second timeout, bounded
captured output and child kill/wait on failure. GUI dispatch detects immediate
process errors and reaps long-lived children when they exit. Dispatch success
does not prove that an external application's window or extension is ready.

The Windows desktop mirror preserves its existing profile/login on ordinary
launch. Its explicit reset command retains the existing guarded reset boundary.
Process-query errors are propagated instead of being reported as zero processes;
the process filter uses the configured mirror profile, including custom paths.
Codex exclusive/coexist and Claude token/passthrough modes retain the upstream
contracts. Their desktop mirror launchers remain Windows-only; passive macOS
CLI/editor detection does not imply support for those desktop mirror surfaces.

## Configuration directories

VS Code and Antigravity use APPDATA on Windows, Application Support on macOS,
and an absolute XDG_CONFIG_HOME (or the home .config directory) on Linux.
An explicit AAG_HOME_DIR takes priority over inherited APPDATA/XDG_CONFIG_HOME,
so isolated tests and portable runs cannot write to a native application profile.
The AAG SQLite location and provider schemas are unchanged.

## Verification

The automatic `verify:features -- --scope all --run` runner detects prerequisites
and supplies private canonical temporary directories without manual environment
setup. For direct Cargo commands, run serially with private AAG_CONFIG_DIR and
AAG_HOME_DIR and set TMPDIR to its real path on macOS, for example `/private/tmp`.
The credential-storage boundary deliberately rejects symbolic-link components.

Launcher regressions execute local fixture programs to check literal environment
delivery, argument boundaries, working-directory failure, script deletion, output
limits and timeout reaping. Platform tables cover installation candidates and
terminal arguments. These fixtures do not launch a real coding agent or submit
provider requests. Windows console/Store behavior and Linux desktop terminals
still require execution on their own native hosts. Browser smoke detects the
host's installed Chrome/Edge/Chromium and remains synthetic IPC evidence only.
