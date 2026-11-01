# Request-bound routing receipts

Status: implementation contract for the R&D integration. Native deployment and
live-provider acceptance remain separate from local fixture verification.

AAG owns routing facts. The R&D consumer may use this versioned HTTP interface;
it does not read AAG's database, private diagnostic stores, credentials, native
session homes, or global last-winner map. The receipt records what the gateway
observed. It cannot attest which proprietary model executed behind an upstream.

## Admission and access

Version 1 is opt-in. A client creates a fresh 16-byte random nonce and a separate
32-byte secret capability before sending inference. The canonical header is:

```text
X-AAG-Receipt: v1.<32 lowercase hexadecimal nonce>.<64 lowercase hexadecimal capability>
```

The nonce is public correlation metadata. The capability is a bearer secret,
kept only by the client and never placed in a URL, log, response header, upstream
request or portable session bundle. AAG retains only the verification MAC.
Knowing the shared gateway credential and a nonce does not authorize retrieval.

Receipt admission requires a nonempty configured gateway credential and an
exact match with it, independently of subscription-passthrough admission. The
edge validates the supported route and unique header, then atomically reserves
bounded space before provider I/O. Invalid request bodies still fail normal
validation before dispatch and may leave a rejected admission observation.
Malformed, duplicated, unsupported or unauthenticated opt-in headers never fall
back to unobserved inference. Reusing a retained nonce is rejected before another
provider call, even with the same capability.

The supported generation surfaces are Chat Completions, Anthropic Messages,
OpenAI Responses and Gemini generate/streamGenerateContent. Model discovery,
token counting and the separate Claude Desktop prefix are not inference receipt
surfaces in v1. Existing requests without the header keep their current behavior.

Messages opt-in requires a captured snapshot without enabled subscription
passthrough support. A mixed snapshot returns HTTP 400 with
`receipt_credential_scope_unsupported` before credential attachment or provider
I/O, leaving a settled receipt with zero attempts. This prevents the gateway
credential required by receipt admission from becoming an upstream relay
credential. Capability discovery advertises
`messages_credential_scope: stored_key_only_snapshot`; discovery is not a lease
on a later configuration. Clients must not retry the rejected request without
receipts automatically. General legacy passthrough behavior is outside this
metadata increment.

`GET /_aag/v1/receipt_capabilities` negotiates the version, listener-instance
identity, supported surfaces, capacity and terminal-retention period without
inference. Authenticated `/v1/models` responses advertise support with
`X-AAG-Receipt-Version: 1`. The consumer verifies the capability response before
opting in; older gateways remain explicitly unsupported.

`GET /_aag/v1/receipts/<nonce>` requires the existing gateway credential plus:

```text
X-AAG-Receipt-Capability: <64 lowercase hexadecimal capability>
```

Missing, expired and wrong-capability records have the same unavailable response.
There is no list endpoint. Browser-origin restrictions and CORS stay unchanged.
An already-enabled authenticated Tailnet publication preserves both checks; this
contract does not enable publication or change Tailnet configuration.

## Ownership and evidence

Each HTTP frontend owns an in-memory bounded registry and random instance ID.
Active rows are retained until their request owner settles. Terminal rows retain
the advertised lifetime and are not evicted early to admit more work. Admission
fails before inference when no slot is available. Retained wire-attempt metadata
is also bounded. Exhausting that metadata budget permanently marks the receipt
`capture_truncated` and incomplete, clears any selected-attempt claim, and leaves
the existing inference/retry policy unchanged. No recurring pruning job is created.

The root record binds nonce, AAG request ID, surface, admitted routing generation,
requested selector, selected attempt, HTTP-body state and lifecycle. Router
selection is captured after affinity and admission. Each actual transport send
gets a distinct request-local ordinal, hop index and subattempt index. Breaker
skips are not transport attempts. Anthropic compatibility retries remain visible
as separate sends within one hop.

An attempt contains only bounded provider/supplier/endpoint references,
association kind, wire protocol, credential mode, final wire model, optional
provider-reported model with its source/state, HTTP status, terminal signal and
fixed failure code. Configured supplier association is not proof of the legal
operator behind a reseller. Unmapped/manual/queue associations remain explicit;
they are never inferred from URL or credential similarity.

The final wire model is observed at the actual body or URL builder. The reported
model is observed before conversion can substitute the requested model. Missing,
rejected and conflicting claims remain distinguishable. Credentials, credential
fingerprints, full URLs, domains, IPs, prompts, response text and tool payloads
are excluded. User-controlled body extras and upstream response headers never
become AAG evidence.

One non-cloneable wire guard owns each attempt. The lightweight request/hop
handle may be cloned; it must be passed explicitly to provider work and captured
by returned streams. No provider instance has a mutable current-request slot.
Streams separately record router commitment, raw protocol terminal evidence,
transport end/failure/drop and HTTP-body lifetime. HTTP 200, first bytes and body
exhaustion do not by themselves establish model completion. Terminal evidence is
captured before yielding its event so terminal-then-drop stays observable.

