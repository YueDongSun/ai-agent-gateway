# `src/cli/` — Commander commands and interactive menu

The CLI surface for the Node runtime. Subcommands start the gateway, switch
an agent, report status, list models, run mock tests, or print the config
file path. When no subcommand is given, the user is dropped into the
`interactiveMenu()` flow.

## Anchor files

- [`commands.ts`](commands.ts) — every Commander handler
  (`startGatewayCmd`, `switchCmd`, `statusCmd`, `modelsCmd`, `testCmd`,
  `configPathCmd`). Each loads the JSON config, performs the action, and
  saves back to disk.
- [`menu.ts`](menu.ts) — `interactiveMenu()` using `@inquirer/prompts`.
  Renders the top-level menu and dispatches to the same handlers as the
  Commander tree, but driven by interactive selections.

## Key contracts

- The CLI name is `aag` (root `package.json` `bin.aag`).
- All subcommands preserve the existing config file and append rather than
  overwrite.
- The `--to` flag accepts `gateway | direct | profile`. `direct` requires
  `--base-url` and `--api-key`; `profile` requires `--profile`.

## See also

- [`src/index.ts`](../README.md) — entry point that wires the Commander
  program and falls back to `interactiveMenu()`.
- [Root README](../../README.md).