# Workflow Management

The desktop workflow registry is an AAG-owned read model. It combines a bundled
capability catalog with selected metadata from the existing MCP registry owner.
Catalog presence means that AAG knows how to describe a capability. It does not
prove installation, configuration, access, or successful invocation.

## Manager commands

The trusted main window can call five commands. Each response uses
`schema_version: 1` and snake_case fields.

| Command | Result | Effect |
| --- | --- | --- |
| `workflow_list_capabilities` | Capability list | Reads bundled descriptors and MCP summaries. |
| `workflow_get_health` | Health snapshot | Reads per-stage observations. |
| `workflow_route_preview(task)` | Route preview | Calls the authoritative Python router in policy-only, read-only mode. |
| `workflow_probe_capability(capability_id)` | Health snapshot | Explicit fixed `--version` probe for one of four listed local CLIs. |
| `workflow_evaluate_route(task)` | Route preview with optional advice | Optional local Strands task observations and explicitly confirmed Jev evaluation; never dispatches work. |

Each capability has `id`, `name`, `kind`, `group`, `owner`, `access_method`,
`host_scope`, `source_ref`, `mcp_server_id`, `summary`, `probe_supported`, and
`observations`. An observation has `stage`, `status`, `observed_at`,
`expires_at`, `evidence_source`, and `summary`. Static and host-owned entries
start at `unknown` and require a host observation. MCP registry definitions get
one `configured` observation and an independent `callable: unknown` observation.
A successful local CLI probe establishes only `installed`. It expires after
five minutes; the previous observation then reads as `stale`.

## Private configuration

The AAG config root may contain a private `workflow_config.json` with keys
`python_path`, `router_path`, `project_dir`, `probe_paths`, `jev_enabled`, and
optional `jev_model`.
The private file is parsed once per request. Semantic validation belongs to the
invoked feature: routing validates interpreter/router/project paths, a probe
validates only its selected path, and enabled advice validates its own settings
when invoked. Invalid unused probe/advice/Jev settings do not disable routing or
an unrelated probe. Absent probe paths remain unknown; selected invalid paths
return an invalid-configuration error. All used paths are absolute and validated. `project_dir` is an existing project
directory. `probe_paths` accepts only `pi_cli`, `cline_cli`, `kimi_cli`, and
`minimax_cli`; each value is a local executable path. The optional
`jev_confidence_floor` is a finite number from 0 to 1; its default is 0.95.
The file must satisfy
the existing AAG private-file policy. Missing configuration produces an
unavailable route preview. No host-specific path is bundled in source. Defaults
keep Jev disabled; native Luna qualification cannot be enabled by this file.

Routing invokes the configured interpreter with the configured router file,
`-E -s -B -X utf8` and `--policy-only --sandbox read-only`, a fixed working directory, bounded stdin,
and a deadline. The task is never inserted into a shell command. AAG validates
the router's safe JSON output and does not replace its scoring or model policy.
The optional Jev adapter defaults off and does not read `TYPESAFE_API_KEY` or
use the network while disabled. When enabled, it uses the fixed official HTTPS
SystemOne endpoint, a bounded task with recognizable private forms removed, policy-eligible model choices, a
response limit and timeout, and a configured confidence floor (0.95 by default).
Its result is advisory and never changes the authoritative route or final gate.
The native Codex app-server advice path remains unavailable until account, model,
session, and no-executable-tool restrictions can be proven on the installed
host. An unusable Jev answer therefore leaves the deterministic route intact
until the independent native fallback contract is qualified. `jev_evidence`
preserves the original Jev result separately from `model_advice` when fallback
is requested. Confidence is provider metadata; the selected-option probability
is a different quantity and never substitutes for confidence.

Jev receives a narrow policy projection containing a fixed schema version,
allowlisted difficulty/reasoning identifiers, and policy-eligible model names.
Router summaries, signals, arbitrary identifiers, and local/model observations
are omitted rather than forwarded as redacted policy text.

Every external evaluation also requires `external_advice_confirmed: true` from
the manager action. The UI defaults it off and names TypeSafe and separate
provider usage. Configuration enablement alone is insufficient. Recognizable
credential assignments are rejected before credential lookup or HTTP access.
Pattern removal does not establish that arbitrary task prose is nonconfidential;
the user reviews the concrete task before allowing its external transmission.

Codex 0.159 was checked through its generated installed schemas and supported
read-only discovery methods. ChatGPT authentication and GPT-6 Luna model listing
were observed separately. `environments: []`, `dynamicTools: []`, a read-only
sandbox, and `outputSchema` do not attest to an empty complete executable-tool
registry. The adapter therefore reports `tool_restriction_unverified` and does
not start an inference turn. A configuration Boolean cannot override this guard.

## Local read facade

