# `src/agents/` — Agent config adapters and switcher

Each supported AI coding tool (Copilot CLI, Kilo, Cursor, Zcode, MiniMax
Code, Kimi Code, Pi, Antigravity) has its own config-file shape. This
subtree owns detection of where each agent keeps its config, the JSON /
JSONC builders that point the agent at the local gateway or a direct
provider, and the `Switcher` that orchestrates those rewrites.

Kilo uses the OpenCode-compatible JSONC provider projection: the adapter
merges provider.aag and the selected model into the global Kilo config,
while preserving other providers, small_model, and unrelated user settings.

The desktop app has its own adapter family under
[`src-tauri/src/services/provider/`](../../src-tauri/src/services/provider/README.md).
The Node adapters here remain available for the legacy CLI / TUI.

## Anchor files

- [`adapters.ts`](adapters.ts) — defines `defaultAgentTargets()` for every
  supported agent, the `AgentAdapter` interface, and a generic JSON adapter.
  Concrete per-agent JSON builders live in this file.
- [`switcher.ts`](switcher.ts) — `Switcher` class. Calls
  `pointToGateway`, `pointToProvider`, and `pointToProfile`; preserves the
  previous file content for rollback.

## Key contracts

- `AgentId` is the closed enum in [`config/schema.ts`](../config/schema.ts).
- `pointToGateway` builds a content string that points the agent at
  `http://<cfg.gateway.host>:<cfg.gateway.port>` with the gateway auth token
  and writes it atomically.
- `pointToProvider` writes a content string targeting the supplied base URL
  and API key directly.
- `pointToProfile` writes a content string referencing a named profile from
  the AAG config (used when an agent supports profile indirection).
- The first existing config path under `target.configPaths` wins; otherwise
  the first path is created on demand.

## See also

- [`src/cli/commands.ts`](../cli/README.md) — `switch` subcommand wiring.
- [Root README](../../README.md).
