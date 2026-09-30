# `src/config/` — Zod schema, JSON persistence, and mutation helpers

The Node runtime's single source of truth is a Zod-validated JSON document at
`~/.ai-agent-gateway/config.json`. This subtree owns the schema, the load /
save round trip, and the in-memory mutation helpers used by the CLI and the
interactive menu.

## Anchor files

- [`schema.ts`](schema.ts) — Zod definitions for `ProviderConfig`,
  `CircuitBreakerConfig`, `RouteHop`, `RouteConfig`, `Profile`, `AgentTarget`,
  and the top-level `AppConfig`. Also exports the `AgentId` enum.
- [`store.ts`](store.ts) — file-system layer. Resolves the config path
  (defaults to `~/.ai-agent-gateway/config.json`, override via
  an absolute, dedicated `AAG_CONFIG_DIR`), reads, writes, and seeds a fresh document with a
  mock provider + route so the gateway works out of the box.
- [`manager.ts`](manager.ts) — `ConfigManager` class. Provides
  `setGateway`, `addProvider`, `updateProvider`, `removeProvider`,
  `setRoute`, `removeRoute`, `setProfile`, etc. Each method returns the
  mutated config so callers can `saveConfig(cfg)`.

## Key contracts

- The on-disk format is JSON. Every load goes through `AppConfig.parse`, so
  schema violations surface immediately.
- The document can contain provider keys and the gateway token. Its directory
  is private before any credential bytes are written: POSIX uses owner-only
  modes (new directory `0700`, files `0600`), while Windows uses a
  no-elevation Windows PowerShell adapter to create and verify a protected
  owner-only DACL. Missing ACL support is a stable fail-closed error.
- An empty `AAG_CONFIG_DIR` uses the default. Relative paths, the process
  working directory, and filesystem roots are rejected before any permission
  or content change. A pre-existing override directory and config file must
  already be private; AAG verifies them without changing their permissions.
  Only the default AAG directory or a newly created override is hardened.
- Saves use a random, exclusive same-directory temporary file, reject
  symlink/reparse paths, preserve tighter existing owner permissions, and
  retain exactly one pre-overwrite `config.json.backup`. Temporary failures
  never overwrite a colliding path.
- `ProviderConfig.type` accepts `openai`, `anthropic`, `gemini`, or `mock`.
- `RouteConfig.alias` is the model name clients request; `hops` is the
  ordered fallback chain.
- All identifiers default to a random six-character hex string so callers
  rarely need to specify them.

The file and its rolling backup remain plaintext at rest. Filesystem isolation
is not encryption and does not protect against malware or another process
already running as the same account. Prefer provider-managed secret references
instead of copied keys where an integration supports them.

## See also

- [`src/gateway/`](../gateway/README.md) — consumes the loaded config.
- [Root README](../../README.md).
