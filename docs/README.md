# `docs/` — Project documentation

Authoritative project documentation for AI Agent Gateway (AAG). Module
ownership, data flow, persistence, verification, and release notes. Use this directory to find the doc that
answers a specific question.

## Anchor files

- [`architecture.md`](architecture.md) — current module ownership, data
  flow, persistence, and compatibility boundaries. The single page to
  open first when you need to know who owns a slice of behavior.
- [`development.md`](development.md) — setup, npm scripts, the complete
  verification gate, and the release boundary.
- [`interface_qa.md`](interface_qa.md) — workspace navigation, header surface,
  provider search / disclosure, dialog behavior, and the visual QA checklist.
- [`cli_provider_boundary.md`](cli_provider_boundary.md) — separation between
  model-only client views and supplier-owned client management.
- [`unified_suppliers.md`](unified_suppliers.md) — normalized supplier and
  endpoint models, per-source dual-document URL discovery with EN/CN explanations, capability routing,
  cc-switch import, and hot-apply behavior.
- [`codex_session_authority.md`](codex_session_authority.md) — Codex CLI session
  authority, home isolation, and protected credential boundaries.
- [`tailnet_publication.md`](tailnet_publication.md) — Tailscale Serve
  publication contracts, local port forwarding, and lifecycle management.
- [`desktop_release.md`](desktop_release.md) — desktop build pipeline,
  CI build matrix, signing / notarization, manual acceptance, preview
  publication, and rollback.
- [`public_release_privacy.md`](public_release_privacy.md) - privacy gates,
  history-free export, public-manifest verification, and new-repository
  publication boundary.
- [`project_structure.md`](project_structure.md) — directory map,
  ownership rules, UI state boundaries, build boundaries, and test
  boundaries.
- [`platform_launchers.md`](platform_launchers.md) — platform discovery,
  terminal/editor launch contracts, isolated profiles, and native acceptance
  boundaries.
- [`prompt_enhance.md`](prompt_enhance.md) — two-stage prompt enhancement,
  backend selection, local history, and IPC error contracts.
- [`mcp_management.md`](mcp_management.md) — registry validation, redacted
  previews, and reviewed client synchronization.
- [`mcp_companion.md`](mcp_companion.md) — read-only stdio companion,
  feature-aware verification plans, and the isolated foreground runner.
- [`releases/`](releases/README.md) — per-release notes. Today:
  [`v0_2_0.md`](releases/v0_2_0.md).


- [DSH plugin management research](dsh_plugin_management_research.md) —
  source-backed proposal for personalization, plugin management, one-click
  setup, and desktop integration; preserved proposal with deferred capabilities.
- [Managed DSH integration](dsh_management.md) — current implementation, setup,
  native Claude authentication, runtime ownership and security boundaries.
- [DSH validation evidence](dsh_validation.md) — tests and remaining acceptance limits.

- [DSH plugin marketplace](dsh_marketplace.md) — bilingual discovery window,
  source provenance, explicit collection and narrow native permissions.

- [DSH plug-and-play setup](dsh_plug_and_play.md) — passive installation discovery,
  current-provider setup, deterministic dispatch and thinking/stream fixes.
- [Plug-and-play validation](dsh_plug_and_play_validation.md) — fresh native,
  browser, real-DSH and protocol evidence.

- [Workflow management](workflow_management.md) — read-only capability
  registry, route-preview contract, private configuration, and the local read
  facade.

## Key contracts

- `architecture.md` is the canonical reference for "who owns this code".
  When it disagrees with the code, the code wins and the doc must be
  updated.
- Release notes describe what changed in a specific release plus the
  verification evidence; the `desktop_release.md` page owns the
  cross-cutting release pipeline.

## See also

- [`../README.md`](../README.md) — product overview.
- [`../DESIGN.md`](../DESIGN.md) — durable design decisions.
