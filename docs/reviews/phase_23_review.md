# Phase 23 review and closure

## Scope

Reviewed `100938befe7c4fbcb5787ccc5f2f5476913765e2` against its parent, plus the
immediately preceding context-split changes needed to assess its closure claims.
On 2026-08-27, `git ls-remote --heads origin master` returned that same commit.
No pull, commit, push, deployment, or real provider request is part of this work.

## Repairs in this slice

### P1: Route actions retain an obsolete render

Introduced in `522b01c`. The actions memo depended only on gateway running/busy
state while its handlers also captured routes, selection, drag state, app type,
and other page values. With the gateway stopped, the initial empty route list
survived asynchronous loading. Save then compared it with the populated baseline
and deleted every persisted route. Batch deletion and pointer dragging could
also read the initial empty state.

A source-extracted, in-memory reproduction deleted both synthetic baseline
routes. The repair publishes current action callbacks instead of suppressing
dependencies. The route context now has its own `routes_state.tsx` module.

### P2: Pool actions retain initial providers and pin state

Introduced in `77f5164`. The empty-dependency actions memo retained an empty
provider list and a null pin. Enable returned without saving; repeated pin
clicks kept selecting the same provider instead of clearing it.

The source-extracted reproduction recorded zero provider saves and two identical
pin requests. The repair publishes current callbacks and separates the pool
context into `proxy_pool_state.tsx`. Existing optimistic rollback is preserved.

### Verification gap: Historical smoke results are insufficient

The previous suite did not test initial Save with backend readback, selected
route deletion, pin/unpin, or pointer dragging. Its enable check asserted a DOM
change without verifying persistence. It also reused any responding server on
the fixed demo port, without proving that server belonged to this checkout.

The revised smoke keeps the public command, separates test responsibilities, and
adds checks against the synthetic backend. Fresh results are recorded in
`PROGRESS.md`; old "28/28" entries are retained only in the history archive.

## Remaining recommendations

### Resolved: Restore the Rust startup-readiness assertion

The latest commit correctly removes the held-listener Windows conflict and uses
an atomic port-0 bind. However, `serve_returns_actual_bound_port` immediately
sends shutdown and only checks the returned port/IP. The previous `/health`
request is gone, so a successful bind no longer proves the server served HTTP.

Resolved on 2026-08-28: the existing test now uses `serve_detached` with port 0,
requests `/health` using the actual address with finite timeouts, and signals
and awaits shutdown before asserting the response. Its database is in memory.
The full serial Rust suite passes 125 tests. The port-0 strategy is retained;
`HELD_PORTS` was not restored.

### P2: Measure row rendering before closing the performance work

In `100938b`, each memoized provider row receives `onEdit` and `onDelete`
callbacks that are recreated by `AppPage`. They invalidate shallow memo
comparison. Route cards and hop rows also consume entire data contexts, so
reading a subset of fields does not make subscriptions selective.

The repair withdraws unsupported isolation claims. A later performance slice
should stabilize callbacks with correct dependencies or pass selected values at
a meaningful boundary, then compare render counts under a fixed fixture. Do not
reintroduce stale closures to reduce renders. Bundle reduction and per-row
render isolation remain unverified, not completed features.

### Observed gateway-modal timing failure

During harness bring-up, the existing invalid-port check once reported
`no-error-shown; portNow=99999`. Subsequent independent full runs passed,
including the final 37/37 run. The port validation implementation and its
assertion were not weakened or claimed repaired. If this recurs, first trace
modal configuration loading and page refresh completion, then replace fixed
delays with explicit readiness conditions and verify that persisted settings
remain unchanged. The root cause of this transient observation remains open.

## Original UI repair evidence - 2026-08-27

- Node typecheck and 18/18 tests passed.
- UI TypeScript/production build and Rust check passed.
- Rust serial tests: 122 passed; optional real-data work stayed disabled.
- Browser smoke: 37/37 passed, with initial Save tested while the gateway was
  still stopped and backend readback for persisted state.
- Missing-browser startup, protocol-error, disconnect, and asynchronous page
  error probes passed; owned browser/profile/server cleanup was checked.
- All 33 archived phase entries match the reviewed commit after normalizing
  line endings. No phase advancement or deployment is inferred from this.

## Evidence boundary

Source probes establish the stale-capture failure, browser smoke establishes
behavior through the synthetic IPC backend, and Rust/Node suites establish their
own tested contracts. None proves native WebView2 behavior, real upstream
availability, live CLI configuration switching, or deployed delivery. Optional
real-database Rust tests remain disabled during this verification.
