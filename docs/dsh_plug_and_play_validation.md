# DSH plug-and-play validation

Date: 2026-08-30. Status: READY_FOR_REVIEW for the local preview.
Baseline: master at 1fa8e287b9d5eb9926948b0c7bc321de5659656d plus the existing
uncommitted DSH/marketplace work and this optimization slice. No commit, push,
global installation or live Claude/DSH configuration change was performed.

## Fresh results

| Check | Result | Evidence boundary |
| --- | --- | --- |
| DSH backend targeted suite | 53 passed, 3 explicit opt-ins ignored | Includes 18 discovery/setup and two preference regressions |
| Final proxy targeted suite | 62 passed, 1 paid canary ignored | Real loopback HTTP and deterministic streaming/dispatch fixtures |
| Full Rust serial suite | 215 passed, 0 failed, 5 ignored; 12.92 s | Current integrated source with disposable configuration |
| Actual DSH acceptance | 2/2 passed, 15.00 s | Both manual and automatic setup paths against actual pinned packages |
| Native Tauri acceptance with auto setup | 15/15 passed | Real WebView2 and IPC, synthetic provider, isolated app/profile |
| Source browser smoke | 101/101 passed | Previous 94 plus seven new onboarding cases |
| Built-demo smoke | 101/101 passed | Fresh demo bundle and independent owned browser |
| Native UI build/typecheck | Passed | Current frontend assets |
| Node typecheck and npm test | Passed; 28 regression tests | Existing independent Node runtime |
| Desktop runner tests | 7/7 passed | Existing development/build configuration |
| DSH transport and capability tests | 2/2 passed | Real owned transport and exact command permission coverage |
| cargo check --locked --offline | Passed | Current Rust source |
| Native executable build | Passed | Locked, offline Windows build after full Rust tests |
| Release preflight | Passed | Preview version/assets, not installer/signing acceptance |

The five normal-suite opt-ins are the paid upstream canary, manual real DSH
acceptance, automatic real DSH acceptance, public pnpm acceptance and public
catalog refresh. The two actual DSH paths were explicitly rerun in this slice.
The paid canary, package install and catalog network refresh were not rerun
or relabeled as fresh evidence.

## Discovery/setup scenarios

The tests cover configured/deployment/PATH/npm/pnpm locations, fixed scan limits,
invalid or oversized manifests, unsupported versions, package/bin identity,
entry containment, non-native files named node, implicit test-runtime exclusion,
and passive reads with no managed-directory creation.

Setup tests cover current provider/model precedence, ambiguous choices,
unavailable current sources, preservation of existing settings and profile
bytes, stale/expired/consumed plans, changed runtime/source metadata,
single-use apply, interrupted transaction markers and rollback after the first
file promotion. No test silently falls back to another provider family.

The native UI test uses AAG_DSH_TEST_RUNTIME_DIR to expose the pinned test
installation and creates a synthetic provider inside its disposable AAG database.
It clicks Auto setup, verifies confirmation is required, applies the detected
paths/current model, and verifies DSH remains stopped and no credential appears
in its snapshot. The existing marketplace permission checks still pass.

## Forwarding and dispatch scenarios

[protocol_tests.rs](../src-tauri/src/proxy/providers/protocol_tests.rs) and the
router/converter/parser tests cover:

- exact native Anthropic signed/redacted blocks and usage/cache extensions;
- OpenAI reasoning aliases, provider delta fields and response/choice/usage
  extensions, including both aliases in one payload;
- Chinese/emoji split across individual network bytes and multi-line SSE events;
- invalid UTF-8, oversized/truncated events and body read errors;
- no failover for HTTP 400/401/403/422 versus retry for 429/503;
- ordered Responses completion, with no success marker after a terminal error;
- correct thinking/text/tool block indexes;
- explicit routes unchanged and stable same-model automatic priority;
- cancellation dropping the owned upstream response;
- repeated late failures opening the breaker without a false winner;
- success/winner bookkeeping only after a real terminal and clean drain;
- neutral cancellation releasing its own half-open permit;
- stale cancellation unable to release a newer probe generation.

These are local protocol fixtures. They do not establish availability, latency,
billing, or compatibility of every real provider/model.

## Actual package-runtime acceptance

The test used Node 24.18.0 and DSH 0.1.1-rc.2 from the previously prepared
pinned dependency directory. It ran the production service, not a substitute
runtime. The automatic path verified canonical Node/DSH/pnpm paths, passive
discovery/planning, current-source recommendation, managed profile initialization,
single-use plans and an actual DSH session calling a local Anthropic fixture.

Bearer authentication, the chosen model, response persistence, credential
exclusion, WebUI JSON settings edits and restart were verified through the
existing actual-runtime assertions. All test state and listeners were owned
and cleaned up.

An initial automatic-path run timed out at the unchanged 45-second readiness
deadline while the subsequent manual path passed. No specific cause was
confirmed. An auto-only diagnostic rerun passed in 6.87 seconds with empty stderr;
an independent repeat of both paths then passed in 15.00 seconds. The timeout
was not increased or described as a diagnosed/fixed defect.

## Reproduce

Use the complete pinned runtime described in
[dsh_management.md](dsh_management.md). For the actual-runtime suite, set only
test-process variables and leave AAG_TEST_CCSWITCH_DB unset:

~~~text
AAG_DSH_TEST_RUNTIME_DIR=<pinned runtime>/node_modules
AAG_DSH_TEST_NODE=<absolute Node executable>
AAG_CONFIG_DIR=<disposable AAG configuration directory>
AAG_HOME_DIR=<disposable home directory>

cargo test --locked --offline --lib services::dsh::acceptance_tests:: -- --ignored --test-threads=1
~~~

For native UI acceptance, finish the build before running the executable and
close other AAG instances:

~~~text
cargo build --locked --offline --bin ai-agent-gateway
AAG_DSH_TEST_RUNTIME_DIR=<pinned runtime>/node_modules
npm run test:dsh_market_native
~~~

With the runtime variable present, the native runner includes the four
automatic-setup checks in addition to its eleven window/security checks.

## Limits and privacy

Node discovery is static evidence, not a verified executable identity or
version. Runtime metadata compatibility does not prove successful startup.
Read-only discovery performs no process execution, downloads or HTTP probes;
filesystem calls remain subject to OS-mounted storage behavior.

No live provider credentials were read from native client settings. No private
provider was called. Native UI and browser tests used synthetic source records,
while the DSH runtime itself was real. Screenshots are visual evidence, not
proof of a real upstream's model behavior.

Full Responses encrypted-reasoning/tool replay, arbitrary runtime installation,
cross-platform native execution, signing/upgrade acceptance and sustained-load
performance remain outside this slice.


Final hygiene: 95 changed/new files were checked for targeted credential/private-
path patterns; zero finding files. Documentation links and Git whitespace checks
passed. The native acceptance left zero AAG processes. Generated permissions,
test dependency installations and screenshots remain ignored. No publication
or long-term memory update was performed.