On Windows, the native app owns one named pipe. Its protected access control
list grants access to the current process user, and the pipe rejects remote
clients. The descriptor is an AAG-private `runtime/workflow_ipc.json` containing
`schema_version`, `instance_id`, `pipe_name`, and `process_id`. The pipe name is
`\\.\pipe\aag_workflow_<instance_id>` where `instance_id` is 32 lowercase hex
characters. A client must validate the descriptor, process identity, and local
pipe prefix before connecting. A stale instance is rejected by the server.

One connection carries one UTF-8 JSON request followed by a newline, at most
64 KiB. The request is:

```json
{"schema_version":1,"instance_id":"<descriptor value>","request_id":"r1","method":"get_health","params":{}}
```

The only methods are `list_capabilities`, `get_health`, and `route_preview`.
The first two accept optional decimal `cursor` and integer `limit` (1–16,
default 8), returning `next_cursor` when another page remains. Follow pages
from one current discovery cycle; restart from the first page if the registry
changes. Page sizes shrink when necessary to stay below the frame bound.
Native manager commands return the full snapshot. User-authored MCP descriptions
are omitted from this read model; summaries use fixed product text. Names and
IDs remain visible to the connected client and must not contain credentials or
private topology.
Private arguments, headers, environment values, and endpoints stay excluded.
`route_preview` requires `params.task`, a nonempty string of at most 8 KiB.
Successful responses contain matching `schema_version`, `instance_id`, and
`request_id`, `ok: true`, and `result` with the corresponding manager DTO.
Failures contain `ok: false` and `error: {code, message}`. Reads and writes
have bounded deadlines. Dispatch has a ten-second reply deadline and at most
four active permits. A synchronous database read cannot be forcibly cancelled;
its permit stays held until it exits, bounding outstanding work after timeout.
Shutdown stops accepting clients and aborts tracked socket tasks. Probe,
inference advice, configuration, mutation, and
arbitrary shell/path/URL operations are absent from this protocol. The
descriptor is removed during normal owner shutdown if its bytes still match
the owner instance. On other operating systems the facade remains unavailable.

The existing static stdio MCP companion stays independent of this live facade.
The Node client must use this facade rather than querying AAG's private
SQLite database or importing native service internals.

## Configure an MCP client

The separate workflow facade uses the pinned official JavaScript MCP SDK.
Start it with a verified absolute Node executable and the absolute
`scripts/workflow/server.mjs` path. Its default discovery file is under AAG's
runtime directory; a trusted `--descriptor-file` option supports portable tests.
That path is never accepted as a tool input. The three exposed tools are
`workflow_list_capabilities`, `workflow_route_preview`, and `workflow_get_health`.
They report a stable `workflow:owner_unavailable` error when the native owner is
absent; successful initialization is not a health or deployment claim.

Use `npm run test:workflow` for SDK/pipe-client contract tests. The existing
feature-aware MCP verification gate includes this suite. Each client retains
its own supported configuration and authentication owner. AAG's reviewed Claude
sync remains the registration write interface; Codex reviews remain preview-only.

## Reference revisions and dependencies

No third-party application framework or model-routing service replaces AAG.
The following public revisions were checked as focused implementation references:

