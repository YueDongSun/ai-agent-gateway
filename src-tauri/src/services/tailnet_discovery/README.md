# Tailnet AAG discovery

This service reads the installed Tailscale client's bounded status document and
probes one saved AAG port on online overlay nodes. It never scans address ranges,
resolves caller-supplied hosts, changes routing/publication, or sends credentials.

The probe contract is `GET /aag/discovery`, returning exactly:

```json
{"service":"ai-agent-gateway","schemaVersion":1,"status":"running","version":"0.2.0"}
```

Only this marker makes a node an observed-online AAG instance. An ordinary HTTP
200 remains unverified; 401/403 is auth-protected; connection failures are not
responding; other transport/server failures have their own count. The UI exposes
addresses only for marker-verified instances. Names/addresses of other peers do
not leave this service, and discovery results are not persisted or logged.

The port is the saved publication port when publication is enabled, otherwise
the saved local gateway port. Discovery does not open a listener or make a
loopback-only gateway reachable over Tailnet.

Limits are centralized in the implementation: 1 MiB status JSON, 512 peer
documents, 64 candidate nodes, eight concurrent HTTP probes, 8 KiB marker bodies,
3 s status-process deadline, 1.5 s per HTTP probe, and 15 s scan budget. Only
Tailscale CGNAT/ULA addresses from `Self` and `Peer` are admitted. DNS/physical
endpoint fields are ignored. Redirects and ambient HTTP proxies are disabled.

One service admits one request at a time. Cancellation is bound to that exact
request ID. An owned task keeps admission until the metadata process reaches its
terminal result and remaining HTTP futures are dropped, even if the IPC caller
disappears. `cancel_all` is available to native exit cleanup. The Suppliers UI's
one-minute timer is opt-in and ends on unmount, which also cancels its own scan.

The installed executable resolver currently supports Windows. Other platforms
return an explicit unsupported state. Older or authenticated listeners that do
not expose the marker cannot be identified as AAG without disclosing credentials;
the UI reports aggregate auth/unverified states instead.

Focused native fixtures:

```text
cargo test --locked --offline --lib services::tailnet_discovery -- --test-threads=1
```

Parser and admission tests use in-memory documents/state. HTTP tests bind
isolated loopback listeners. No test invokes the installed Tailscale binary or
scans a live Tailnet.
