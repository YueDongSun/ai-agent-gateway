# Public baseline notes (v0.2.0 product line)

This repository is a sanitized, history-free publication baseline. It contains
no private author identities, no private commit hashes, no remotes, no tags,
and no credential or machine-specific material. All commits share one public
publisher identity on a GitHub noreply address.
Commit dates are publication-sequence dates, not original authoring times.

## Milestone map

The history below reconstructs the product line up to the v0.2.0 desktop
preview as coarse, reviewable milestones. Newer work (DSH marketplace and
web UI, MCP workspace, mirror credential hardening, Responses relay fixes,
additional provider adapters) is intentionally excluded from this baseline
and can follow as later public releases.

1. Initial Node gateway, proxy and configuration switcher
2. Tauri migration phases 0-2 with agent instructions and skills
3. Copilot and Pi switching, browser demo mode, English localization
4. Official model catalog, Anthropic-shaped models API, failover seeds
5. Smart provider form, vendor management, themes and layout
6. Standalone upstream provider with coexistence detection and failover queue
7. Codex mirror support, model catalogs, message tool-use fidelity
8. Route editor persistence, gateway settings, upstream controls
9. Smoke hardening, state context split, test stabilization
10. v0.2.0 desktop preview closure
11. Public baseline notes and snapshot manifest (this commit)

## Deviations from a literal replay

- Public-packaging files ride along from milestone 1 so that every commit is
  independently auditable: `LICENSE`, `SECURITY.md`, `.npmignore`,
  `.env.example`, the final `.gitignore`, and the npm `private`/`files`
  fields in `package.json`. They carry no product behavior.
- Privacy tooling (`scripts/privacy/`, `tests/privacy/`, related privacy
  tests, the `privacy_gate` workflow, `docs/public_release_privacy.md`, and
  the `privacy:*` npm scripts) is included for the same reason.
- `shared/dsh_market_catalog.json` is an empty placeholder satisfying the
  export size gate. The marketplace arrived after v0.2.0; the stub carries
  no entries and nothing in this tree reads it.
- Three unreferenced root diagnostics (`check_codex.py`, `check_db.py`,
  `check-db.cjs`) were removed in the final commit; they carried
  machine-specific paths and were never part of the build.
- Two documentation lines were sanitized at introduction: a machine-specific
  path note in `AGENTS.md` and a secret-shaped example value in `README.md`.
- A stale filename duplicate from an early rename (`circuit_breaker.ts`
  alongside the v0.2.0 `circuit-breaker.ts`) was dropped; only the v0.2.0
  filename remains.
- `public_snapshot_manifest.json` is the byte-identical envelope produced by
  the privacy exporter for the sanitized source tree: policy hash,
  export-tree digest, per-file hashes, redacted gate summary, link check,
  and npm package-surface check. It references no private commit or
  timestamp. This notes file is the only tracked addition beyond the
  manifest envelope.

## Verification performed before publication

- Exporter privacy audit of the sanitized source tree: 0 blockers.
- No-Git directory audit of the exported tree against the manifest:
  0 blockers, manifest-bound, no broken local links, clean npm surface.
- Post-reconstruction checks in this repository: per-file bytes match the
  manifest for all 190 envelope files; Git-tree audit and full-history
  audit report 0 blockers; no remotes or tags present.

## Publishing

Push to a brand-new empty remote only. Never push the private preparation
branch or mirror private refs into this history. Before accepting
contributions, enable secret scanning with push protection, private
vulnerability reporting, branch protection, and the tracked `privacy_gate`
check.
