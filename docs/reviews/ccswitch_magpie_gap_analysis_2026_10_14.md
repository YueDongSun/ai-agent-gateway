# cc-switch and Magpie feature gap analysis

Research date: 2026-10-14. Baselines: AAG `master` @ `4f02046`; cc-switch
`3d176b9` (v3.17.0); `yetone/magpie` @ `main` (2026-10-14).

## Sources

| Project | Reference checkout | Shape |
|---|---|---|
| AAG | this repository | Rust (Tauri 2) + React, Node legacy gateway |
| cc-switch | `~/Documents/Codex/2026-07-05/ben/work/cc-switch-src/` | Rust (Tauri 2) + React |
| Magpie | `~/Documents/Codex/2026-07-05/ben/work/magpie-src/` | Go 1.26 (Wails v3), 3017 files, ~623k LOC |

`github.com:443` is unreachable from this host; the Magpie tree was fetched
through `codeload.github.com`, which responds. Both reference checkouts are
read-only.

## Why this is a port, not a fork

Magpie is 623k lines of Go across 60 `internal/` packages built on Wails v3.
`internal/agent` alone is 78k lines covering roughly 100 agent adapters;
`internal/gateway` is 167k lines. A line-level fork into a Rust/Tauri product is
not executable. cc-switch is the same language and architecture as AAG, so its
designs lift directly; Magpie's value is in specific *algorithms* (catalog
enrichment, quota-aware scheduling, format-preserving config surgery), each
small enough to reimplement in Rust.

The three projects occupy one problem space with different emphases:

- **cc-switch** — config management breadth: profiles, skills, sync, OAuth
  login flows, workspaces, sessions.
- **Magpie** — routing intelligence: live catalog enrichment, quota-aware
  scheduling across accounts, format-preserving config surgery, plugin system.
- **AAG** — protocol fidelity and failover correctness: bidirectional canonical
  conversion across four wire formats, circuit breakers, model auto-discovery.

AAG is already the strongest of the three on protocol conversion. Its gaps
against Magpie are concentrated in scheduling and catalog richness.

## Capability matrix

Legend: **full** = implemented; **part** = present but narrower; **no** = absent.

| Capability | cc-switch | Magpie | AAG |
|---|---|---|---|
| OpenAI Chat Completions ⇄ Anthropic ⇄ Responses ⇄ Gemini, bidirectional | full | full | **full** (canonical is Chat Completions) |
| Ordered failover + circuit breaker | full | full | **full** |
| Agent config switching, 8 app types | ~10 apps | ~100 apps | 8 types + 3 detect-only |
| TOML comment-preserving edit | no | **full** | **full** (2026-10-14, `toml_edit`-based merge) |
| JSONC comment-preserving edit (auto-switch) | no | **full** | **full** (2026-10-22, span-splicing patcher) |
| YAML config editing | no | **full** | **no** |
| Provider presets | ~40 | **83** | 31 |
| Live vendor `/v1/models` probing | yes | yes | **yes** |
| models.dev-style external catalog (prices, context, release dates, modalities) | no | **full** | **full** (2026-10-21, models.dev facts merged with live probing) |
| Routing modes beyond ordered failover | failover | **order / rotate / usage / pace / weight** | **order / rotate / weight / usage** (observed traffic; no quota `pace`) |
| Quota/allowance-aware scheduling | no | **full** | **no** |
| Routing groups | no | **full** (nested) | no |
| Reusing signed-in Claude Code / ChatGPT / Copilot OAuth as a provider | **full** (7 login commands) | **full** (reads + refreshes token files) | **no** (relay-only passthrough) |
| Usage/pricing accounting with IPC surface | full (13 commands) | full | **full** (IPC 2026-10-14; dashboard 2026-10-21) |
| Profiles (snapshot/apply all agent settings) | full | full | no |
| Skills | full (24 commands) | via plugins | no |
| S3 / WebDAV sync with IPC | full | davsync | service only, **no commands** |
| Plugin runtime | no | Go microkernel + OpenCode npm plugins + JS agent plugins | DSH marketplace (different concept) |
| Sessions tracking | full | full (~20 parsers) | no |
| Mid-stream failover after first byte | no | **full** (prefill-resume, `internal/gateway/continuation.go`) | **no** (documented non-goal) |

