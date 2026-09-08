---
name: aag-phase-loop
description: Execute one bounded PROGRESS.md-driven migration sub-task for the AAG Tauri/Rust port. Use for the cron-driven (or manual) iteration loop - read PROGRESS.md, do ONE sub-task, verify with the full gate, then update PROGRESS.md log + next_action. Never advance a phase without green verification.
---

# Skill: aag-phase-loop

Execute ONE bounded migration sub-task for the Node->Tauri port, following the
loop discipline that `PROGRESS.md` documents. This is the workflow the 2-hour
cron uses; it also works for manual iterations.

## The loop (strict order)

1. **Read state.** Open `PROGRESS.md`. Note `Status`, `current_phase`, and
   `next_action`. Read `DESIGN.md` for the current phase's spec if needed.
2. **Pick ONE bounded sub-task.** Do not attempt a whole phase. One DAO, one
   provider, one command, one adapter, one route - sized so it can be verified in
   this iteration. If `next_action` names a multi-step Step, take just the first
   slice.
3. **Inspect before editing.** Read the relevant Node source (the behavior to
   port) and the Rust target file + surrounding conventions (see `aag-port-to-rust`).
   Trust code over docs; `README.md` is stale.
4. **Implement** the one sub-task, scoped to one tree (`src/` OR `src-tauri/`
   OR `ui/`). Follow existing patterns, naming, imports, ownership boundaries.
   No unrelated refactors, no dep upgrades, no global formatting.
5. **Verify** with the full gate (use `aag-verify`): `npm run typecheck`,
   `npm test`, `cargo check`, `cargo test` (Rust changes), `ui` tsc + vite build
   (UI changes). ALL must be green.
6. **Update `PROGRESS.md`:**
   - Append a dated entry under `## Log` (what was done, files touched,
     verification result, what's next).
   - Update `next_action` to the next bounded slice.
   - If a checklist step completed, tick its `[x]`.
   - If a phase's steps are all done, set `current_phase` to the next phase and
     update the `## Phase checklist`.
7. **Never advance without green.** If verification fails, set `Status: blocked`,
   describe the blocker in the log, and leave `next_action` pointing at the fix.
   Do NOT mark the sub-task done or advance the phase.

## Sizing a sub-task (examples of "one bounded slice")

- Phase 2 Step 5 split: (a) add `/v1/messages` non-streaming route only; (b) add
  SSE streaming for `/v1/chat/completions`; (c) add SSE for `/v1/messages`. Each
  is its own iteration.
- Phase 2 Step 6 split: (a) `openai.rs` provider impl + factory; (b)
  `anthropic.rs` provider impl + the `convert/` module; (c) tests for each.
- A DAO: one table's CRUD + 1-3 unit tests, in one iteration.
- A live-config adapter: one app's `detect`/`read_live`/`write_live` + dispatch
  + 1 test, in one iteration.

## Porting faithfulness (non-negotiable)

The Rust proxy must **match** `src/gateway/` behavior, not invent new behavior.
Preserve exactly (see `aag-port-to-rust` for detail):
- Canonical OpenAI format (struct-for-struct).
- Retriable classification (5xx/429/network=retriable; 4xx=not) -> drives
  failover.
- Circuit-breaker states + lazy open->half-open transition.
- One-shot streaming failover (first-chunk probe).
- Auto-route fallback when no alias matches.
- Snapshot-at-start (Rust Router is immutable per run).
- Auth (optional Bearer/x-api-key, empty=disabled).
- Atomic writes + backup-before-overwrite for live config.

## Verification commands (full gate)

```
npm run typecheck          # repo root
npm test                   # repo root, 8 e2e tests
cd src-tauri && cargo check
cd src-tauri && cargo test   # if Rust changed
cd ui && npx tsc --noEmit     # if UI changed
cd ui && npx vite build       # if UI changed or first checkout
```
Tauri gotcha: `ui/dist/` must exist before `cargo check` (the
`generate_context!` proc macro panics otherwise). Run `npx vite build` in `ui/`
once on a fresh checkout.

## Log entry format

Keep entries tight and factual. Example:

```
### 2026-07-25 (session N - Phase 2 Step 6a)
- Implemented `proxy/providers/openai.rs`: OpenAIProvider impl Provider, reqwest
  POST to `<base>/chat/completions`, retriable on 5xx/429. Registered in
  build_provider(). 2 unit tests (success + forced 503 retriable).
- Verification: cargo check clean, cargo test 6/6, npm typecheck/test green
  (src/ untouched).
- Next: Phase 2 Step 6b - anthropic.rs + proxy/convert/ module.
```

## When blocked

- Set `Status: blocked` at the top of `PROGRESS.md`.
- In the log, name the blocker precisely (command, error, file:line) and what
  was attempted.
- Leave `next_action` pointing at the unblocking fix, not the next feature.
- Do not silently skip verification or mark incomplete work done.
