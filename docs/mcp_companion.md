# AAG MCP companion and feature-aware verification

## Two separate MCP surfaces

The desktop **MCP Tools** page manages definitions in the existing AAG SQLite
registry. It provides reviewed Claude Code configuration writes and redacted
Codex previews. See [MCP management](mcp_management.md).

The bundled **AAG companion** is an independently runnable, read-only stdio MCP
server. It helps a coding agent discover AAG workflows and choose the appropriate
verification commands. It does not connect to the desktop database or control the
application. These surfaces deliberately do not share write authority.

## Start the companion

Prerequisite: Node.js 20 or newer and a trusted checkout of this repository. No
additional packages, account, credentials, listener, or background service are
needed. Configure an MCP client with an absolute executable/script path:

```json
{
  "mcpServers": {
    "aag_companion": {
      "command": "node",
      "args": ["/absolute/path/to/aag/scripts/mcp/aag_mcp.mjs"]
    }
  }
}
```

On Windows use forward slashes or JSON-escaped backslashes. Select the Node
executable explicitly when the client has a different PATH. This example is a
template, not an installed configuration. In AAG, add the inner server definition,
use an ID such as `aag_companion`, then review the client sync. AAG-managed Claude
keys have an additional `aag_` namespace prefix.

Use the direct `node` command in MCP client configuration. `npm run mcp:serve`
is a manual convenience; npm's ordinary banner is not protocol output. If npm is
used by a client, it must be run with `--silent`.

## Tool contract

| Tool | Input | Result | Authority |
| --- | --- | --- | --- |
| `aag_capabilities` | Empty object | Tools, feature scopes, prohibited actions | Read-only static metadata |
| `aag_verification_plan` | `scope`: `node`, `ui`, `mcp`, `dsh`, or `all` | Ordered commands, isolation, exclusions | Plan only |
| `aag_workflow` | `topic`: `mcp`, `dsh`, or `verification` | Steps and safety boundaries | Read-only static guidance |

All three tools reject unknown arguments. No tool accepts a shell command, file
path, URL, credential, approval flag, or client configuration. No plan can execute
or approve itself. Source modules and the CLI share one command catalog in
`scripts/verification/feature_catalog.mjs`; the companion never imports the
execution module. Static guidance is not a health check or proof of installation.

The server implements newline-delimited JSON-RPC over stdio, initialization and
ready-state gating, ping, tool discovery, and tool calls. It supports protocol
versions `2025-11-25`, `2025-06-18`, and `2025-03-26`, negotiating `2025-11-25`
when a client proposes another version. It does not claim support for a newer
revision, HTTP transport, sampling, prompts, resources, or long-running tasks.
Requests are limited to 64 KiB. Invalid JSON, oversized messages, unknown methods,
invalid arguments, and early calls produce bounded protocol errors without
echoing input contents. EOF releases the process; stdout contains protocol only.
No per-request audit payload is persisted.

The machine-readable safety contract is
[aag_companion_contract.json](contracts/aag_companion_contract.json). Provenance
warnings about intentionally disabled actor/input/artifact recording are expected:
the companion is stateless and keeps no request log.

## Verification automation

Inspect a plan without executing anything:

```text
npm run verify:features -- --scope mcp
```

Execute the reviewed local gate:

```text
npm run verify:features -- --scope all --run
```

The runner uses fixed executable/argument arrays, never a shell command string.
It creates disposable `AAG_CONFIG_DIR` and `AAG_HOME_DIR`, removes the live-database
test opt-in, builds the native frontend before Cargo, and runs Cargo locked,
offline, and serially. It stops on the first failed/unavailable/timed-out check,
lists unrun checks, and emits a structured receipt. A stage has a 15-minute limit;
SIGINT/SIGTERM requests cancellation. No packages are installed automatically.

The command catalog covers Node tests, automation/stdio tests, desktop runner
checks, DSH transport/catalog tests, both frontend builds, both owned browser
smokes, local release preflight, and Rust checks/tests as appropriate to the
selected scope. `all` is the cross-tree gate. Individual scopes are focused
checks, not an automatic proof that unrelated modules are unaffected.

This runner executes **trusted repository tests**, not arbitrary untrusted code.
Passing means the foreground command exited successfully. Detached descendants
are not universally contained or verified, particularly on Windows; each test
owns its own background resources. Receipts state that limitation explicitly.
The browser smoke separately verifies its owned browser/server/profile cleanup.
Never interpret temporary configuration isolation as an OS process sandbox.

Real providers, live client switching, DSH package installs, native installation,
upgrades, publication, and recurring jobs remain excluded. No scheduled task is
created by this feature.

## Tests and sources

```text
npm run test:automation
npm run ui:smoke
```

The tests exercise an actual stdio child, malformed/oversized message recovery,
initialization, unsafe argument rejection, shared plans, foreground cancellation,
timeout, fail-fast behavior, and configuration isolation. Desktop MCP file safety
has separate Rust tests and the GUI has synthetic IPC tests.

External contracts were checked against the primary specifications:
[MCP lifecycle](https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle),
[stdio transport](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports),
and [MCP tools](https://modelcontextprotocol.io/specification/2025-11-25/server/tools).
The unified registry workflow is inspired by
[CC Switch](https://github.com/farion1231/cc-switch), without copying its private
runtime state or enabling its integrations automatically.
