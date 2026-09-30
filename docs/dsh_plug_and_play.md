# DSH plug-and-play setup and forwarding

This implementation reduces manual runtime configuration while preserving
explicit provider/model choices. It detects existing software and prepares an
AAG-managed connection; it does not install a global runtime, read native
Claude credentials, change model families, or start DSH on page mount.

## Lessons from cc-switch

The primary reference was farion1231/cc-switch at
d8065cc628fcd373d00c4363d718095f19e78c9e (2026-08-29). The inspected repository
has an MIT license; no source code was copied into AAG.

| Reference | Applied principle | Deliberate boundary |
| --- | --- | --- |
| [Per-app switch lock](https://github.com/farion1231/cc-switch/blob/d8065cc628fcd373d00c4363d718095f19e78c9e/src-tauri/src/proxy/switch_lock.rs) | Serialize related state changes and protect backup/selection consistency | AAG uses its existing managed-instance lock and transaction journal |
| [Live provider configuration](https://github.com/farion1231/cc-switch/blob/d8065cc628fcd373d00c4363d718095f19e78c9e/src-tauri/src/services/provider/live.rs) | Provider-owned configuration and explicit values take precedence over defaults | AAG writes its isolated DSH profile, not another application's live configuration |
| [Provider router](https://github.com/farion1231/cc-switch/blob/d8065cc628fcd373d00c4363d718095f19e78c9e/src-tauri/src/proxy/provider_router.rs) | Configured order and circuit state determine retry candidates | Explicit AAG routes and pins remain authoritative; automatic candidates keep the exact requested model |
| [Thinking rectifier](https://github.com/farion1231/cc-switch/blob/d8065cc628fcd373d00c4363d718095f19e78c9e/src-tauri/src/proxy/thinking_rectifier.rs) | Compatibility repairs need observable triggering errors | AAG does not copy broad generic-invalid-request rules that remove thinking blocks |
| [Reasoning bridge](https://github.com/farion1231/cc-switch/blob/d8065cc628fcd373d00c4363d718095f19e78c9e/src-tauri/src/proxy/providers/reasoning_bridge.rs) | Opaque reasoning needs explicit protocol/provenance contracts | AAG does not introduce proprietary pseudo-signatures or claim full Responses reasoning replay |
| [Environment checker](https://github.com/farion1231/cc-switch/blob/d8065cc628fcd373d00c4363d718095f19e78c9e/src-tauri/src/services/env_checker.rs) | Report configuration conflicts rather than hide them | Passive runtime discovery does not collect raw API environment values or native agent settings |

These references informed responsibility and failure boundaries; their features
are not presented as automatically available in AAG.

## Automatic installation detection

Opening the DSH page performs bounded, read-only filesystem discovery:

1. Preserve already configured runtime paths.
2. Check an explicitly configured AAG_DSH_RUNTIME_DIR, if present.
3. Inspect executable/package siblings of up to 48 absolute PATH directories.
4. Inspect the bounded npm `_npx` cache for official DSH package roots.
5. Inspect fixed npm/pnpm user/system prefixes and common Homebrew layouts.

Discovery checks at most 1,024 metadata locations and returns at most 48
candidates. Package manifests are capped at 256 KiB. It does not recursively
scan repositories/home directories, execute shell shims, download packages or
make HTTP discovery requests. Windows UNC/device inputs are rejected;
OS-mounted filesystems remain subject to normal filesystem behavior.

Node candidates have only a static executable-format check. Their version and
identity remain unverified until explicit execution; a filename or PE/ELF
header is not a trust certificate. DSH and pnpm candidates are matched against
their package names, bin entries, package containment and supported versions.
An official `npx @deepseek-ai/dsh@0.1.1-rc.2` cache entry is launched through
its verified `lib/bin.js` path inside AAG's owned Node process; AAG does not
execute the `npx.cmd` shell shim or silently resolve a floating package version.
The current qualified package boundary remains DSH 0.1.1-rc.2 and pnpm 10.34.5.
Unsupported/missing installations are shown, not silently substituted.

The repository's test runtime is never treated as an implicit production
installation. Explicit deployment/test configuration may point at it, with a
visible diagnostic. Missing software produces actionable setup guidance; the
page does not silently install system dependencies.

## Automatic provider setup

The Auto setup action prepares a reviewable plan:

- Nonempty saved DSH paths, provider and model selections win.
- An explicit selection in the proposal is used only for unset values.
- Otherwise the current AAG Claude provider is preferred.
- Its configured model is proposed only when explicit, unambiguous and present
  in the source metadata.
- With no current selection, a unique available source/model may be proposed.
- Ambiguous, unavailable or stale choices require an explicit selection.
  An unavailable current Claude source does not silently become another model
  family.

Providers are AAG's saved/imported sources, including the existing cc-switch
import path. This feature does not scan or copy native client credential files.
Credentials still resolve in Rust immediately before runtime launch.

The preview is bound to the settings/profile digest, selected source metadata,
a five-minute expiry and a single-use plan ID. Application revalidates runtime
metadata and state under the instance lock. It atomically promotes managed
settings and, if absent, the initial web profile, retaining backups and
interruption markers. Existing profiles remain byte-for-byte unchanged.
A failed second write restores the first or reports rollback failure explicitly.

Setup does not execute a package manager, start DSH or contact a provider.
Start remains a separate user action. Unsaved form edits block automatic setup
instead of being overwritten by background detection. The new APIs are limited
to the trusted main window; the marketplace and DSH runtime WebUI cannot use them.

~~~text
dsh_discover_runtime
dsh_preview_setup
dsh_apply_setup
~~~

## Dispatch adaptations

Explicit route order and forced-provider pins are unchanged. For automatic
routing among enabled providers advertising the exact requested model, the
matching surface queue takes priority. Remaining matching providers follow
stable provider-ID order. HashMap iteration no longer makes this choice.

Only network failures, HTTP 429 and HTTP 5xx cause failover. Explicit supported
overload/rate-limit SSE errors can be retried before the first committed chunk.
Authentication, validation and unknown terminal errors are not silently rerouted.
No failover occurs after streaming begins.

An owned provider-attempt guard tracks completion rather than treating the first
event as success. A real terminal followed by a clean stream end updates success
and last_used. Late retriable errors count once toward the breaker. Cancellation
releases its own half-open permit without claiming success or failure, and a
stale attempt cannot release a newer probe generation. Winner persistence occurs
only after successful stream consumption.

## Thinking and stream compatibility

- Native Anthropic request/response/SSE envelopes retain thinking,
  redacted_thinking, signatures, tool-block order and usage extensions.
- OpenAI reasoning and reasoning_content fields, delta fields, response/choice
  extensions and extra usage fields survive the OpenAI surface.
- Cross-protocol output includes actual returned reasoning only. It does not
  invent a signature or reveal unavailable reasoning.
- SSE decoding buffers bytes through complete events, handling split UTF-8,
  Chinese/emoji, CRLF and multi-line data correctly. Events above 8 MiB fail
  explicitly rather than truncating or consuming unbounded memory.
- Body read/UTF-8/JSON/framing errors remain errors. The proxy does not append
  a synthetic successful terminal after an error.
- Responses completion markers are ordered after response events; DONE can no
  longer race ahead of content.
- Thinking/text blocks close before tool blocks. Later text never reuses a tool
  block index. Reasoning precedes answer text when both arrive in a delta.
- Client cancellation drops the owned upstream response body.

Diagnostics contain status/timing/identity metadata, not upstream payloads or
thinking text. Existing narrow thinking/signature compatibility retries remain
distinct from lossless forwarding and are not expanded to generic errors.

The Responses adapter still uses the existing chat-canonical representation.
This slice fixes terminal sequencing/error handling; it does not implement
full native Responses tools/encrypted reasoning replay or guarantee lossless
conversion between every protocol.

## Verification and operating limits

See [validation record](dsh_plug_and_play_validation.md) for fresh unit,
loopback HTTP, browser, native and actual-DSH evidence. Original tests remain in
place. No live paid model was invoked and no existing Claude/DSH configuration
was modified during verification.

The Rust/Tauri gateway is the scope of these forwarding changes. The legacy Node
runtime remains independently runnable and receives its regression gate; it is
not silently migrated to the Rust implementation. Native signing/installers,
cross-platform execution and live provider performance remain separate gates.