- [Vercel form router](https://github.com/vercel-labs/jev-ai-sdk-form-router/tree/27ea469d15b5b26b922d9faacfdcffc13bf024bb): typed choice, confidence boundary, bounded calls, and independent fallback pattern.
- [Official TypeSafe JavaScript SDK](https://github.com/typesafe-ai/typesafe-sdk-js/tree/66880ccded6cb642dc1809620c2b108c33730214): request/response contract; the inspected package version is 0.6.0. The Rust adapter uses existing `reqwest`, without installing or running a second SDK.
- [Vercel eve](https://github.com/vercel/eve/tree/682c7a68f22fbae52e3102718534ece8fc36f9a1): allowlisted model-selection pattern; no eve runtime is installed.
- [Decision-model benchmark](https://github.com/nibzard/decision-model-benchmark/tree/eabd88b04706bcc9b7769213d1ea644c09d8a7f8): evaluation structure only; published results are not workflow acceptance.
- The facade dependency is exactly `@modelcontextprotocol/sdk` 1.31.0 in the root npm manifest and lockfile. Both root and UI lockfiles remain authoritative; the existing static companion gains no dependency or behavior change.

Model namespaces remain provider-specific. Native `gpt-6-luna`, Gateway
`openai/...` identifiers, Jev versions, and existing AAG aliases are not
interchangeable. See [native authentication](https://learn.chatgpt.com/docs/auth),
[app-server contracts](https://learn.chatgpt.com/docs/app-server), and
[the official Jev quickstart](https://docs.typesafe.ai/introduction/quickstart).

## Evidence boundary

Automated tests cover parsing, identity and mutation rejection, bounded input,
current-user connection to the restricted Windows pipe, Jev choice validation,
disabled zero-network behavior, and unavailable policy behavior. A successful local build does not prove a
signed installation, browser behavior, external provider readiness, or that a
model inference ran. Native pipe and host-owner acceptance require separate
observation on the installed desktop runtime.

## Shared local Strands workflow advice

The `strands_workflow_advice` skill owns one portable Python adviser used by the
global Codex workflow and the trusted AAG evaluation action. It classifies the
requested deliverable as `artifact_change`, `artifact_assessment`, or
`other_or_unclear`. The result is observation-only and does not change route
score, model responsibility, delegation, sandbox, execution scope, or approval.

AAG configuration supports `strands_advice_enabled` (default false),
`strands_advice_path`, and `strands_advice_config_path`. These are host-private
absolute paths. The shared adviser owns and validates its separate configuration
contract; its configuration supplies a numeric loopback endpoint, expected
checkpoint, logical deployment identity, runtime directory, confidence floor,
and bounded timing. The shipped defaults remain disabled.

`workflow_evaluate_route` first obtains the deterministic policy, then invokes
the adviser with fixed argv and bounded stdin through the existing process
owner. Its optional `task_advice` is distinct from native model selection and
Jev evidence. Jev receives only the narrow deterministic policy projection, so a
separately confirmed external evaluation does not forward local observations.
`workflow_route_preview` and the three-method read facade perform no local
model inference. The skill supplies the explicit global advisory entry point;
no inference tool is registered into that read facade.

The adviser performs one health check and at most one inference request, with
no redirects, retries, ambient proxy credentials, or raw task logging. One
shared permit serializes the integrated callers. An interrupted submitted
request leaves a private uncertainty marker; later callers report
`recovery_required` until the deployment owner independently establishes
completion or recovery. Health alone, endpoint changes, and disabling/re-enabling
do not clear that marker. A model failure leaves the deterministic route usable
and never enables an external provider automatically.

For three choices, confidence is `(3 * max_probability - 1) / 2`. A floor of
0.95 implies a top probability of approximately 0.9667. This concentration
filter is not calibrated workflow accuracy or execution authority. Lower-score
valid classifications remain visible as `below_confidence_floor`.

Configuration must be enabled only after the matching AAG application supports
these fields; older applications reject unknown keys. Install and verify the
shared skill first, test this source adapter, then update the native owner and
enable its optional fields. Global skill configuration is independent of this
application rollout. Roll back by disabling local advice; preserve uncertain
request state until recovery is separately established.

Use the shared skill tests for transport and response-contract coverage, the
Rust workflow suite for policy binding, and the explicit ignored
`local_strands_integration_preserves_authoritative_policy` test for a real
producer/consumer check. That integration test requires
`AAG_STRANDS_TEST_CONFIG` to identify a reviewed disposable manager config; it
does not run by default. Source tests, real inference, native owner deployment,
and optional external Jev access remain separate evidence.

## Child environment and cleanup

Router, adviser, and fixed version subprocesses start with an empty environment.
The allowlist retains `PATH` for runtime discovery, `HOME` for user-home
resolution, and `TMPDIR`/`TMP`/`TEMP` for temporary-file placement. Windows also
retains `SYSTEMROOT`/`WINDIR` for OS runtime loading, `COMSPEC`/`PATHEXT` for runtime
launchers, and `USERPROFILE`/`HOMEDRIVE`/`HOMEPATH` plus
`APPDATA`/`LOCALAPPDATA`/`PROGRAMDATA`/`PROGRAMFILES`/`PROGRAMFILES(X86)` for standard
home and runtime discovery. Other ambient values are excluded, including provider
credentials, proxy settings, and all `PYTHON*` customization. This is environment
minimization, not a filesystem sandbox. Python uses `-E -s` to ignore environment
configuration and user site packages while retaining reviewed sibling imports.

Owned process groups on Unix and Job Objects on Windows are terminated on
timeout, output overflow, and normal capture completion. Cleanup covers members
of the owned group or job, not arbitrary descendants. Windows assigns the child
to its Job Object immediately after spawn, not atomically with process creation;
a descendant created before assignment may remain outside the job. This is not
hostile-code containment. Direct children are explicitly reaped with a short
bounded wait; delayed exits remain owned by a reserved reaper thread. Reaper
setup happens before process launch; setup failure returns a fixed unavailable
error without starting a child. Capture never waits synchronously for delayed
exits. The reaper retries interrupted waits, recognizes
Unix already-reaped child identity, and retains the child on a parked owner after
an unexpected permanent wait error instead of retrying forever or abandoning
ownership. Windows termination continues to use retained child
handles and the assigned Job Object, without PID-based control of unknown jobs.
