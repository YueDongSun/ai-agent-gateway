---
name: aag-gateway-debug
description: Debug AAG proxy routing, failover, circuit-breaker, or Anthropic/OpenAI format-conversion issues. Use when a request returns the wrong provider, failover isn't triggering, a breaker is stuck open/closed, or a stream errors mid-flight. Includes the /health inspection workflow, the mock-fail reproduction recipe, and the retriable-classification decision tree.
---

# Skill: aag-gateway-debug

Debug the gateway proxy: routing, failover, circuit breaker, or format
conversion. Applies to the Node proxy (`src/`, working today) and the Rust proxy
(`src-tauri/`, Phase 2). Commands target the Node proxy on `127.0.0.1:8787`.

## Step 0: Is the gateway up?

`curl http://127.0.0.1:8787/health` -> `{"status":"ok","providers":[...],"models":[...]}`.
Connection refused -> `npm run gateway` (Node) or `proxy_start` (Rust desktop).
If `authToken` is set (in `~/.ai-agent-gateway/config.json` Node, or the
`proxy_config` row Rust), add `-H "Authorization: Bearer <token>"` (or
`-H "x-api-key: <token>"`) to every curl.

## Step 1: Inspect breaker states

`GET /health` returns each provider's breaker snapshot: `state` (`closed` |
`open` | `half-open`), `failures`, `successes`, `openedAt` (unix ms),
`cooldownMs`. Node uses string states; Rust serializes lowercase (`half-open`
keeps the hyphen).

- A breaker is genuinely "stuck open" only if `now - openedAt < cooldownMs`.
- The open->half-open transition is **lazy**: it fires inside `allow()` on the
  next request after cooldown, NOT on a timer. A breaker that "should" be
  half-open only flips when the next request probes it.

## Step 2: Reproduce / isolate

### "Failover isn't happening"
1. Confirm the route has >=2 hops: `GET /v1/models` lists aliases; check
   `~/.ai-agent-gateway/config.json` `routes[]` (Node) or the `routes` table
   (Rust) for the alias's `hops`.
2. Hit the model and watch `router:` logs: each hop logs
   `FAIL <provider>/<model>: <msg> (retriable=true|false)` then
   `OK <alias> <- <provider>/<model>` on success.
3. `retriable=false` -> error propagated WITHOUT failover (expected for 4xx).
4. `circuit OPEN for "<provider>", skipping` -> provider was open and skipped;
   check `/health` for why it opened.

### "Breaker never opens / opens too early / never recovers"
- Threshold = per-provider `circuitBreaker.failureThreshold` (consecutive
  failures in CLOSED). Seed sets `mock-primary` to 3.
- Reproduce with the seeded `mock-fail` alias (primary always 503 retriable,
  fallback 200): send 3 `mock-fail` requests, then `GET /health` ->
  `mock-primary` breaker `state == "open"`. After 3, later requests skip primary
  and go straight to fallback.
- "Opens too early": check `failureThreshold`; a success in CLOSED resets the
  counter (`recordSuccess` sets `failures=0`).
- "Never recovers": needs (a) cooldown elapsed, (b) a request to probe (lazy),
  (c) `successThreshold` consecutive successes in half-open. A half-open FAILURE
  immediately re-opens. No traffic => stays open forever (by design).

### "Stream errors partway through / no failover mid-stream"
**Expected, not a bug.** Streaming failover is **one-shot**: `Router.stream`
probes only the FIRST chunk before committing (`src/gateway/router.ts:~102`).
Once a provider yields any byte, a mid-stream error surfaces to the client as an
SSE `data: {"error":{...}}` line then `[DONE]` - no transparent failover. Do not
promise mid-stream failover in fixes or docs.

### "Wrong response shape / format conversion"
- OpenAI clients hit `/v1/chat/completions`; Anthropic clients hit `/v1/messages`
  (Node only - Rust `/v1/messages` is Phase 2 Step 5, not yet implemented).
- The Anthropic edge converts incoming -> canonical OAI via
  `anthropicReqToOAIReq` (edge), routes, then converts back via
  `oaiRespToAnthropicResp` / `oaiChunksToAnthropicStream`.
- **Converter is text-only**: `textOf` flattens non-string content (tool calls,
  images) to text. Multi-modal/tool-use content is DROPPED. If a request loses
  tool calls, that's why - it's a known limitation, not a regression.
- Anthropic SSE is **synthesized** from OAI chunks, not parsed from native
  upstream events. `anthropicStreamToOAIChunks` listens for `message_start` /
  `content_block_delta` / `message_delta` / `message_stop` only.

## Retriable-classification decision tree

`makeError(msg, {status, retriable})` -> `retriable` defaults to
`status === undefined || status >= 500`.

| HTTP status | retriable | Router behavior |
|-------------|-----------|-----------------|
| 5xx (500/502/503/504) | true | record failure, try next hop |
| 429 | true | record failure, try next hop |
| network error / timeout | true | record failure, try next hop |
| 400 / 422 (bad request) | false | propagate immediately, NO failover |
| 401 / 403 (auth/perm) | false | propagate immediately, NO failover |
| 404 | false | propagate immediately, NO failover |

If failover is wrongly skipped on a 5xx: the provider's `makeError` likely set
`retriable: false` explicitly, or the breaker was already OPEN (check `/health`).
If failover wrongly happens on a 4xx: the error didn't carry a `status` (so
retriable defaulted to true) - the provider must set `status` on 4xx errors.

## Rust-proxy-specific notes

- Rust proxy is Phase 2: only `mock` provider, non-streaming, `/v1/chat/completions`
  only. No `/v1/messages`, no SSE. A 501 `streaming not yet implemented` is
  returned for `stream:true`.
- The Rust `Router` is a DB snapshot taken at `proxy_start` - immutable per run.
  Config changes need a proxy stop/start to take effect (unlike Node which
  rebuilds per process start).
- Rust logs go to `tauri-plugin-log`; breaker logs use tag `circuit:`, router
  uses `router:`, same as Node.

## After a fix

- Add/update a test: Node in `tests/e2e.test.ts` (mock substrate, no keys); Rust
  inline `#[cfg(test)]`. Run `aag-verify`.
- If you changed retriable classification or failover, that's a behavior change
  - a test is mandatory (AGENTS.md rule 8).
