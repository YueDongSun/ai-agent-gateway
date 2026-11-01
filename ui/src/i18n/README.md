# `ui/src/i18n/` — Locale store and message catalogs

Lightweight i18n with no external dependency. Plain objects for the
catalogs, a React hook for live locale switching. Locale is persisted to
`localStorage` under `aag.locale`; the catalog falls back from the active
locale to English and finally to the message key itself.

## Anchor files

- [`index.ts`](index.ts) — `t(locale, key, vars?)` helper, `useI18n()`
  hook (returns `{ locale, setLocale, t }`), and the `Locale` union
  (`'en' | 'zh'`).
- [`en.ts`](en.ts) — English message catalog. Keys are flat dot-namespaced
  (e.g. `header.gateway_running`, `app.gateway.start`); values support
  `{var}` interpolation.
- [`zh.ts`](zh.ts) — Simplified Chinese catalog. Add keys in lockstep
  with `en.ts`.

## Key contracts

- Adding a key requires updating both `en.ts` and `zh.ts`, including the same
  interpolation variables. `tests/i18n_key_parity.test.ts` enforces both. In all
  build modes, a missing translation falls back to English; the raw key appears
  only when neither catalog defines it.
- `useI18n` writes `lang` on `<html>` so CSS can react (e.g. line-height
  for Chinese).
- `detectInitialLocale` preserves an explicit `localStorage` choice and
  otherwise starts in English, independently of the browser language.
- Locales other than `'en' | 'zh'` are not supported. The `Locale` union
  must be widened before adding a new catalog.
- Page components subscribe through `useCtx`/`useT` from the shared provider;
  they do not create independent locale stores with another `useI18n` call.

Supplier discovery uses `suppliers.discovery.*` for help, catalog health,
preservation notices, stages and result explanations. The help, health and
persisted-report views rerender on EN/CN changes without another backend request.
`supplier_discovery_text.ts` maps recognized backend codes and uses a generic
message for unknown discovery details; supplier names and model IDs stay intact.

## See also

- [`../lib/ctx.ts`](../lib/README.md) — the global `I18nCtx` provider
  that bundles locale, theme, and the `t` function.
- [Root README](../../../README.md).