## Recovery and compatibility

Receipts are metadata, not a replay cache or durable exactly-once execution.
Restart or retention expiry can remove duplicate history. Missing evidence means
unknown and never authorizes inference retry. It cannot recover a lost generated
answer. A client that loses its private capability cannot reconstruct it from
public session context. Gateway token rotation and endpoint replacement can also
make an old receipt unavailable; consumers do not redirect or change providers
to recover it.

The R&D consumer keeps `runtime_profile` as its existing configuration-consumption
record and adds separate `gateway_routing` provenance. It correlates the nonce,
server instance and requested selector, preserves valid evidence on post-send
content errors, and performs at most one inference POST. An older gateway's lack
of advertised support remains unknown routing evidence. No receipt can promote
VibeMux's live attestation or writable-harness qualification.

Roll out the producer first, then the optional consumer. Rollback removes
advertisement/opt-in use; legacy inference remains available. There is no database
or live-home migration, no public export and no automatic credential provisioning.

## Verification boundary

The focused producer command is:

```text
cargo test --locked --offline --lib routing_receipt -- --test-threads=1
```

The 2026-10-26 run passed 27 tests; one optional sibling-consumer fixture was
ignored in the ordinary suite. This covers all four wire protocols, raw versus
canonical model fields, compatibility retries, malformed terminals, stream
drop/EOF, strict access, duplicate nonce rejection and Messages credential scope.

To select the optional actual HTTP integration, provide absolute paths in
`AAG_RD_NODE_EXE` and `AAG_RD_CONSUMER_FIXTURE_SCRIPT`. The latter must name the
Hub's `scripts/rd_aag_contract_probe.mjs`. Then run:

```text
cargo test --locked --offline --lib proxy::routing_receipt_tests::hub_consumer_fixture_reads_aag_routing_receipt_over_actual_http -- --ignored --exact --test-threads=1
```

That fixture passed separately using a disposable Axum listener, a synthetic
upstream, a cleared child environment and bounded process cleanup. It tests the
real Hub adapter/HTTP transport and validates both complete and zero-attempt
incomplete producer receipts. It does not contact a live model or read AAG state.

The broader serial library gate reported 1455 passes, 63 failures and 39 ignored
tests in the same verification stage. Its 430 proxy and 43 proxy-runtime tests
passed; failures were reported in other modules, chiefly native ownership,
storage and process fixtures, with three additional content assertions. This
does not establish a clean-baseline cause for every failure. Do not present the focused receipt results
as a green full-library or native-release gate. Existing compiler warnings also
remain. Deployment, live-model acceptance and the general legacy passthrough
credential contract are outside this increment's verification.

The integration is additive and explicitly opted in: header-absent requests use
the existing provider methods. Supplier references are derived during the
planner's existing endpoint read, with no extra database lookup or new snapshot
failure path. Metadata observation cannot change route order, failover, breaker
settlement, timeouts, wire payloads or conversion acceptance. In particular,
typed OpenAI response decoding remains typed so duplicate known fields keep
their original rejection behavior.

## Acceptance

Use disposable authenticated loopback providers. Check concurrent requests and
hot reload, all four protocols, explicit/pinned/queue/supplier paths, alias versus
wire versus reported model, absent/invalid/conflicting claims, HTTP fallback and
Anthropic subattempts. Exercise first-byte timeout, unread/dropped/truncated/error
streams and terminal-then-drop. Verify duplicate nonce races, capacity, expiry,
restart loss, wrong capability, disabled auth, passthrough-only auth, unsupported
versions/surfaces, forged fields, header non-forwarding and credential exclusion.
Consumer tests must assert one POST after missing or malformed evidence and must
keep classification routing evidence distinct from later VibeMux execution.

The implementation decisions above are approved within the current source task.
Live account/provider/device actions and the outstanding IM-content destination
authorization are unchanged.

## Research basis

OpenTelemetry's [GenAI reference scenarios](https://github.com/open-telemetry/semantic-conventions-genai/blob/20259355b9631e3f032dcc099d1b5a2666818c42/reference/scenarios/litellm/scenario.py)
keep requested and response model fields distinct. The earlier conventions also
explicitly caution that a protocol provider label may differ from the actual
model provider. AAG additionally keeps its routed wire value separate and excludes
content and endpoint data from this interface.