## Findings that matter most

### 1. Codex `config.toml` comments are destroyed

`src-tauri/src/services/provider/codex.rs:305-330` writes the mirror config as
a `format!` template — the entire file. A `# comment` the user wrote in
`~/.codex-cli/config.toml`, and any unrelated key they set, is destroyed on
every provider switch. Magpie solves this with byte-offset splicing against a
comment-blanked copy of the original:

- `internal/edit/json.go:91-116` — blank comments in place (same byte length),
  walk the key path, `splice` the value at the recorded offsets, infer
  indentation from the surrounding line, re-apply CRLF at `:55-62`.
- `internal/edit/toml.go:180-217` — replace the key's line span, preserving a
  trailing comment; `:77-135` splices whole tables Magpie owns.
- `internal/edit/yaml.go:117-148` — `yaml.v3` node tree, keeps comments and key
  order.
- `internal/edit/file.go:318-366` — `edit.Atomically` snapshots every listed
  file before a multi-file apply and restores on error.

AAG already has the atomicity half (`config_writes.rs`, 310 lines: validate,
backup, atomic rename, per-file rollback that refuses to clobber third-party
edits) and a partial JSONC stripper (`config_editor.rs:116-160`) used by the
*manual* edit path only. What is missing is the offset-splicing merge. This is
a correctness bug, not a feature gap: users lose data on an ordinary switch.

**Shipped for TOML (2026-10-14).** `services/provider/toml_merge.rs` merges the
managed keys into the file's existing bytes with `toml_edit`, which preserves
formatting and comments natively — so AAG does not need Magpie's hand-rolled
byte splicing to get the same result. Three subtleties the tests caught, all
now handled: a dotted path must be walked segment by segment (`doc["a.b"] =
Table::new()` duplicates an existing `[a.b]` header as a dotted key), an
existing key must be updated through `get_key_value_mut` rather than
`insert` (which drops a trailing comment), and the trailing comment lives in
the *value's* decor, which must be carried across.

**Shipped for JSONC (2026-10-22).** `services/provider/jsonc_edit.rs` is a
span-splicing patcher in the Magpie mold: it tokenizes strings and comments,
splices only the edited member spans, infers indent unit and line ending from
the structural bytes, inserts new members before a last member's trailing
comment, then re-parses the result and requires it to equal the intended
merged value; any doubt falls back to the previous full rewrite. The kilo,
opencode, and copilot auto-switch writers use it, so user comments, key order,
and formatting survive an ordinary switch.

### 2. Routing has exactly one strategy

`src-tauri/src/proxy/router.rs` implements ordered failover only. No random,
round-robin, weighted, usage-based, or latency-based selection; every
`classif*` hit in `proxy/` is `classify_upstream_4xx` (failover gating), not
routing. Magpie implements five (`internal/provider/routing.go:21-28`):

| Mode | Constant | Behavior |
|---|---|---|
| smart (default) | `""` | first with quota to spare, then the one with most |
| order | `Ordered` | next only when the one before cannot take it |
| rotate | `Rotate` | each in turn |
| usage | `LeastUsed` | least used first |
| pace | `Pace` | share of budget window left, per hour until reset |
| weight | `Weighted` | smooth weighted round-robin over a provider's keys |

The interesting part is not the modes but the substrate: `Allowance`
(`routing.go`, `For`/`Renewal`/`Pace`/`Pooled`) models each account's quota
windows — used share, reset time, window span, whether a window is scoped to
one model family or the whole account — and `planGroup`
(`internal/gateway/fallback.go:375`) plans a candidate order from them, with
nested routing groups and per-account rests. `Pooled` distinguishes "this
account is out of Opus" from "this account is out", which is the distinction a
flat ordered queue cannot make.

AAG's circuit breaker already tracks per-provider health; the missing piece is
*quota* as a routing input rather than a health input.

