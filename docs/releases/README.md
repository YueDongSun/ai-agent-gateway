# `docs/releases/` — Per-release notes

Release-by-release notes describing what changed, what was verified, and
where the distribution boundary sits. Each release has its own file
named after the version (e.g. `v0_2_0.md`).

## Anchor files

- [`v0_2_0.md`](v0_2_0.md) — the unsigned `v0.2.0` desktop preview.
  Closes the Phase 23 state-lifetime repairs, aligns npm browser and
  Tauri builds, and introduces the shared vector branding and the
  release / development / demo build badge. Documents the local
  verification gate (Node, Rust, browser smoke, release preflight,
  Windows NSIS packaging) and the explicit boundary for native
  installation, signing, notarization, updater delivery, macOS / Linux
  execution, real upstream requests, and live agent switching.

## Key contracts

- Release notes describe the change set plus the verification evidence
  that was recorded when the release was prepared. They do not
  retroactively claim new evidence.
- Distribution boundaries are explicit: native installers are packaging
  candidates, not proof of install / upgrade acceptance.

## See also

- [`../desktop_release.md`](../desktop_release.md) — release pipeline.
- [`../architecture.md`](../architecture.md) — module ownership.
- [`../../PROGRESS.md`](../../PROGRESS.md) — current status.