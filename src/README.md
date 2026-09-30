# `src/` — Node compatibility surface

The Node gateway, CLI / TUI, JSON configuration store, and legacy agent
adapters. This tree stays independently runnable so headless users can use AAG
without installing the Tauri runtime. It does **not** depend on `src-tauri/`
or `ui/`; conversely, those trees do not import from here at runtime.

## Anchor files

- [`index.ts`](index.ts) — CLI entry point. Selects the interactive TUI when
  no subcommand is given; otherwise dispatches to Commander handlers in
  `cli/`.
- [`cli/`](cli/README.md) — Commander command implementations and the
  interactive menu.
- [`config/`](config/README.md) — Zod schemas, JSON persistence, and mutation
  helpers. The on-disk file lives at
  `~/.ai-agent-gateway/config.json` (override with `AAG_CONFIG_DIR`).
- [`gateway/`](gateway/README.md) — Hono HTTP server, canonical OpenAI format,
  routing, providers, conversion, SSE, and circuit breakers.
- [`agents/`](agents/README.md) — agent config adapters and the gateway /
  direct / profile switcher that rewrites live agent config files.
- [`util/`](util/README.md) — shared logger.

## Key contracts

- TypeScript is strict ESM with explicit `.js` extensions in relative imports.
- The canonical internal request and response shape is OpenAI Chat Completions.
  Anthropic conversion happens at the HTTP edge and inside the Anthropic
  provider.
- Router failover advances only for network errors, HTTP 429, and HTTP 5xx.
  Streaming failover probes the first chunk before committing; later stream
  errors are not transparently recovered.
- Agent config writes must back up the previous file content so `pointToGateway`
  and `pointToProvider` can be reverted.

## See also

- [docs/architecture.md](../docs/architecture.md) — Node surface ownership.
- [docs/development.md](../docs/development.md) — install and run commands.
- [Root README](../README.md).