**Shipped 2026-10-21/22.** AAG now implements `order`, `rotate`, `weight`
(smooth weighted round-robin, weight 0 = standby), and `usage` (least
*AAG-observed* traffic over a tumbling one-hour window). Spread modes apply
only to queue-built chains; explicit routes and planner-assessed alias chains
keep their order. `usage` is not Magpie's `LeastUsed` over vendor quota — AAG
has no allowance substrate yet, so `pace` and quota-aware `smart` remain
design-only in `docs/reviews/quota_substrate_plan_2026_10_22.md`.

### 3. The usage DAO is written but never called

`src-tauri/src/database/dao/usage.rs` is 512 lines. A grep for callers across
`src-tauri/src/` returns nothing outside the DAO's own tests; `commands/` has no
usage command at all. The UI therefore has no token or cost accounting, though
the tables and queries exist. This is the cheapest win in the whole analysis —
the data layer is done and unwired.

### 4. No OAuth credential reuse

AAG never performs an OAuth flow and never reads a native credential file. Its
`anthropic_passthrough` provider (`proxy/providers/anthropic.rs:84-114`) relays
the *client's own* inbound bearer, and `Claude_mirror_mode::Passthrough`
(`services/provider/claude_mirror.rs:39-41`) declines to write
`ANTHROPIC_AUTH_TOKEN` so a login made inside the mirror passes through. Both
are sound designs that avoid handling credentials at all.

cc-switch implements seven login commands (`commands/auth.rs`,
`commands/codex_oauth.rs`, `commands/copilot.rs`). Magpie goes further: it
reads `~/.claude/.credentials.json` or the macOS Keychain item
(`internal/provider/account.go:427-429`, `:494-512`), refreshes at
`console.anthropic.com/v1/oauth/token`, and writes the whole blob back
untouched; for Codex it reads `auth.json`, refreshes at `auth.openai.com/oauth/token`
with Codex's own `client_id`, and preserves every other field (`account.go:1036-1085`).

This is a deliberate boundary choice for AAG, and the read-refresh-write cycle
is genuinely risky: it mutates the user's credential store on behalf of a
router. Recommend keeping passthrough as the default and treating direct
subscription reuse as a separate, opt-in feature — not a parity checkbox.

### 5. Catalog richness

Magpie reads models.dev (`internal/catalog/catalog.go:1-4`) and joins it with
live vendor `/v1/models` probing. The catalog entry carries release date, price
per million tokens, context and max-context, output cap, reasoning efforts,
input/output modalities, and which APIs serve the model (`catalog.go:32-60`).
Pinned model snapshots and a small builtin list are the fallback.

AAG's `services/model_discovery/` already probes live catalogs and ranks
never-downgrade successors by encoded version — genuinely good work, better
than cc-switch's. It has no price, context-window, or modality metadata, and no
external catalog fallback when a vendor exposes no `/v1/models` (4 of AAG's 9
configured upstreams return 404, per the 2026-09-30 round in `PROGRESS.md`).

**Shipped 2026-10-21.** `services/model_catalog/` merges bounded live
`/v1/models` probing with models.dev facts (context window, prices, release
date, modalities, tool support), with a cached fallback when the public feed is
unreachable. The catalog is presentation and planning metadata, never an
exact-name routing substitute.

### 6. Provider preset breadth

AAG ships 5 presets (`ui/src/lib/provider_presets.ts:22-68`), all
Anthropic-protocol coding endpoints. Magpie ships 83
(`internal/provider/presets.go`), each with regional endpoint variants, optional
header hints, and a key hint. The `PresetDef` struct
(`presets.go:23-60`) is the interesting part — a preset is data, so the picker
needs no per-vendor code.

## Recommended port order

Ordered by value against effort, with each item independently shippable.

