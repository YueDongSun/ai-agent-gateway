# Gateway router speed research

Research date: 2026-10-08. Reviewed baseline:
`de287e1caf28ad419213b6fe04c04f6cc4e5b2d9`.

## Decision and scope

Optimize the shared Rust upstream SSE decoder first. Source inspection found
repeated movement of unread bytes and repeated scanning of a growing partial
line. Local release benchmarks confirm that these costs are material under
coalesced events and fragmented large tool arguments. The decoder serves the
OpenAI, Anthropic, and Gemini providers, so one bounded change improves all
three streaming input paths.

This change addresses local CPU work. It does not establish lower provider
time to first token, faster model generation, or a production latency result.
Ordered provider choice, retry eligibility, first-event commitment, transport
settings, authentication, native lifecycle, and the independently runnable Node
gateway retain their existing contracts.

## Recent repository and upstream updates

The current checkout already contains `793574d` (2026-09-17), which shares
explicit route hops through `Arc` instead of cloning them per request.
The 2026-10-07 proxy and discovery changes (`84ba873`, `9159b06`) restrict
passthrough credentials and apply model upgrades from current state.
Those contracts must survive a performance change. Reusable provider HTTP
clients also already exist in `proxy/providers/http_client.rs`; introducing
another pool would duplicate an existing capability.

Observed runtime/dependency versions are Node 24.18.0, bundled Undici 7.28.0,
Hono 4.12.32, reqwest 0.12.28 for the proxy, Hyper 1.11.0, hyper-util 0.1.20,
Tokio 1.53.1, and bytes 1.12.1. The lockfile also contains a transitive reqwest
0.13.4; that does not mean the gateway uses the newer reqwest API.

Current upstream releases were inspected as well as installed-version docs:

