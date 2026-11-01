# Managed DSH integration

AAG now provides a native management page and Rust backend for one isolated
DeepSeek Harness instance. This is a local preview implementation, not a bundled
zero-configuration DSH distribution or a security certification for community
plugins. The original [research proposal](dsh_plugin_management_research.md)
also describes capabilities that remain outside this implementation.

## What is implemented

- A lazy-loaded DSH page with English/Chinese text, appearance preference,
  categorized plugin catalog, exact-version installation, bundle enable/disable,
  removal, operation previews, cancellation, receipts, and runtime controls.
- A managed profile under the resolved AAG configuration directory. Refreshing
  the page reads state only: it does not initialize, probe, download or execute.
- Native Claude/provider reuse from AAG's existing database, including providers
  previously imported through the existing cc-switch flow. No native Claude file
  is read or overwritten by this feature.
- Separate API-key and Bearer authentication. A saved Claude API key is referenced
  through AAG_DSH_API_KEY; a saved ANTHROPIC_AUTH_TOKEN uses the published
  Anthropic catalog adapter and ANTHROPIC_AUTH_TOKEN. Credentials go only into
  the selected child environment, never plugin manifests or frontend DTOs.
- Optional use of the actual running AAG gateway and its loaded model snapshot.
  This uses Anthropic Messages on the DSH-to-AAG connection. Individual upstream
  hops can still involve the existing cross-protocol conversion. A stopped or
  pinned gateway is not offered as a usable automatic fallback.
- An AAG-owned Claude-style DSH Workspace is available from the DSH page. Its
  sidebar exposes the saved/native, pool, and gateway sources with their
  advertised models. Provider changes use the same review-bound managed-settings
  transaction as the settings form; the workspace never sends a prompt itself.
- An authenticated loopback DSH WebUI opened in a separate native window.
  DSH content has no AAG command permissions. The trusted manager remains usable
  when DSH fails.

## Setup and use

The supported package boundary is DSH **0.1.1-rc.2**, pnpm **10.34.5**, and a Node
runtime supported by those packages. Development verification used Node 24.
AAG does not install or switch a global Node environment. The pinned DSH package
requires --expose-internals; AAG sets this only on its owned Node process, not
through global NODE_OPTIONS.