| # | Item | Source | Effort | Why now |
|---|---|---|---|---|
| 1 | Wire the usage DAO to IPC + UI | AAG internal | S | 512 lines already written, zero callers | ✅ **IPC shipped 2026-10-14**; **dashboard UI shipped 2026-10-21** |
| 2 | Comment-preserving TOML/JSONC merge | `internal/edit/` | M | Fixes data loss on every Codex switch | ✅ **TOML shipped 2026-10-14**; **JSONC auto-switch path shipped 2026-10-22** (kilo/opencode/copilot writers splice spans instead of re-serializing) |
| 3 | YAML config editing | `internal/edit/yaml.go` | S-M | AAG has no YAML path at all | **Deferred 2026-10-22**: no AAG adapter consumes YAML, so the merge code would have no caller until one ships |
| 4 | Rotate + weighted routing modes | `internal/provider/routing.go` | M | Two cheap modes; no quota substrate needed | ✅ **shipped 2026-10-21** (`order`/`rotate`/`weight`/`usage`; Magpie `pace` and quota-aware `smart` still need 8) |
| 5 | Catalog metadata (context, price, modalities) | `internal/catalog/` | M | Enables 6; fixes the 4 upstreams with no `/v1/models` | ✅ **shipped 2026-10-21** (`services/model_catalog/` merges live `/v1/models` probing with models.dev facts) |
| 6 | models.dev fallback catalog | `internal/catalog/live.go` | S | Depends on 5 | ✅ **shipped 2026-10-21** (bounded `https://models.dev/api.json` fetch with cached fallback) |
| 7 | Preset catalog expansion | `internal/provider/presets.go` | M | 5 → ~40 presets, data-only | ✅ **shipped 2026-10-21** (5 → 31 presets + searchable category picker) |
| 8 | Quota-aware scheduling | `routing.go` + `fallback.go` | L | The deepest differentiator; needs 4 | **Planned 2026-10-22**: design in `docs/reviews/quota_substrate_plan_2026_10_22.md` |
| 9 | Routing groups | `internal/provider/group.go` | L | Depends on 8 |
| 10 | Subscription OAuth reuse | cc-switch `auth.rs` | XL | Security-sensitive; keep opt-in |

Items 1-3 are self-contained and address real defects. Item 4 is the cheapest
routing win. Items 5-7 build the catalog. Items 8-9 are the substantive
Magpie ideas and should follow the catalog work. Item 10 is a product
decision, not an engineering task. Shipped to date: items 1, 2, 4, 5, 6, and
7 (items 1 and 4-7 on 2026-10-21, item 2's TOML half 2026-10-14 and JSONC
half 2026-10-22). Item 3 stays deferred until an AAG adapter actually reads
YAML. Next up by this ordering: item 8, whose substrate design is in
`docs/reviews/quota_substrate_plan_2026_10_22.md`; item 9 depends on it.

## Explicitly not recommended

- **Forking Magpie wholesale.** 623k lines of Go on Wails v3 against a Rust
  product; the gateway alone would be a rewrite, not a port.
- **Porting the plugin runtime** (`internal/pluginrt`, `internal/plugin`,
  `internal/agentplug`). Three overlapping extension systems, including a Bun
  sandbox for npm plugins. AAG's DSH marketplace already occupies this
  product position.
- **Agent-adapter breadth.** Magpie supports ~100 agents at ~800 LOC each;
  AAG's 8 cover the market that matters, and each new adapter is a maintenance
  commitment against config formats the maintainers can change.
- **Session parsers** (~20 per-agent formats, Magpie `internal/sessions`).
  High churn, low defensibility.
- **Mid-stream failover.** AAG documents this as a non-goal in `DESIGN.md`.
  Magpie does implement it (`internal/gateway/continuation.go`): when a stream
  is cut before a stop signal, it appends the partial reply as an assistant
  prefill and re-asks the same candidate, with exponential backoff and a
  tool-call-begin guard. It is clever and bounded, but it changes request
  semantics mid-flight and is the hardest thing in the whole survey to keep
  correct across four protocols. Keep AAG's documented non-goal.

## Evidence notes

All AAG claims were verified against source on this host, not against
`PROGRESS.md`. `cargo check --locked --offline` passes at `4f02046` (16 s).
No behavior was exercised against a real upstream in this round; claims about
AAG's runtime contracts are read from code and existing test coverage.