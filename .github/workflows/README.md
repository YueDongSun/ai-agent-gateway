# `.github/workflows/` — CI workflows

GitHub Actions workflows. Privacy checks run automatically for pushes and pull
requests. Functional regressions run automatically for relevant changes. The
desktop build matrix remains manual-only.

## Anchor files

- [`desktop_build.yml`](desktop_build.yml) — manual-only workflow
  (`on: workflow_dispatch`). Builds Windows x64, macOS Intel, macOS
  Apple Silicon, and Ubuntu 22.04 x64 with the Tauri build matrix.
  Uses `contents: read` permission only and uploads CI artifacts; it
  creates no GitHub Release. Each matrix member has an independent
  45-minute limit and does not cancel the others on failure. Installs
  dependencies with `npm ci --ignore-scripts` from tracked root/UI lockfiles;
  Rust uses the tracked Cargo lock. Third-party Actions are pinned to commits.

- [`privacy_gate.yml`](privacy_gate.yml) - automatic, non-publishing privacy
  validation for every push and pull request. It installs the locked root
  dependencies without lifecycle scripts, runs the privacy regression suite,
  and audits the reviewed repository tree. It has read-only contents
  permission and neither uploads artifacts nor creates a release.

- [`functional_regression.yml`](functional_regression.yml) — automatic Node,
  React/browser-demo and ordinary Rust checks selected by changed paths. It
  installs tracked dependency locks and runs browser suites sequentially. The
  Rust job builds the frontend and populates a cold dependency cache before
  offline verification. It creates no package, release or uploaded artifact;
  native Windows qualification and real-provider acceptance remain separate.

## Key contracts

- The automatic privacy and functional workflows cannot publish or upload
  build artifacts. `desktop_build.yml` is intentionally manual so a
  release candidate is never produced without an explicit human decision.
- Workflows run with `contents: read` permission only. No secrets,
  no signing credentials, no notarization credentials, no updater
  private key, no update endpoint.
- Successful CI builds are necessary but not sufficient for a
  release. Native installation, signing, notarization, and manual
  acceptance remain separate evidence requirements.

## See also

- [`../copilot-instructions.md`](../copilot-instructions.md) —
  repository rules.
- [`../../docs/desktop_release.md`](../../docs/desktop_release.md) —
  release pipeline.
- [`../../docs/releases/v0_2_0.md`](../../docs/releases/v0_2_0.md) —
  current preview distribution boundary.
- [Root README](../../README.md).
