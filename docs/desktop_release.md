# Desktop Release Preparation

The desktop release version is owned by `src-tauri/tauri.conf.json`. It must match `ui/package.json` and the `[package]` version in `src-tauri/Cargo.toml`. The root `package.json` is the legacy Node CLI and intentionally remains on its independent version.

This is preparation guidance, not evidence of signing, installation, upgrades,
or publication permission. Use the complete gate in the
[development guide](development.md) before a release. Direct Node/Rust tests
require disposable private `AAG_CONFIG_DIR` and `AAG_HOME_DIR` values with
`AAG_TEST_CCSWITCH_DB` unset; do not test storage against a live home.

Run the local preflight after generating icons and before a desktop build:

```powershell
npm run icons:generate
npm run release:verify
npm run desktop:build
```

`release:verify` runs the release-input preflight and privacy audit, not the full
unit, browser, or native acceptance gate. `desktop:build` builds the native
frontend into `ui/dist`, which remains the Tauri `frontendDist`. The browser
demonstration build is separate and writes to `ui/dist_demo` through
`npm run ui:build_demo`.

## npm UI and Desktop Entry Points

Run commands from the repository root. Use `npm run ui:demo` for the current
browser source and `npm run ui:preview` for a freshly built browser demo. Use
`npm run desktop:dev` for a Tauri window with one owned Vite server and
`npm run desktop:build` for native bundles. The existing `tauri:dev` and
`tauri:build` names remain available. Root `npm run dev` still starts the Node
TUI; it is not the desktop UI.

The build launcher relays handleable `SIGINT`/`SIGTERM` events to its foreground
CLI, waits for its exit, and reports cancellation as nonzero. This does not
guarantee cleanup after uncatchable termination or of detached build helpers.

Demo builds use synthetic data only when no native bridge is present. Release
builds do not silently activate the mock backend in a browser. The main
application shell displays the desktop version and a demo, development, or
release build badge. A release badge identifies compilation mode, not signing or
native acceptance.

## Startup and Tray Lifecycle

`npm run tauri:dev` starts the Vite-served React UI inside the Tauri desktop
window. On each release launch, the desktop executable checks login-startup
registration and attempts to enable it if disabled, using `--aag-autostart` for
the login entry. Registration failures are logged rather than aborting startup.
The development executable does not enable registration because it depends on
the Vite server owned by `tauri:dev`.

Login launches leave the main window hidden. Proxy startup is attempted
asynchronously only when the saved `proxy_config.enabled` is true; a configuration
read or proxy-start failure is logged, not proof of a running listener. Tray-only
launch success therefore does not establish gateway availability.

The system tray uses `src-tauri/icons/tray_icon.png`. Left-click, Show, and the
Hide menu item control the main UI; Quit is the explicit process-exit action.
Closing the main window hides it to the tray rather than exiting. Tray
installation failure aborts startup rather than leaving a hidden app without a
tray entry. Actual icon visibility and interaction still require native acceptance.

### Console-free local build and lifecycle checks

On Windows, the release executable uses the GUI subsystem; the debug executable
uses the console subsystem. Hiding a debug console does not change that binary
contract. Build a local release executable without installers or signing:

```text
npm run desktop:build -- --no-bundle -- --offline
```

A normal launch shows the main window. `--aag-autostart` instead leaves it
tray-only. The independent `--aag-no-autostart` argument suppresses registration
for that launch, including a disposable acceptance run. It does not remove or
change an existing login-startup entry, and it does not change window visibility.

After exiting any running AAG, run from a non-elevated Windows shell:

```text
npm run test:desktop_native
```

The runner requires a built GUI-subsystem release executable. It owns disposable
AAG home/configuration, diagnostics logs and WebView2 directories, passes
`--aag-no-autostart`, and refuses to replace a detected existing AAG process.
Preflight checks both the executable name and the locked Tauri plugin's named
single-instance mutex, so a renamed live executable also blocks the run.
The application creates its configured main window during setup with an explicit
WebView2 data directory under `AAG_CONFIG_DIR/webview`; the runner verifies the
resulting `EBWebView` profile there without setting `WEBVIEW2_USER_DATA_FOLDER`.
It checks actual console attachment,
native IPC/rendering, authenticated local health without model generation,
close-to-tray, native tray callbacks/menu dispatch, single-instance reopening,
proxy restart, graceful Quit, tray-only startup, persisted configuration and
unchanged client-state fixtures. The process driver binds actions to the expected
executable, PID and creation time. Failure or unavailable prerequisites return
nonzero; forced cleanup is not reported as a graceful Quit. Windows cleanup
confirms only the foreground process, not detached WebView2 descendants. Abnormal
owned-process cleanup therefore retains disposable state instead of deleting it;
`STATE=removed` describes directory removal, not verified descendant exit.

The native screenshot is written to ignored `tmp/desktop_native_normal.png`.
These are automated native lifecycle checks, not visible shell-icon, physical
tray-click, installer, upgrade, signing, performance or real-upstream acceptance.
Rendering exceptions are checked only after CDP attachment. The disabled,
no-model supplier fixture does not exercise mirror writers; unchanged fixture
files establish only preservation during these lifecycle operations. The runner
compares both Windows startup-registration values and top-level live
local-application-data metadata after each owned launch and during failure cleanup,
without repairing persistent state on its own. That metadata comparison is a
tripwire, not proof that no nested live file was written. The DSH runtime and
marketplace windows remain outside this main-window lifecycle check. The existing
browser smoke remains a separate synthetic evidence stream.