1. Open the DSH tab. Existing installations are detected without execution.
   Use Auto setup to review detected paths, the current provider/model and
   managed-profile initialization. Missing or ambiguous values stay explicit.
    The manual settings form remains available: supply absolute paths to the Node executable,
    the installed DSH JavaScript entry (normally the package's lib/bin.js), and
    pnpm's JavaScript entry (normally bin/pnpm.cjs).
    A package resolved by the official `npx @deepseek-ai/dsh@0.1.1-rc.2` is also
    detected from the bounded local npm `_npx` cache and uses its verified
    `lib/bin.js` entry directly; AAG does not execute a Windows `npx.cmd` shell
    shim or resolve a floating package during start.
2. Select an optional saved Claude/native provider, pool provider, or running
   gateway, and choose a model advertised by that source. Review and confirm
   the settings change. Omitting the source is permitted; no model request is
   automatically sent.
3. Use Open DSH workspace for the Claude-style AAG shell when you want a focused
   provider/model view. Select Use provider in its sidebar to open the same
   review dialog. Open native WebUI when DSH is running to start the actual
   conversation; the AAG shell intentionally does not proxy prompts.
4. If Auto setup has not already initialized the managed profile, review and
   confirm initialization with the existing control.
5. For a community plugin, enter an exact published version and review its
   installation. Dependencies may be downloaded, but package lifecycle scripts
   and pnpm hooks remain disabled.
6. Start the instance and open its WebUI. Stop DSH before changing runtime
   settings or plugin packages. Start does not automatically open a window.

The parent runtime installation is a prerequisite, not something the
Initialize button downloads. A disposable development installation can be
prepared explicitly with:

~~~text
On Windows x64, create <runtime_directory> and copy scripts/dsh_test_runtime_windows.json to its package.json.
npm install --prefix <runtime_directory> --registry=https://registry.npmjs.org --ignore-scripts --legacy-peer-deps --omit=optional
~~~

The test manifest pins required peer packages and Windows prebuilt binaries as well as DSH and pnpm. Installing only those last two packages with legacy peer resolution omits required runtime dependencies. Other platforms require their own native packages. This is the Windows dependency variant used in local integration testing. Do not infer
that optional native capabilities or plugins requiring build scripts are
supported. The earlier researched GitHub alpha version was not published on npm
when checked; package versions are verified from manifests before execution.

## Ownership and storage

The DSH service owns the following paths under the AAG configuration directory:

~~~text
dsh/
  aag_settings.json
  aag_model_binding.json
  settings.json
  aag_runtime.patch.json
  dsh_preload.mjs
  profiles/web/package.json
  profiles/web/pnpm-lock.yaml
  profiles/web/node_modules/
  operations/<operation_id>/
  workspace/
  user_home/
~~~

The frontend receives a sanitized snapshot; it never accesses these files
directly. The DSH settings provider explicitly targets settings.json through an AAG-owned composition overlay, so the original WebUI preserves the same JSON format. AAG updates only
its managed instance and refuses unparseable externally edited documents instead
of overwriting them. The existing Node gateway retains its independent runtime.

Before an explicit DSH launch or package-manager operation, the shared runtime
environment prepares the directories referenced by HOME, USERPROFILE, AppData
and XDG variables. Windows also receives an existing managed Desktop directory
for its native folder picker and a validated SystemDrive (derived from an
absolute SystemRoot only when SystemDrive is absent). Directory preparation
rejects linked paths and conflicting files without replacing their contents.
Discovery, command construction and operation previews remain read-only.

The managed workspace is the initial working directory, not a filesystem
sandbox. DSH's own WebUI can select other directories. This changes neither
the OS user's permissions nor the separate review needed before running plugins.

AAG tracks its selected provider route and default model separately. It preserves
unrelated providers added through the WebUI; clearing the selection removes only
the AAG-owned route and restores the previous user default when applicable.
The appearance setting maps to the verified DSH ui-theme.preference field.

Credential files in the managed DSH home can override injected credentials in
the upstream runtime. Startup refuses a conflicting credential/environment store
rather than silently using the wrong provider or overwriting secrets. Resolve
that conflict explicitly; the feature never adopts the user's default DSH home.

Operations hold the OS-backed AAG_CONFIG_DIR/dsh_instance.lock outside DSH_HOME
so DSH file watchers do not collide with the Windows exclusive handle. The lock
is released by the OS on process termination, even though the file remains.
Operations also use version/digest checks, a staging directory
and backups. A preview expires after five minutes and is consumed once, including
after failure. The frontend requires acknowledgement before applying it.
Reusing a consumed preview is rejected instead of running a second operation.

Installation and removal use structured Node/pnpm arguments. Only curated
package names with exact versions are accepted; arbitrary URLs, Git specs,
shell fragments and filesystem package specs are not accepted by this API.
The active profile is changed only after stage identity and bundle-path checks.
Failed promotion attempts restore the affected original files, or return
rollback_failed distinctly. The retained operation directory is recovery
evidence, not a reusable installation cache. An unresolved mid-promotion crash
marker blocks further mutation/startup with interrupted_transaction. Recovery
after abrupt termination during promotion requires reviewing the journal and
restoring a consistent backup; do not simply delete the marker. Automatic
power-loss recovery and backup garbage collection are not implemented.

## State and failure semantics

Installed package version, configured enablement and runtime execution are
different facts. The current catalog reports runtime_phase as unknown; it does
not fabricate a per-bundle execution result from an installed dependency.
The runtime's web_ready state verifies the matching authenticated listener and
an actual DSH HTML page, not that every tool or plugin works.

The curated catalog currently includes core/base and WebUI references,
appearance skins, cost-meter and MCP-client candidates. Some entries are plain
modules rather than dsh.bundle packages; they can be installed but cannot be
enabled through bundle toggling. Core composition entries cannot be removed.
Package installation does not configure an MCP server, grant permissions, or
activate a Skill. Cost-meter values are estimates, not billing records.

Runtime settings and plugin changes are refused while the instance is active.
Startup, cancellation, stop and process-exit monitoring use a generation ID so
an older request cannot kill or mark a replacement instance. Startup also binds
to the settings digest used for upstream resolution. Child processes have
bounded execution and explicit descendant cleanup; desktop exit cancels an
active job before stopping the runtime.

Hard AAG exit is platform-asymmetric. Windows assigns child processes to a job
object the operating system tears down when AAG exits, while Unix teardown
relies on AAG's own cleanup handler. A crashed or force-killed AAG on Unix can
therefore leave the DSH process group orphaned, still holding its loopback
port, until it is stopped manually; no parent-death signal is installed for
the Unix group.

Errors use stable dsh-prefixed codes, including settings_changed,
profile_changed, instance_busy, exact_version_required, package_not_allowlisted,
credential_store_conflict, runtime_exited, startup_timed_out, cancelled and
rollback_failed. Original failures remain failures even when recovery succeeds.
Raw child stdout/stderr is deliberately not copied into AAG diagnostics.

## Native security boundary

The AAG-owned Node preload runs before DSH and guards the configured HTTP and
WebSocket listener. A cryptographically random token authenticates the backend
readiness probe. A private native bootstrap URL exchanges that token for an
HttpOnly, SameSite=Strict session cookie and redirects to the clean root URL.
The token and bootstrap URL are retained only in memory, not returned in the
snapshot or written to logs. Host and Origin checks reject foreign requests.

Tauri application commands have explicit generated permissions for the main
manager window. An additional command-dispatch guard rejects DSH runtime WebView labels.
The separate bundled marketplace has only three public catalog/source commands;
it cannot read provider credentials or control plugins. The DSH window permits only its owned loopback origin, denies popups and
downloads, and uses an isolated ephemeral browser context. AAG's own frontend
has an explicit CSP.

This transport guard is not an operating-system sandbox. DSH plugins run as the
DSH process user; plugins opening additional listeners, accessing workspaces or
using credentials still require their own review. The adapter does not turn
Claude subscription OAuth into a generic API key. Unsupported native auth
families and custom-header requirements fail explicitly.

## Modules and commands

- src-tauri/src/services/dsh/: configuration, catalog, installation transactions,
  child lifecycle and redacted upstream resolution.
- runtime_environment.rs: shared, explicit home preparation and filtered child
  environment; used by both DSH and pnpm without inheriting host credentials.
- owned_process.rs: process-tree ownership, timeout/cancellation and cleanup;
  runtime.rs owns DSH lifecycle and authenticated readiness.
- src-tauri/src/commands/dsh.rs: typed IPC and server-side credential resolution.
- src-tauri/src/dsh_window.rs and resources/dsh_preload.mjs: presentation and
  authenticated transport.
- ui/src/components/dsh/: manager orchestration, settings, catalog, confirmation
  dialog, receipts and the AAG-owned Claude-style workspace shell.
- ui/src/lib/api/dsh.ts: snake_case wire contracts.
- ui/src/lib/mock/dsh.ts: explicitly synthetic browser-demo state.

Read commands: dsh_get_snapshot and dsh_list_upstreams. Mutations:
dsh_save_settings, dsh_preview_operation, dsh_execute_operation,
dsh_cancel_operation, dsh_start and dsh_stop. Native presentation:
dsh_open_web_ui. None of the read commands probes an external provider.

## Verification and current limits

Use the existing full gate in [development.md](development.md), plus
npm run test:dsh_transport. The transport suite uses real owned HTTP/WebSocket
traffic and checks authentication, hostile origins, bootstrap cookies and
application-command coverage.

Rust tests exercise redaction, API-key/Bearer mapping, stale configurations,
one-use previews, package failure, cancellation, timeout, real subprocess
execution with deterministic package fixtures, startup identity races and
cleanup. Browser tests use synthetic IPC and are labeled accordingly.

The opt-in real-package acceptance test requires explicit test runtime paths;
it never reads a real provider key. Its results are recorded separately in
[dsh_validation.md](dsh_validation.md).

Deferred: shipping runtime binaries inside every installer; arbitrary marketplace
imports; named multi-instance profiles; a full MCP-server/Skill editor;
per-plugin permission sandboxing and runtime status aggregation; community
plugin qualification; signed releases and per-platform installation/upgrade
acceptance. The broad research proposal is not a claim that these are installed.


## Plugin marketplace

Use the Plugin marketplace button in the DSH page header to open the separate
bilingual discovery window. The market does not require DSH to be running.
See [marketplace guide](dsh_marketplace.md) for categories, provenance and refresh.

See [plug-and-play setup](dsh_plug_and_play.md) for detection locations,
provider precedence, transaction boundaries and forwarding changes.
