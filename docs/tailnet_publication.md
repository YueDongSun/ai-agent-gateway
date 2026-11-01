# Tailnet gateway publication

The Rust/Tauri desktop app can explicitly publish its gateway through an owned
foreground Tailscale Serve session. This feature is disabled by default. The
Node gateway remains independent.

## Configuration and controls

The settings DAO stores validated JSON under `tailnet_publication_config`:

```json
{
  "enabled": false,
  "port": 8989
}
```

An absent setting uses these defaults. Existing saved ports, including an
explicit port 443, retain their values. The accepted fields are `enabled`
(boolean) and `port` (an integer from 1 through 65535). Malformed JSON, extra
fields, invalid types, and an oversized saved value fail validation. AAG chooses
the private backend address; it is not a configurable field.

The gateway panel provides an auto-start checkbox, TCP port, Save, Start sharing,
Stop sharing, and Refresh. Their behavior is:

- Save commits the validated policy and then stops any current publication.
  If cleanup reports an error, the saved policy has still changed.
- Start sharing requires the saved policy to be enabled and the local gateway
  to be accepting requests. The UI saves its current draft before starting.
- Stop sharing closes the current publication while keeping the local gateway
  running. It preserves the saved auto-start preference.
- Clear the auto-start checkbox and Save to disable future automatic sharing.
- A successful local gateway startup attempts publication when the saved policy
  is enabled. This covers application auto-start and supplier/launcher paths
  that bring up or replace the gateway. A publication startup failure leaves
  the local gateway running and exposes a safe error code.

The native IPC surface is `tailnet_publication_config_get`,
`tailnet_publication_config_save`, `tailnet_publication_start`,
`tailnet_publication_stop`, and `tailnet_publication_status`. Publication DTOs
contain settings, state, a qualified endpoint, and safe error codes. They do
not contain the gateway key. Command registration, the build-time application
manifest, and the trusted manager capability grant cover these commands.

## Request and ownership boundary

The publication frontend binds a separate ephemeral IPv4 loopback listener.
It reuses the accepting gateway's `ProxyAppState` and router builder, including
the same routing owner, hot snapshots, providers, and current gateway key.
The main gateway keeps its configured listener and local API behavior.

An outer authentication layer covers every published route, fallback, and
method mismatch. It requires the existing gateway credential even when local
Anthropic subscription passthrough permits another inbound credential. Existing
protocol-specific credential precedence remains in force. No publication key
is generated, and stopping publication does not rotate the gateway key.

Current transport support is HTTP/1.1, including SSE and WebSocket upgrades. The displayed endpoint
uses `http://`: the foreground Serve session forwards TCP to this HTTP frontend.
AAG does not configure HTTPS termination. WebSocket routes extract
`Extension<PublicationUpgradeOwner>` and submit their complete socket-owning
handler future through `spawn`. This gives the frontend an explicit cancellation
and join boundary. It does not terminate arbitrary tasks detached by a route.
The session broker itself remains a separate, uninstalled component.

The native service resolves the installed Windows executable from the OS
Program Files known folder and the fixed `Tailscale/tailscale.exe` suffix.
Reparse paths are rejected. It does not select an executable from `PATH` or a
caller-supplied path. Other platforms currently return
`tailnet_publication_platform_unsupported`.

Startup reads bounded `serve status --json` output and rejects an occupied
requested port, a Funnel conflict on that port, or unverifiable ownership.
The launched command has this shape:

```text
tailscale serve --tcp=<publication-port> tcp://<owned-loopback-backend>
```

The child remains foreground and belongs to the native process owner. AAG
records the exact new session/route, verifies the target, and checks that
unrelated Serve configuration has not changed. Cleanup terminates its own child
handle and verifies route removal. It does not invoke Funnel, `--bg`, port-wide
Serve `off`, or `reset`. Existing unrelated routes remain outside its ownership.

## Stop, cancellation, and local gateway completion

One managed `Publication_service` owns lifecycle admission. Operations use the
proxy-state lock before the publication operation/manager locks. Publication
workers do not acquire proxy state in the opposite order.

Stop synchronously signals cancellation, closes the publication listener,
shuts down every tracked accepted socket including upgraded connections,
aborts accepted HTTP connection tasks and registered upgrade handlers, and joins
them. Socket guards remain attached to Hyper's transferred upgrade I/O, so an
upgrade continues to consume the connection limit. This drops pending HTTP
handlers, SSE response bodies, and idle/partial requests. It then stops and
verifies the exact foreground native receipt. Publication shutdown has its own
native cleanup budget; it does not reuse an already-cancelled startup flag to
skip verification.

Local gateway stop and replacement await publication cleanup before signalling
the main server. A publication cleanup error is returned while preserving the
local gateway state. The main gateway's existing graceful drain behavior is
unchanged.

The gateway completion subscription identifies one exact server run. Unexpected
completion of the current run cancels and stops its publication. A source check
before cancellation and another check before cleanup prevent delayed completion
from an older gateway from stopping a replacement publication. Startup also
races the bound gateway's completion, covering local failure during native
admission. The application owns its completion watcher and aborts and joins the
old watcher when replacing the binding.

Application exit permanently closes new publication admission and immediately
signals cancellation. The exit hook waits up to 20 seconds for cleanup before
the remaining application teardown continues. Native startup and stop have
bounded operations; an exit timeout remains an error, rather than proof of
verified removal. The owned child/job handles provide a final destruction
boundary for this process's child.

