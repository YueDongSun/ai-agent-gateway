# `src/util/` — Shared logger

Tiny module-level utility used by every other Node subtree. Levels are
`debug / info / warn / error`; output goes to stdout (info, debug) or stderr
(warn, error) with a millisecond ISO timestamp and an uppercase tag.

## Anchor files

- [`log.ts`](log.ts) — exports `logger`, `setLogLevel`, and the `LogLevel`
  union. `logger.debug / info / warn / error` take a tag, a message, and
  optional structured `extra`.

## Key contracts

- The current log level is module-global. `setLogLevel` mutates it.
- Messages below the current level are dropped.
- `extra` (when provided) is serialized as JSON on the line following the
  message — useful for one-line metrics, never for credentials or live
  configuration content.

## See also

- [Root README](../../README.md).