## Icon Contract

The release preflight reads icon headers without an image library. It requires
8-bit RGBA square PNG headers at 32, 128, 256 (`128x128@2x`), 512 (`icon.png`),
and 32 (`tray_icon.png`) pixels. It checks ICO directory bounds and entries for
16, 24, 32, 48, 64, and 256 pixel layers. For ICNS it checks container length,
chunk bounds, and nonempty required layer tags for 32, 128, 256, and 1024 pixels.
These checks do not decode image payloads or prove native rendering. Generate
the bundle from `ui/public/app_icon.svg` with the installed Tauri CLI; do not
substitute a PNG file renamed as `.icns`.

See the official [Tauri icon guide](https://v2.tauri.app/develop/icons/) for supported bundle assets.

## CI Build Matrix

`.github/workflows/desktop_build.yml` is manual-only. Its matrix is configured
for Windows x64, macOS Intel, macOS Apple Silicon, and Ubuntu 22.04 x64, with CI
artifact upload. It has `contents: read` permission only and creates no GitHub
Release. Each matrix member has an independent 45 minute limit and does not
cancel the others on failure.

The workflow configures native runners, Linux dependencies, Node, and Rust
caching; see the [Tauri GitHub pipeline guide](https://v2.tauri.app/distribute/pipelines/github/)
for general setup guidance. Root/UI npm lockfiles and `src-tauri/Cargo.lock` are
tracked; CI restores npm dependencies with `npm ci --ignore-scripts` and builds
Rust with `--locked`. Third-party Actions are pinned to immutable commit SHAs.
Lockfiles and pins improve reproducibility but do not replace SBOM, signature,
provenance, or dependency-vulnerability review.

The workflow passes its matrix `--target` through `desktop:build` to Tauri's
target option. The build launcher keeps Tauri options before the runner separator
and always retains Cargo `--locked`. Verify the packaged artifact's architecture
in each job; the matrix label alone does not establish the bundling target.

Windows can be exercised locally. macOS and Linux output requires successful
builds on the appropriate native hosts or workflow runners. A Windows machine
must not claim to have built or validated macOS and Linux artifacts from local
Windows checks alone.

## Signing, Notarization, and Updates

The build workflow deliberately has no signing secrets, notarization credentials,
updater private key, or update endpoint. An explicitly authorized preview may
publish unsigned candidates as a GitHub prerelease with these limitations stated.
Do not promote it to a signed stable distribution or enable automatic updates
until the following are configured and independently tested:

- Windows signing identity and installer verification, following [Tauri Windows signing guidance](https://v2.tauri.app/distribute/sign/windows/).
- macOS Developer ID signing, notarization, stapling, and launch on both Intel and Apple Silicon, following [Tauri macOS signing guidance](https://v2.tauri.app/distribute/sign/macos/).
- A stable updater endpoint and matching public key, following the [Tauri updater documentation](https://v2.tauri.app/plugin/updater/).

## Manual Acceptance Matrix

For each native artifact, install and launch it on its target operating system. Confirm the application icon is visible in the OS launcher or taskbar, then verify tray show, hide, and quit behavior, deep-link handling, and CLI invocation. Verify both a fresh install and an upgrade path after signing and update infrastructure are configured.

The Codex Desktop mirror behavior is Windows-only and remains a separate local integration check; it is not evidence of macOS or Linux compatibility.

## Preview Publication and Rollback

The v0.2.0 preview notes are in [the release notes](releases/v0_2_0.md). Public
publication requires explicit authorization and the separate, audited
history-free checkout described in the [privacy guide](public_release_privacy.md).
Never publish private development branches or copy their refs and tags. Run the
complete local gate, review the intended source diff, and bind release assets to
an explicit verified commit and tag in the authorized release checkout; do not
let a release tool silently select a newer default-branch commit. Include SHA-256
checksums for attached binaries and keep repository visibility unchanged.

Native installers are built by Tauri on their host OS. To restrict a Windows
candidate to NSIS, pass the bundler options through either build entry point:

```text
npm run desktop:build -- --bundles nsis --ci
```

Options before an explicit `--` go to Tauri; options after it go to Cargo, with
`--locked` retained automatically. For example, to select a Tauri target and
also require Cargo's local cache:

```text
npm run desktop:build -- --target x86_64-apple-darwin -- --offline
```

The `tauri:build` alias uses the same argument contract. Packaging success is not
an installation or upgrade test. If native build matrix artifacts are attached
later, record their source commit, artifact architecture, and successful job
result separately from manual device acceptance.

Before testing an upgrade, exit AAG and back up its application data directory
(`~/.aag` by default, or `AAG_CONFIG_DIR`) and any agent files the user intends
to switch. Database initialization applies pending schema migrations; do not
assume an older binary can read the upgraded database. Keep the previous app
binary and its matching stopped-app backup for rollback testing. These are manual
procedures, not evidence that rollback or live configuration switching has been
performed.
