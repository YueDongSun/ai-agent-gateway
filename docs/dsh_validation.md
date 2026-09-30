# DSH validation record

Date: 2026-08-30. Source baseline: master at
1fa8e287b9d5eb9926948b0c7bc321de5659656d plus the uncommitted DSH implementation.
No commit, push, installer deployment or live credential migration was performed.

Environment: Windows x64; Node 24.18.0; Rust 1.97.1; DSH and its pi-ai adapter
0.1.1-rc.2; pi-ai 0.82.1; pnpm 10.34.5.

Status: READY_FOR_REVIEW for this local preview. Not published or qualified for production.

## Current verification results

| Command / path | Result | Evidence boundary |
| --- | --- | --- |
| npm run typecheck | Passed | Existing Node surface |
| npm test | 28 passed | Isolated Node regression tests |
| npm run test:desktop_runner | 7 passed | Desktop dev/build configuration |
| npm run ui:build | Passed | Latest native frontend TypeScript and production bundle |
| npm run ui:build_demo | Passed | Latest explicit browser demo bundle |
| npm run ui:smoke | 74/74 passed | Owned browser; synthetic IPC; original 54 checks plus 20 DSH checks |
| npm run ui:smoke_demo | 74/74 passed | Same coverage against the built demo; zero uncaught exceptions |
| npm run test:dsh_transport | 2/2 passed | Real owned HTTP/WebSocket requests and explicit native command coverage |
| cargo check --locked --offline | Passed | Native Rust compilation |
| cargo test --locked --offline --lib -- --test-threads=1 | 165 passed, 0 failed, 3 ignored; 14.82 seconds | Isolated deterministic Rust suite |
| Explicit pinned DSH acceptance | 1/1 passed, 12.48 seconds | Actual DSH runtime and local Anthropic fixture |
| Explicit pinned pnpm acceptance | 1/1 passed, 21.20 seconds | Actual package installation/removal; no plugin execution |
| npm run release:verify | Passed | Existing preview asset/version preflight; not installer acceptance |

The final core suite intentionally leaves the paid/live provider canary and
two explicit integration tests ignored in its normal invocation. The two DSH
integration tests were run separately as recorded above. No required Node process
test silently passes when Node is unavailable.

## Real DSH acceptance

[acceptance_tests.rs](../src-tauri/src/services/dsh/acceptance_tests.rs) exercised
the production Dsh_service against the installed published DSH packages:

1. Initialize and save a disposable managed profile, then start through the
   actual service with its generated settings overlay and transport preload.
2. Confirm the process remains web_ready after initial page readiness.
3. Refuse unauthenticated API traffic; exchange the private bootstrap token for
   a session cookie; read actual DSH HTML and theme bootstrap.
4. Query the real pluginInventory/list endpoint and observe active host inventory
   and model-adapter entries.
5. Create a real DSH session with the selected native Claude model. Send a prompt
   through DSH to a local mock Anthropic HTTP server.
6. Verify the exact Bearer header, absence of x-api-key, selected model and
   fixture prompt. Verify the completed reply enters real session history.
7. Confirm the fixture credential is absent from the public snapshot and
   persisted model settings.
8. Change the theme through DSH's real settings API, read back valid JSON, stop
   the runtime and restart it successfully.
9. Stop and verify the listener has closed. A subsequent process check found no
   owned DSH Node process remaining.

This proves the native Bearer integration against a local protocol fixture.
It does not prove availability, authorization, quotas, billing, or performance
of Anthropic or a user's real upstream service.

## Real package-manager acceptance

[package_acceptance_tests.rs](../src-tauri/src/services/dsh/package_acceptance_tests.rs)
used the real pnpm 10.34.5 JavaScript entry. It installed the public package
@steven-wu/dsh-cost-meter@0.1.0-rc.3, disabled its bundle, enabled it again, and
removed it through Dsh_service previews and operations.

The test checked real package manifests, generated lockfile, configured bundle
membership and terminal receipts. Runtime plugin phase remained unknown. DSH
was not started with this plugin and its code was not executed. Package scripts
and pnpm hooks remained disabled.

Intentional dependency changes explicitly use --no-frozen-lockfile even in CI,
while retaining the previous manifest and lockfile for failure recovery. A
copied old lockfile is not incorrectly treated as immutable during a reviewed
install/remove operation.

## Reproducing the explicit checks

Prepare a disposable Windows x64 runtime directory using
[scripts/dsh_test_runtime_windows.json](../scripts/dsh_test_runtime_windows.json)
as its package.json. Install with scripts disabled; see
[dsh_management.md](dsh_management.md). The manifest includes required published
peer dependencies and Windows prebuilt native packages. It is not a universal
cross-platform dependency bundle.

Set these variables only in the test process:

~~~text
AAG_DSH_TEST_RUNTIME_DIR=<runtime_directory>/node_modules
AAG_DSH_TEST_NODE=<absolute Node executable>
AAG_CONFIG_DIR=<disposable AAG configuration directory>
AAG_HOME_DIR=<disposable home directory>
~~~

Keep AAG_TEST_CCSWITCH_DB unset. From src-tauri:

~~~text
cargo test --locked --offline --lib services::dsh::acceptance_tests::pinned_dsh_web_native_claude -- --ignored --exact --test-threads=1
cargo test --locked --offline --lib services::dsh::package_acceptance_tests::pinned_pnpm_plugin_lifecycle -- --ignored --exact --test-threads=1
~~~

Invoked acceptance tests fail if their explicit prerequisites are missing. They
do not substitute a mocked runtime or silently skip.

## Findings retained from bring-up

- The researched GitHub alpha version was not published on npm. Tests bind to
  the available 0.1.1-rc.2 release instead.
- Default npm peer resolution stalled; legacy peer resolution completed but
  omitted required peer dependencies. Explicit peer pins and two Windows
  prebuilt native packages were then materialized, with install scripts still
  disabled.
- The actual DSH runtime requires --expose-internals. The launcher now supplies
  this only to its child process.
- The Windows exclusive instance lock originally sat inside DSH_HOME.
  DSH's file watcher attempted to read it and crashed with EBUSY after initial
  page readiness. The same strong OS lock now lives outside the watched root.
- Original WebUI writes could serialize a .yaml settings document as YAML.
  The generated composition overlay now selects the supported .json settings
  provider path; actual API writes and restart were verified.
- A combined browser lifecycle check exceeded the CDP per-evaluation deadline;
  it was split into independent assertions without increasing the timeout or
  weakening the lifecycle criteria.
- The first rollback fault fixture did not obstruct a rename on Windows.
  A nonempty obstruction directory provides the intended cross-platform failure;
  the restoration and rollback-failure assertions remain unchanged.

The failed setup and integration attempts are not presented as successful runs.
The final results above refer to the repaired implementation and explicit
dependency variant.

## Review and remaining limits

A source-level audit covered changed/new files for private-key blocks, common
credential formats and machine-specific private paths, reporting filenames only.
The repository-specific inspect_git_release.py helper was unavailable; native
Git status/diff checks, ignore checks, targeted pattern review and the
project's own release preflight were used instead. This is not a full security
audit of DSH or community dependencies.

Browser screenshots were visually inspected after automated layout checks.
This is demo UI evidence, not native WebView2 interaction evidence.

Not performed: real paid upstream calls; community plugin runtime qualification;
native desktop window interaction automation; Windows installation/upgrade;
macOS/Linux execution; signing or publication. Runtime binaries are not bundled
into the AAG installer. Per-plugin runtime aggregation, full MCP/Skill editing,
automatic mid-promotion crash recovery and backup garbage collection remain
outside this preview.