- [reqwest 0.13.5](https://github.com/seanmonstar/reqwest/releases/tag/v0.13.5),
  released September 8, includes DNS/error classification, proxy-auth fixes,
  fewer clones, and reuse of a read-timeout timer. The timer change concerns
  configured read timeouts; AAG's client builder does not configure one.
  These release notes do not establish an AAG speed gain from a major-version
  migration. Keep a transport upgrade separately measurable.
- [Hono 4.13.13](https://github.com/honojs/hono/releases/tag/v4.13.13), released
  October 4, introduces mount middleware and fixes JSX behavior. These changes
  do not identify a performance improvement for AAG's JSON/SSE endpoints.
- [Undici 8.11.2](https://github.com/nodejs/undici/releases/tag/v8.11.2), released
  September 24, fixes aborted-request reconnects and rejected HTTP/2 WebSocket
  streams. The newer 7.x release also contains a decompression backpressure
  fix. Node's bundled Undici version is a separate observation from the npm
  package release; changing transport APIs needs a pinned compatibility test.

## Mechanism and alternatives

The original decoder uses `Vec::drain` for each line and `remove(0)` for an
optional LF after CR. A chunk containing many events repeatedly shifts its
remaining suffix. A partial line is scanned from byte zero each time another
HTTP fragment arrives. Together these operations can produce quadratic work
in chunk density or fragment count.

The candidate uses `BytesMut::split_to` and `Buf::advance` to consume prefixes,
plus a scan offset that resumes at newly appended bytes. The installed
[bytes 1.12.1 documentation](https://docs.rs/bytes/1.12.1/bytes/struct.BytesMut.html#method.split_to)
defines splitting as a constant-time operation using indices and reference
counting. This removes per-line suffix movement, but does not make the entire
decoder zero-copy: ingress chunks still enter a bounded buffer, UTF-8 is
validated, and owned event strings are produced. Buffer growth may still
reclaim or reallocate storage.

The [SSE specification](https://html.spec.whatwg.org/multipage/server-sent-events.html#parsing-an-event-stream)
requires UTF-8, recognizes CR/LF/CRLF, joins data lines, and dispatches on a
blank line. Its framing rules guide regression coverage. AAG additionally
treats an incomplete terminal event or invalid UTF-8 as an explicit failure;
the optimization preserves that stricter existing provider contract.

Other approaches were assessed:

1. **Transport tuning or replacement.** Installed
   [reqwest 0.12.28](https://docs.rs/reqwest/0.12.28/reqwest/struct.ClientBuilder.html)
   already enables `TCP_NODELAY` by default. AAG explicitly sets a 90-second
   idle timeout, four idle connections per host, and TCP keepalive. An idle
   pool limit is not an active-request concurrency limit. HTTP/2 window changes
   need evidence of flow-control stalls. Shorter total deadlines can truncate
   valid long streams. The existing
   [Hyper streaming-body contract](https://docs.rs/hyper/1.11.0/hyper/body/)
   supports demand-driven reading and inexpensive byte handles, so a wholesale
   raw-Hyper rewrite is not justified by the measured decoder problem.
2. **Adaptive provider ordering.** An EWMA or percentile policy could favor
   faster successful providers without duplicating requests, but it changes
   configured ordered-hop semantics. It needs an explicit opt-in objective,
   comparable capabilities, separate failure/timeout observations, and tests
   for both first-token and completion latency. It cannot be treated as a
   transparent decoder repair.
3. **Hedged upstream generation.**
   [The Tail at Scale](https://research.google/pubs/the-tail-at-scale/)
   describes delayed duplicate requests and cancellation for replicated
   services, with load costs and workload-specific benefits. Paid,
   nondeterministic model generation has different semantics. Cancelling a
   losing HTTP request does not prove its upstream computation or billing
   stopped. Hedging would require a separate opt-in execution/budget contract.
4. **Node fetch replacement and cleanup.** Official
   [Undici guidance](https://undici.nodejs.org/)
   says response bodies must be consumed or cancelled to release resources.
   AAG's Node health method currently returns after checking status, making
   cleanup a concrete future investigation. Undici's generic throughput
   benchmark uses high connection counts and pipelining; those results do not
   prove lower latency for independent long SSE streams. Benchmark before
   replacing `fetch`, and test cancellation coupling before using pipelining.

## Benchmark method

`scripts/router_performance` is a separate offline Rust crate that imports the
actual decoder source by path, avoiding a native GUI build for each experiment.
The optional baseline feature imports a temporary decoder extracted directly
from the exact Git revision above. No copied baseline source is retained.

Both decoders run in the same process with identical bytes, alternating order
for 31 measured samples after three warmups each. Each fixture represents about
8 MiB of wire data. The release profile matches the desktop application's
`opt-level = "s"`, LTO, codegen-unit, and panic settings. The observed host is
Windows x64 with an Intel Core i7-13700K; this remains a host-local synthetic
result, not a controlled hardware laboratory benchmark.

Fixtures cover roughly 200-byte JSON events; 200-byte, 1 KiB, 16 KiB, 64 KiB,
and 1 MiB HTTP chunk sizes; LF and CRLF framing; and 64 KiB JSON tool events
fragmented into 128-byte or 1 KiB chunks. Fixture construction is outside the
timer. Timed work includes cloning input byte handles, constructing/polling the
decoder, and collecting decoded strings. Exact event count, order, and payload
equality are checked after every sample. Median and nearest-rank p95 batch
duration are reported in microseconds. Output collection belongs to the
benchmark harness; production still yields events incrementally.

The comparison is evidence for decoding cost under these fixed inputs.
It excludes JSON provider conversion, HTTP/TLS, upstream generation, downstream
rendering, end-to-end request latency, and production memory high-water marks.
Small-chunk results are more sensitive to scheduling noise than the large
algorithmic improvements. No production/provider load test was run.

## Measured results and verification

Accepted comparison: final lazy-fixture runner, 31 samples per decoder, three warmups, approximately 8 MiB per fixture, matching application release settings. Every fixture passed exact output equality. Times below are median batch duration in milliseconds; p95 is included to show observed scheduling variability.

- `coalesced_16k_crlf`: 21.781 -> 9.850 ms, 54.8% less time; p95 24.274 -> 10.621 ms; 41944 events, 8388800 input bytes.
- `coalesced_16k_lf`: 18.925 -> 11.857 ms, 37.4% less time; p95 25.640 -> 13.383 ms; 42367 events, 8388666 input bytes.
- `coalesced_1k_crlf`: 16.147 -> 12.243 ms, 24.2% less time; p95 17.700 -> 13.775 ms; 41944 events, 8388800 input bytes.
- `coalesced_1k_lf`: 15.312 -> 11.436 ms, 25.3% less time; p95 20.166 -> 13.285 ms; 42367 events, 8388666 input bytes.
- `coalesced_64k_crlf`: 62.831 -> 10.902 ms, 82.6% less time; p95 69.365 -> 12.475 ms; 41944 events, 8388800 input bytes.
- `coalesced_64k_lf`: 39.805 -> 11.338 ms, 71.5% less time; p95 51.050 -> 13.501 ms; 42367 events, 8388666 input bytes.
- `packed_1m_lf`: 635.006 -> 11.525 ms, 98.2% less time; p95 659.201 -> 12.496 ms; 42367 events, 8388666 input bytes.
- `tool_event_64k_fragment_128b`: 667.891 -> 9.833 ms, 98.5% less time; p95 709.433 -> 13.948 ms; 128 events, 8395008 input bytes.
- `tool_event_64k_fragment_1k`: 90.049 -> 7.423 ms, 91.8% less time; p95 96.268 -> 8.247 ms; 128 events, 8395264 input bytes.
- `typical_crlf_200b`: 17.363 -> 14.579 ms, 16.0% less time; p95 20.525 -> 18.495 ms; 41944 events, 8388800 input bytes.
- `typical_lf_200b`: 18.882 -> 14.483 ms, 23.3% less time; p95 23.402 -> 19.766 ms; 42367 events, 8388666 input bytes.

All measured medians and p95 values improved in this run. The largest gains address the deliberately adverse packed/fragmented inputs. Small token chunks already have low absolute local cost: for example, the 200-byte LF batch contains 42,367 events, so its median improvement averages about 0.104 microseconds per event. This result should not be presented as a comparable percentage reduction in normal model response time. Two preceding probes showed the same direction of improvement, with expected host timing variation.

Observed validation commands and outcomes:

- `cargo test --locked --offline --lib proxy::providers::sse:: -- --test-threads=1`: initial seven decoder tests passed. After adding Pending/error/CR-only EOF coverage, all nine passed in the proxy scope and the standalone test invocation below.
- `cargo test --manifest-path scripts/router_performance/Cargo.toml --locked --offline`: 9 passed, 0 failed on the final source.
- `cargo run --manifest-path scripts/router_performance/Cargo.toml --locked --offline --release --features baseline -- --samples 31 --target-bytes 8388608`: passed all 11 paired fixture comparisons on the final runner.
- `cargo check --locked --offline` in `src-tauri`: passed.
- Scoped `rustfmt --check` for `sse.rs`, benchmark `cargo fmt --check -- --config skip_children=true`, and `git diff --check`: passed.
- Full affected proxy scope: 191 passed, 1 failed, 1 ignored. The unchanged `http_client::tests::shared_client_reuses_an_idle_connection` expected one connection but observed two; an isolated rerun also failed. Its cause remains unresolved. The nine decoder tests and route/protocol tests passed; this is not an entirely green proxy gate.
- First broader Rust library attempt: 513 passed, 91 failed, 8 ignored (612 tests, before the two final decoder edge cases were added). A targeted rerun of unchanged `config::private_fs::tests::atomic_write_rejects_a_deterministic_temporary_collision` failed with `private_storage:owner_mismatch`. The filesystem source has no diff. The wider suite is not accepted as passing, and the remaining failures were not all diagnosed individually.

Independent source review found no decoder regression and prompted the lazy-fixture allocation fix and standalone test dependency. Node/UI checks and real-provider/native deployment acceptance were not claimed. No dependencies in the production manifests or lockfiles changed.

## Follow-up priorities

After this decoder change, measure these source-backed candidates separately:

- Automatic model-route resolution scans advertised models and searches the
  priority queue during sort comparisons. A snapshot-owned model index and
  precomputed priority ranks could remove repeated work, provided tests
  preserve alias precedence, disabled-provider behavior, surface compatibility,
  credential admission, and provider ordering.
- The Rust server serializes a canonical chunk to a string and then copies it
  into an SSE frame. A bounded writer could reduce event-frame allocation if
  profiling shows that step matters after decoder optimization.
- Test Node health/body cleanup and client disconnects with an owned fixture
  upstream. Assert body cancellation and connection recovery explicitly.
- Add a scheduled-byte fixture relay benchmark for added first-event and p95
  forwarding delay, slow consumers, and concurrent streams before claiming
  user-visible HTTP latency improvement.

Only move proven blocking work to a bounded executor. Installed
[Tokio guidance](https://docs.rs/tokio/1.53.1/tokio/task/fn.spawn_blocking.html)
warns that CPU-heavy work can prevent other futures progressing, while started
blocking tasks cannot be aborted. Moving every small serialization off-thread
would add its own scheduling and cancellation costs.