The manager's startup waiter always cancels synchronously when dropped. It
schedules cleanup only when an entered Tokio runtime is available. Otherwise,
the manager retains the exact native receipt for explicit reconciliation or
owner destruction. Status/stop keep that receipt in the slot across asynchronous
waits; runtime-aborted continuations cannot turn retained ownership into a false
`stopped` result.

## Endpoint, Tailnet policy, and firewall boundary

Endpoint discovery runs bounded `status --json` through the same installed
native executable. It projects only `Self.TailscaleIPs` when the backend reports
`Running`, validates overlay addresses, and prefers IPv4 when present. Peer
records and a top-level address list do not supply the displayed endpoint.
The UI displays an endpoint only when the publication is running and this self
metadata qualifies the address. Missing metadata leaves the endpoint unavailable
and exposes a safe error; it does not fabricate an address or hostname.

The intended remote entrypoint is the reviewed Tailnet Serve port, governed by
the host's Tailscale access rules and firewall policy, plus AAG's existing-key
authentication. The private publication backend listens on loopback. This
feature makes no host firewall or Tailnet access-policy changes and preserves
the main gateway's configured bind address. A pre-existing broad main-gateway
bind therefore requires its own review before claiming Tailnet is the only
remote entrypoint. Serve status and a displayed self endpoint do not establish
peer reachability or firewall isolation.

Browser-demo controls are synthetic. Their state changes create no native
process, Serve route, firewall rule, or qualified endpoint.

## State and recovery

| State or code | Meaning and next action |
| --- | --- |
| `running` | The publication owner reports its foreground child and exact route as current. Endpoint qualification is a separate metadata check. |
| `stopped` | No active owned publication remains in the manager. A completed native stop has confirmed removal before clearing its receipt. |
| `outcome_unknown` | Ownership or cleanup could not be confirmed, or cancellation retained an owner awaiting reconciliation. New starts are refused. |
| `tailnet_publication_disabled` | Enable and save the policy before an explicit start. |
| `tailnet_publication_config_invalid`, `tailnet_publication_port_invalid` | Correct the saved JSON or port through the settings controls. |
| `tailnet_publication_local_gateway_required` | Start an accepting local gateway first; a gateway that completed during startup cannot supply publication authority. |
| `tailnet_publication_existing_key_required` | Configure the local gateway's existing authentication key before sharing. |
| `tailnet_publication_slot_occupied`, `tailnet_publication_funnel_conflict` | The requested slot is already active or conflicts with Funnel. Inspect the existing owner; AAG does not replace it. |
| `tailnet_publication_ownership_unknown`, `tailnet_publication_ownership_lost` | Admission is closed and ownership needs reconciliation. Refresh status and retry Stop for a retained receipt. Preserve other Serve owners. |
| `tailnet_publication_process_unavailable`, `tailnet_publication_platform_unsupported` | The trusted installed executable or supported native platform is unavailable. AAG does not install or elevate automatically. |
| `tailnet_publication_status_invalid`, `tailnet_publication_output_limit_exceeded` | Native status is malformed or exceeds a bound. Inspect the installed service without pasting raw status or credentials into diagnostics. |
| `tailnet_publication_cancelled`, `tailnet_publication_exiting` | The operation was cancelled, or application exit has closed admission. |
| `tailnet_publication_startup_timeout`, `tailnet_publication_stop_timeout`, `tailnet_publication_command_timeout` | A bounded operation expired. Check status and ownership before retrying. |
| `tailnet_publication_frontend_*`, `tailnet_publication_metadata_task_failed` | A private listener/task or metadata operation failed. The UI retains the error for diagnosis. |

Some failed startups cannot produce a verified owned receipt. They retain an
unknown outcome and can keep rejecting Stop and Start. Review the actual native
status and its owner before another publication attempt. A port-wide reset or
disabling somebody else's route is not an application recovery operation.

Diagnostics retain safe codes, metadata, and sizes. Avoid copying keys, full
native status, peer records, prompts, or model responses into evidence files.

## Verification and current evidence

Run frontend checks from the repository root:

```text
npm run typecheck --prefix ui
npm run ui:build
npm run ui:build_demo
```

With `ui/dist` available, run focused Rust checks from `src-tauri/` using
disposable application configuration/home paths as described in the
[development guide](development.md):

```text
cargo check --locked --offline
cargo test --locked --offline --lib services::tailnet_publication:: -- --test-threads=1
cargo test --locked --offline --lib proxy::publication:: -- --test-threads=1
cargo test --locked --offline --lib services::proxy_runtime::lifecycle:: -- --test-threads=1
cargo test --locked --offline --lib commands::proxy::lifecycle_tests:: -- --test-threads=1
cargo test --locked --offline --lib commands::acl_tests:: -- --test-threads=1
```

Coverage includes strict authentication, real loopback HTTP/SSE/idle connection
cancellation, fake-native lifecycle ownership, stale/current gateway completion,
startup failure, saved-policy validation, self-only endpoint projection, and
runtime teardown/reconciliation. Native child fixtures exercise owned test
processes; they do not establish a live Tailnet publication.

Recorded implementation-stage evidence is 26 passing Tailnet tests and six
passing frontend/auth tests before the two added runtime-drop regressions and
the final default-port update. The UI build passed before its final default-port
tweak. These results do not establish fresh verification of the final revision;
current acceptance evidence belongs in `PROGRESS.md`.

No live Serve start/stop, host firewall validation, or peer acceptance has been
performed for this feature in that evidence. Signing, release packaging,
publication, and pushing are also outside that evidence. A future live
acceptance must separately establish the foreground route's ownership,
authorized-peer behavior, wrong/missing-key rejection, stop during active
traffic, preservation of unrelated routes, and the reviewed firewall boundary.
