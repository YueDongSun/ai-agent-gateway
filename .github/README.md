# `.github/` — Agent instructions and CI workflows

Files that GitHub loads for the repository: Copilot agent instructions
and the manual desktop build workflow.

## Anchor files

- [`copilot-instructions.md`](copilot-instructions.md) — repository
  rules for GitHub Copilot. Read `AGENTS.md` first, then this file.
  Documents the runtime map, the naming and ownership rules, the
  verification gate, and the known contracts (snapshot routing,
  one-shot streaming failover, unsigned `v0.2.0` preview).
- [`workflows/`](workflows/README.md) — CI workflows. Today: the
  manual-only desktop build matrix.

## Key contracts

- `.github/copilot-instructions.md` is project-controlled; do not
  rewrite without aligning it with `AGENTS.md`.
- Workflows in `.github/workflows/` run with `contents: read`
  permission only and never publish a GitHub Release.

## See also

- [`workflows/`](workflows/README.md) — CI workflows.
- [`../AGENTS.md`](../AGENTS.md) — repository rules for coding agents.
- [`../docs/architecture.md`](../docs/architecture.md) — module
  ownership.
- [Root README](../README.md).