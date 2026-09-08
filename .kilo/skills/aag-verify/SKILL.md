---
name: aag-verify
description: Run the AAG verification gate (typecheck + vitest + cargo check + ui tsc + vite build) and report pass/fail per command. Use after any change to src/, src-tauri/, or ui/ to confirm nothing is broken before advancing a phase or committing.
---

# Skill: aag-verify

Run AAG's 5-command verification gate. This is the single most-repeated
workflow in the repo: `PROGRESS.md` mandates all five green before any phase
step advances, and behavioral rule §3 of `AGENTS.md` requires the smallest
meaningful check after every change.

## The gate (baseline, verified 2026-07-25)

All five must pass. Run from the repo root unless noted. The commands are
independent; run them in parallel when possible.

| # | Command | Workdir | What it checks | Expected |
|---|---------|---------|----------------|----------|
| 1 | `npm run typecheck` | repo root | `src/` TypeScript (tsc --noEmit) | clean, no output |
| 2 | `npm test` | repo root | `tests/e2e.test.ts` vitest suite (8 tests, mock substrate, no API keys) | 8 passed |
| 3 | `cargo check` | `src-tauri/` | Rust backend compiles | clean (warnings OK; "never used" expected mid-phase) |
| 4 | `npx tsc --noEmit` | `ui/` | React frontend types | clean, no output |
| 5 | `npx vite build` | `ui/` | Produces `ui/dist/` (Tauri build needs it) | build OK |

## Optional, deeper

- `cd src-tauri && cargo test` - inline `#[cfg(test)]` unit tests (DAO + breaker
  + sanitize). Use `Database::memory()` for DB tests. Run after Rust logic
  changes.
- `npx vitest run -t "failover"` - single test by name (Node).

## Decision tree: which subset to run

- **Changed only `src/` (Node)**: run #1 + #2.
- **Changed only `src-tauri/` (Rust)**: run #3 (and `cargo test`). Run #5 only if
  you suspect `generate_context!` would break (i.e. `tauri.conf.json` / `ui/dist`
  changed) - normally Rust-only edits don't need a UI rebuild, but `cargo check`
  still requires `ui/dist/` to exist.
- **Changed only `ui/`**: run #4 + #5.
- **Cross-tree / porting / before committing / before advancing a phase**: run
  ALL FIVE (the full gate).
- **First time in a fresh checkout**: run #5 once before #3 (Tauri gotcha:
  `ui/dist/` must exist or `cargo check` panics in `generate_context!`).

## Tauri `ui/dist/` gotcha (must know)

`tauri.conf.json` sets `frontendDist = "../ui/dist"`. The `generate_context!`
proc macro reads this at compile time. If `ui/dist/` does **not** exist:
`cargo check` and `cargo build` fail with a proc-macro panic, NOT a normal
Rust error. Fix: `cd ui && npx vite build` once. `npm run tauri:dev` does not
need `dist/` (uses the dev server) but still runs `cargo check` internally, so
build `dist/` at least once before the first `tauri:dev`.

## Procedure

1. Identify which trees the change touched -> pick the subset from the decision
   tree.
2. Run the chosen commands (parallelize the independent ones).
3. For each command, capture: exit status + key output line. A command is GREEN
   only on a clean exit. Treat warnings in `cargo check` as OK; treat ANY
   `error[` / `cannot find` / `FAILED` as RED.
4. Report a compact table: command | status (GREEN/RED) | note. If RED, quote
   the first error line and the file:line.
5. Do NOT mark work done or advance a `PROGRESS.md` phase step unless the full
   relevant subset is GREEN.

## Known transient blockers (don't "fix" these)

- `npm test` re-creates `~/.ai-agent-gateway/config.json` on first `loadConfig()`
  (`src/config/store.ts:100-103`). This is intended; tests are hermetic after.
- The old `circuit_breaker.ts` vs `circuit-breaker.js` import typo was resolved
  (file renamed to `circuit-breaker.ts`). If typecheck ever fails with
  `TS2307 Cannot find module '../circuit-breaker.js'`, the rename was reverted -
  re-rename the file (one char, export name `CircuitBreaker` unchanged).