[MCP-Injector](https://arxiv.org/abs/2610.32555v1) reports a 1,350-cell fault-injection
study and substantial quality loss under its sampled MCP fault profile. This
motivates explicit failure/loss fixtures here; the study is not evidence that
AAG's receipts, providers, throughput or recovery behavior are already verified.

## Version 2: snapshot-bound admission

Version 2 adds an explicit precondition for clients that require the observed
routing snapshot to be the snapshot actually admitted for dispatch. It does not
change v1 capabilities, headers, receipt JSON or header-absent inference.

Authenticated `GET /_aag/v2/receipt_capabilities` returns a descriptor from one
live runtime snapshot: `server_instance_id`, positive `generation`, and
`pinned_provider_present`, alongside schema 2, Chat/Messages surfaces and the
existing capacity/retention/credential-scope fields. No-live construction sites
return 503 rather than inventing a routing generation.

The client uses a fresh nonce and private capability, then submits both:

```text
X-AAG-Receipt: v2.<nonce32hex>.<capability64hex>
X-AAG-Route-Precondition: v2.<server-instance32hex>.<canonical-generation>.<0-or-1-pin>
```

The snapshot comparison and dispatch state derive from the same atomic capture.
A reload after capture cannot change that request's router, pin or credential
policy. Captured planner inputs are immutable; operational breaker/affinity/spread
state may still select among eligible attempts, which the receipt records.
A stale instance, generation or pin returns 409 before upstream effects.
Malformed, duplicated, missing or unpaired headers return 400; unavailable snapshot
state returns 503. This version admits only Chat Completions and Messages; other
surfaces keep v1 behavior and reject v2 opt-in. Messages still refuses mixed
subscription-passthrough snapshots. The precondition and opt-in capability are
removed before protocol/provider handling.

`GET /_aag/v2/receipts/<nonce>` requires the same gateway authentication and
private capability. Its closed result is:

```text
{
  schema_version: 2,
  issuer: "aag",
  admission: null | { server_instance_id, generation, pinned_provider_present },
  routing_receipt: <the unchanged version-1 receipt>
}
```

Admission is populated only after a successful snapshot match. A rejected request
can retain a null-admission receipt with zero attempts. A matching proof alone is
not a successful inference result: clients must independently validate nested
request correlation, selected attempt, complete capture, raw protocol termination
and HTTP body settlement. Server instance/nonce/request ID and expected selector
must match before associating this metadata with a caller's result.

Versions share nonce reservation, capacity and retention. A nonce cannot be reused
under another version, and a v1 record cannot be retrieved through v2 or vice
versa. Version-specific capability verification remains private to the registry.
There is no durable inference replay cache or retry permission after lost evidence.

The opt-in VibeMux consumer now keeps a separately versioned snapshot fingerprint
and gateway-observed attribution. It can admit a model call only under explicit
`bound_receipt_v2` startup policy and releases text only after the matching terminal
receipt. This does not attest an upstream proprietary model or qualify native
workers, reviewers, sandboxes, deployment or scientific outcomes.

Producer validation commands:

```text
cargo test --locked --offline --lib routing_admission -- --test-threads=1
cargo test --locked --offline --lib proxy::routing_admission_tests::routing_admission_vibemux_consumer_reads_actual_v2_owner_over_http -- --ignored --exact --test-threads=1
```

The second command requires `VIBEMUX_AAG_V2_PROBE_TEST`, an absolute path to the
prebuilt VibeMux consumer test executable. It runs only the exact ignored source
fixture with an owned ephemeral listener, synthetic upstream and synthetic token.
Its child must exit zero and emit the expected success sentinel. A zero-test filter
match cannot pass this gate. Original v1 Hub acceptance remains a separate check.

Roll out the producer before the consumer, then explicitly enable the consumer's
reviewed policy. These source changes perform no rollout or configuration mutation.
V1 consumers can continue unchanged. New VibeMux gateway-observed stored records
require a compatible reader on rollback; disabling new admission does not rewrite
those records. Producer receipt memory still expires on restart or retention.


### Version 2 verification, 2026-10-28

Eight focused admission tests passed, including stale instance/generation/pin
refusal before upstream I/O, post-capture reload isolation, v1 compatibility and
version-specific lookup. The separately selected actual AAG-to-VibeMux HTTP
fixture passed with one synthetic upstream request. The existing actual Hub v1
consumer fixture also passed against this producer.

The affected proxy suite passed 459 tests (five optional tests ignored), and
all 43 runtime-snapshot tests passed. The proxy run used explicit test-process
`NO_PROXY=127.0.0.1,localhost,::1`: the unchanged connection-reuse test failed with
ambient proxy behavior and passed in a same-image paired direct-loopback control.
Global proxy settings, production client behavior and assertions were unchanged.
Original failures remain retained; ambient connection reuse is not qualified by
the isolated result. The build emitted 142 warnings. This is not full-library or
native-release acceptance.

A concurrent Gemini compile error initially blocked the broad run; this task
preserved that work, and a later owner correction allowed compilation. The first
v2 cross-repository probe also exposed a reserved-prefix error in its synthetic
consumer configuration; that fixture was corrected without relaxing validation.
The source review has no remaining actionable findings. AAG advanced concurrently
from 98ae3df2 through 92e56fbe to 45c98a2d; verification covered the uncommitted receipt diff and no runtime was
replaced or deployed by this task.


Privacy contract tests passed 32 checks. The separate working-tree publication
audit still reports 22 blockers across nine files unchanged from the intake
revision; none are in this task's receipt changes. Those findings remain blockers
for public export. No release or publication readiness is inferred from the
focused producer and consumer results.
