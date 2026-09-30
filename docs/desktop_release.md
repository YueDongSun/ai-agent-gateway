# Desktop Release Preparation

The desktop release version is owned by `src-tauri/tauri.conf.json`. It must match `ui/package.json` and the `[package]` version in `src-tauri/Cargo.toml`. The root `package.json` is the legacy Node CLI and intentionally remains on its independent version.

Run the local preflight after generating icons and before a desktop build:

```powershell
npm run icons:generate
npm run release:verify
npm run desktop:build
```

`desktop:build` builds the native frontend into `ui/dist`, which remains the Tauri `frontendDist`. The browser demonstration build is separate and writes to `ui/dist_demo` through `npm run ui:build_demo`.

## npm UI and Desktop Entry Points

Run commands from the repository root. Use `npm run ui:demo` for the current
browser source and `npm run ui:preview` for a freshly built browser demo. Use
`npm run desktop:dev` for a Tauri window with one owned Vite server and
`npm run desktop:build` for native bundles. The existing `tauri:dev` and
`tauri:build` names remain available. Root `npm run dev` still starts the Node
TUI; it is not the desktop UI.

Demo builds use synthetic data only when no native bridge is present. Release
builds do not silently activate the mock backend in a browser. Every UI displays
the desktop version and a demo, development, or release build badge. A release
badge identifies compilation mode, not signing or native acceptance.

## Startup and Tray Lifecycle

`npm run tauri:dev` starts the Vite-served React UI inside the Tauri desktop
window. The packaged desktop executable registers itself for login startup on
its first release launch and passes `--aag-autostart` to that entry. Login
launches keep the main window hidden while the process and configured proxy run
in the background. The development executable does not register itself for
autostart, because it depends on the Vite server owned by `tauri:dev`.

The system tray uses `src-tauri/icons/tray_icon.png`. Left-click, Show, and the
Hide menu item control the main UI; Quit is the explicit process-exit action.
Closing the main window hides it to the tray rather than exiting. Tray icon
installation is required at native startup so a hidden window always has a
visible recovery path.

## Icon Contract

The release preflight reads icon headers without an image library. It requires RGBA square PNGs at 32, 128, 256 (`128x128@2x`), 512 (`icon.png`), and 32 (`tray_icon.png`) pixels. It also verifies ICO directory bounds and 16, 24, 32, 48, 64, and 256 pixel layers, and verifies a real ICNS container with meaningful 32, 128, 256, and 1024 pixel layers. Generate the bundle from `ui/public/app_icon.svg` with the installed Tauri CLI; do not substitute a PNG file renamed as `.icns`.

See the official [Tauri icon guide](https://v2.tauri.app/develop/icons/) for supported bundle assets.

## CI Build Matrix

`.github/workflows/desktop_build.yml` is manual-only. It builds Windows x64, macOS Intel, macOS Apple Silicon, and Ubuntu 22.04 x64, then uploads CI artifacts. It has `contents: read` permission only and creates no GitHub Release. Each matrix member has an independent 45 minute limit and does not cancel the others on failure.

The workflow follows the current [Tauri GitHub pipeline guide](https://v2.tauri.app/distribute/pipelines/github/) for the runner setup, Linux dependencies, Node, and Rust caching. Root/UI npm lockfiles and `src-tauri/Cargo.lock` are tracked; CI restores npm dependencies with `npm ci --ignore-scripts` and builds Rust with `--locked`. Third-party Actions are pinned to immutable commit SHAs. Lockfiles and pins improve reproducibility but do not replace SBOM, signature, provenance, or dependency-vulnerability review.

Windows can be exercised locally. macOS and Linux output is unverified until this workflow has run successfully on those native runners. A Windows machine must not claim to have built or validated the macOS and Linux artifacts.

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

The v0.2.0 preview notes are in `docs/releases/v0_2_0.md`. Publish only after the
local gate passes, inspect the intended diff, and commit source before creating
the tag. Use an explicit verified tag for the release; never let the release
tool silently select a newer default-branch commit. Include SHA-256 checksums
for attached binaries and keep the repository's existing visibility unchanged.

Native installers are built by Tauri on their host OS. Building a Windows NSIS
candidate can be restricted to `npm run desktop:build -- --bundles nsis --ci`.
Packaging success is not an installation or upgrade test. If native build
matrix artifacts are attached later, record their source commit and successful
job result separately from manual device acceptance.

Before testing an upgrade, exit AAG and back up its application data directory
(`~/.aag` by default, or `AAG_CONFIG_DIR`) and any agent files the user intends
to switch. This release does not add a schema migration. Keep the previous app
binary and restore the stopped-app backup if rollback is needed. No rollback
or live configuration write was performed as part of release preparation.
