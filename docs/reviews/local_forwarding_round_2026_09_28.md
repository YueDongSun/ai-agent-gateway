# Local-forwarding optimization round — 2026-09-28

## Source and scope

A follow-up optimization round focused on local forwarding: the relay path a
local Claude subscription client takes through AAG (`anthropic_passthrough`).
The pass reviewed the passthrough pipeline end to end (`server.rs` auth edge,
`router.rs` hop walk, `providers/anthropic.rs` relay) and closed three gaps,
plus one Windows storage-boundary defect the round exposed.

## Changes

1. `POST /v1/messages/count_tokens` is now served (neither runtime had it).
   Claude Code measures context on every turn through this endpoint; before,
   local clients got a 404. The routed provider answers: Anthropic-family
   hops relay the body upstream to the derived `messages/count_tokens`
   endpoint with the hop's upstream model (exact count; passthrough relays
   the client credential), other hops serve a deterministic local estimate
   (ASCII ≈ 4 chars/token, CJK ≈ 1 char/token). Auth uses the Anthropic
   surface gate — the `/v1/messages` relaxation (loopback bind + enabled
   passthrough row) covers it with the same fail-closed conditions. Router
   hop walking mirrors `complete_hops` skips, including the
   credentialless-passthrough skip and retriable 401 failover to static hops.
2. The client's `anthropic-beta` header is captured at the edge and relayed
   verbatim on passthrough hops (messages and count_tokens): a subscription
   login keeps its negotiated beta features (context management,
   token-efficient tools, …) through the relay. The static-key wire format
   is unchanged (byte-identical, no beta relay there).
3. `config.rs`/`database/mod.rs`: an explicit `AAG_CONFIG_DIR` tree is now
   created/hardened through the managed-private policy instead of the
   external policy. Defect: on Windows the parent tree can carry inherited
   group-writer ACEs (this host's sandbox stamps them on workspaces), and
   Node's `mkdir(0700)` cannot strip Windows inheritance — every private
   write into an explicit config tree then failed closed
   (`private_storage:directory_permissions_unsafe`), including the automatic
   verification gate's own config dir. The tree AAG is pointed at is
   AAG-owned state wherever it lives; hardening it is strictly tighter.
4. Endpoint derivation `anthropic_count_tokens_url` lives in the shared
   `endpoint.rs` (root/versioned/full-endpoint spellings, query preserved);
   test-coverage notes and `docs/architecture.md` updated.

Not changed, deliberately: `/v1/models` keeps the strict token gate (the
documented relaxation boundary is the messages surface); the Node legacy
gateway does not gain count_tokens (the Rust proxy is the product surface;
noted as an accepted asymmetry); the static-key hop does not relay
`anthropic-beta` (documented byte-identical contract).

## Evidence (Windows x64 host, disposable canonical config/home dirs)

| Command | Result |
| --- | --- |
| `cargo test --locked --offline --lib -- --test-threads=1` | 481 passed, 0 failed, 5 ignored (9 new tests: relay×2, beta×2, count relay×3, relaxation gate, estimate, URL derivation, hop-skip) |
| `cargo check --locked --offline --tests` | Passed, no new warnings |
| `npm run typecheck` | Passed |
| `npm test` | 76 passed, 1 skipped |
| `npm run release:verify` | 0 blockers, 0 warnings |

One release:verify iteration: inserting a plain helper fn between the
wildcard-bind loopback test and the next `#[tokio::test]` pushed the
scanner's ±240-char test-keyword window off the private-address fixture; renamed the helper
`count_tokens_test_payload` (matching the existing `anthropic_test_payload`
naming) — no scanner rules were touched.

## Limits

Local synthetic evidence only: upstream fixtures are loopback axum servers,
not real Anthropic relays; no real subscription OAuth, signing, installation,
or upgrade acceptance is claimed. The count estimate on non-Anthropic hops
is a heuristic, not a tokenizer.
