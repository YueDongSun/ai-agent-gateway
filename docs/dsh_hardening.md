# DSH safety hardening

Date: 2026-08-31. Source baseline: `a48b927dfd47936a913852fb99214e1ed471c5c3`
plus the local changes described here. These changes strengthen the existing
single managed DSH instance; they do not install DSH, enable a plugin, start a
real runtime, change native agent configuration, or introduce unattended jobs.

Status: **validated locally for the scoped deterministic DSH gate**. Repository
integration and production acceptance remain separate gates. All tests below
use disposable files, synthetic credentials and owned local subprocesses.

## Reproduced failures

The regression tests were first run against the unmodified implementations:

| Boundary | Observed baseline failure | Repair |
| --- | --- | --- |
| HTTP authentication | An unauthenticated `POST` with `Expect: 100-continue` reached a registered `checkContinue` handler and returned HTTP 200. | Apply Host, Origin, target and authentication checks before Node's alternate HTTP event handlers. |
| Startup ownership | Aborting the asynchronous startup task left its child, instance lock and public state in `starting`. | A generation-bound startup guard reaps the owned process and releases the lock when the future is dropped. |
| Runtime selection | Any JavaScript file below a package with the expected name/version passed execution validation, even if it was not the declared executable. | Require the canonical selected path to match the package's declared `dsh` or `pnpm` bin entry. |
| Bundle enablement | Enabling an already installed package accepted a bundle patch outside that package. | Reuse package-local canonical patch validation for both installation and enablement. |

The initial transport run failed one of two test cases. The initial Rust
lifecycle subset had 14 passing tests and three failing new regressions,
corresponding exactly to startup cancellation, runtime selection and bundle
enablement above. Those failures remain the diagnostic baseline, not successful
verification evidence.

## Current contracts

### Authenticated transport

The preload protects the configured managed HTTP listener's `request`,
`upgrade`, `checkContinue`, `checkExpectation` and `connect` events. Expect
handlers preserve their normal behavior only after authorization. HTTP CONNECT
is refused even with a valid token because the WebUI is not a tunnel proxy.
WebSocket upgrades reject foreign absolute request targets as well as foreign
Host and Origin values. Existing bootstrap-cookie and bearer flows remain
compatible; neither token nor raw child output is added to diagnostics.

This is an HTTP transport boundary, not an operating-system or plugin sandbox.
Additional listeners opened by a plugin are still outside the managed listener.

### Startup and cancellation

Dropping a startup future while its own runtime generation is still `starting`
removes the child, control URL, generation and OS-backed instance lock. The
snapshot becomes `failed` with `startup_cancelled`; its PID and URL are empty.
The guard cannot stop or relabel a replacement generation, and successful
`web_ready` instances retain their normal observer and explicit Stop behavior.

An already cancelled package operation fails with `cancelled` before attempting
to spawn its executable. Running operations retain bounded execution and the
existing descendant cleanup mechanisms.

### Runtime and plugin identity

Runtime execution uses the same safe relative JavaScript entry policy as
passive discovery. The package name and pinned version must match, its declared
entry must remain inside that package, and the selected canonical path must be
that entry. A matching package directory is not permission to run arbitrary
sibling scripts. This metadata check is not a binary-signature or source-trust
certificate.

The managed `node_modules` root cannot be a symbolic link or Windows junction.
Package-manager links below that root remain permitted only when the resolved
package stays within that root. Its `package.json` must stay inside the resolved
package and declare the requested name. A bundle patch must resolve to a real
file inside its package; malformed or escaping paths fail closed.

An enable preview binds the installed manifest and bundle patch bytes in
addition to the existing settings/profile/lockfile digest. Changing patch
content invalidates that single-use preview. Plugin execution checks the digest
again after acquiring the cross-process instance lock and before any package
execution, then again before promotion. Existing backup, rollback and crash
marker behavior is retained.

These checks do not hash an entire dependency tree or prevent a malicious
same-user process from modifying files after validation. Community plugin
execution still requires trust review. No native credential file is adopted.

## Verification

Environment: Windows x64, Node `v24.18.0`, Rust `1.97.1`. The following commands
ran against these local changes:

```text
node --test scripts/test_dsh_transport.mjs
cargo test --locked --offline --manifest-path src-tauri/Cargo.toml --lib services::dsh -- --test-threads=1
git -c core.safecrlf=false diff --check -- src-tauri/src/services/dsh src-tauri/resources/dsh_preload.mjs scripts/test_dsh_transport.mjs docs/dsh_hardening.md
```

- Transport: 2 passed, zero failed. Real loopback requests cover ordinary
  HTTP, Expect event paths, CONNECT refusal, authenticated WebSockets, hostile
  origins/targets, bootstrap cookies and manager/market command scope.
- Scoped Rust: 60 passed, zero failed, four explicitly ignored live/package
  acceptance tests; test execution took 7.02 seconds. This includes existing
  discovery, setup, upstream redaction, model preservation, transaction rollback
  and generation-race tests.
- New Rust boundary coverage includes a real Windows directory junction,
  package identity mismatch, stale patch contents, undeclared runtime scripts,
  pre-spawn cancellation and aborted startup cleanup. The cancelled PID was
  independently probed with signal zero and observed absent; the instance lock
  could then be reacquired.
- Changed-file whitespace validation passed. The repository-specific
  `scripts/inspect_git_release.py` helper was unavailable; native Git diff/status
  checks and a changed-file private-data review were used instead.

Not performed in this slice: pinned real DSH or pnpm acceptance, public catalog
refresh, live provider requests, installed native WebView interaction, signed
packaging, installation/upgrade, Linux/macOS execution or publication. The
normal DSH suite intentionally does not invoke those opt-in external tests.

## Implementation locations

- [Transport guard](../src-tauri/resources/dsh_preload.mjs) and
  [owned transport tests](../scripts/test_dsh_transport.mjs).
- [Runtime lifecycle](../src-tauri/src/services/dsh/runtime.rs) and
  [shared discovery entry policy](../src-tauri/src/services/dsh/discovery.rs).
- [Package and bundle validation](../src-tauri/src/services/dsh/catalog.rs) and
  [preview-bound operations](../src-tauri/src/services/dsh/plugins.rs).
- [Lifecycle and safety regressions](../src-tauri/src/services/dsh/tests.rs).

See [managed DSH integration](dsh_management.md) for the retained installation,
credential, native-window, backup and crash-recovery boundaries.
