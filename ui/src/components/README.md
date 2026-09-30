# `ui/src/components/` — Reusable components and the per-app page subtree

Shared icons, reusable widgets (secret input, model suggest, release
badge), and the per-app page subtree under
[`app/`](app/README.md). These components are consumed both by the Tauri
desktop build and the browser demo.

## Anchor files

- [`Icons.tsx`](Icons.tsx) — stroke-only inline SVG icons. `BrandLogo`,
  `ClaudeIcon`, `CodexIcon`, `CopilotIcon`, `SunIcon`, `MoonIcon`,
  `CopyIcon`, `TestIcon`. All icons are 16×16 with `currentColor`
  stroke; sizing follows `font-size`.
- [`ModelSuggest.tsx`](ModelSuggest.tsx) — portal-rendered model name
  picker. Plain controlled `<input>` plus a chevron button. Menu rendered
  via `createPortal(document.body)` so it is immune to table / sticky
  toolbar clipping.
- [`release_badge.tsx`](release_badge.tsx) — version + build-channel
  badge. `data-testid="build_version"` and `data-testid="build_channel"`
  for smoke tests.
- [`secret_value.tsx`](secret_value.tsx) — masks a secret by default
  with `••••••••`, supports explicit reveal / hide, and resets on value
  or application changes. Copy receives the original value.
- [`diagnostics_modal.tsx`](diagnostics_modal.tsx) — global local-log status,
  temporary Debug controls, directory opening, and safe summary copying.
- [`error_boundary.tsx`](error_boundary.tsx) — React render-failure fallback
  and sanitized native error reporting.
- [`app/`](app/README.md) — the per-app page subtree (Claude, Codex,
  Copilot, OpenCode, Kilo, VS Code variants) plus the shared
  `GatewayPanel`, `ProvidersPanel` / `ProviderCard`, `AdvancedRoutes` /
  `RouteEditor`, `Launchers`, and the split providers / routes state
  contexts.

## Key contracts

- Components in this directory are presentation-only. Persistence and
  business logic live in the page that owns the component or in
  [`hooks/`](../hooks/README.md) / [`lib/api/`](../lib/api/README.md).
- All icons forward `className` and `style` so callers can size with
  `font-size` and override `stroke-width` / `opacity`.

## See also

- [`app/`](app/README.md) — per-app page subtree.
- [`../i18n/`](../i18n/README.md) — message keys referenced by these
  components.
- [Root README](../../README.md).
