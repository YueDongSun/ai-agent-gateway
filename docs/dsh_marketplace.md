# DSH plugin marketplace

The DSH page now includes a **Plugin marketplace** button. In Tauri it opens
a dedicated bundled window; repeated clicks restore and focus the same window
without resetting its filters. Browser demo mode opens a separate same-origin
tab. Opening the market does not require a running DSH instance and does not
install, enable, or execute any plugin.

## Browse and inspect

The market has independent EN/CN and light/dark controls, search, function
categories, practical-use filters, star/name/updated sorting, and twelve-card
pagination. Function categories include personalization, development, agents,
models, knowledge/data, integrations, media, security, and observability.
Use filters include coding, automation, research, model/tool connections,
desktop/WebUI work and Claude integration.

The category/use labels are bilingual. Upstream descriptions are shown in the
selected language when the source provides that language; otherwise the UI
identifies the original description rather than presenting an invented
translation. Classification is an inference from public metadata and keyword
rules. It does not establish a plugin's actual compatibility, safety or quality.

Each detail drawer exposes:

- repository and author;
- package/version and non-executable installation reference, if supplied;
- source-reported license and update/star metadata;
- the individual plugin revision supplied by the registry;
- the separate registry revision and collection time;
- links to the repository, plugin revision, pinned registry JSON and upstream
  collection script.

Source and registry revisions are separate facts. A registry commit must never
be substituted for a plugin commit. Registry inclusion or upstream verification
labels are not an AAG security audit. The market deliberately does not expand
the existing managed installer allowlist.

## Real catalog and provenance

The bundled normalized snapshot currently contains **8,874 entries** from
[YELEBAI/dsh-plugin-marketplace](https://github.com/YELEBAI/dsh-plugin-marketplace).
It was collected from registry commit
33af32809ed33ee0588ab2acac454a0d8c4a4de3 at
2026-08-30T13:27:51.996Z. Individual source revisions come from that registry,
not from separately running every repository or reviewing its code.

The upstream [collector](https://github.com/YELEBAI/dsh-plugin-marketplace/blob/33af32809ed33ee0588ab2acac454a0d8c4a4de3/scripts/registry-scan.mjs)
discovers GitHub topic candidates and maintains its registry.
AAG reads the published output; it does not execute upstream collectors,
package scripts, YAML expressions or plugin code.
The normalized data is redistributed with the
[upstream MIT notice](../scripts/dsh_market/source_notice.txt).

AAG's [collector](../scripts/dsh_market/collect_catalog.mjs) resolves the current
registry branch to a commit and downloads the registry by that exact commit.
[Shared classification rules](../scripts/dsh_market/classification_rules.json)
are used by the Node collector and Rust refresh adapter. The generated snapshot
is [shared/dsh_market_catalog.json](../shared/dsh_market_catalog.json).

## Refresh and offline behavior

Opening the market reads a validated cached catalog or the bundled snapshot.
It does not initiate a network scan or create a cache. **Refresh catalog** is
an explicit network action. The native service performs only two allowlisted
GitHub GET requests, with redirects disabled, a thirty-second total deadline,
a 24 MiB input limit, and a 20,000-entry limit.

The service validates the replacement before publishing it. A failed network,
rate limit, malformed response or invalid catalog does not replace the last
good cache. The UI keeps the existing list and shows the error. Invalid local
cache data falls back to the bundled snapshot with a warning. The cache is
separate from DSH runtime configuration.

Browser demo mode reloads the real bundled snapshot without simulating a fresh
network collection. The demo notice and collection date remain visible.
No recurring job, GitHub workflow or background polling was installed.

Rebuild the repository snapshot explicitly:

~~~text
npm run dsh:market_collect
npm run test:dsh_market_catalog
~~~

The collector accepts only its documented data inputs and trusted source
configuration. It does not accept shell fragments from registry records.

## Window and permission boundaries

The native market window is labeled dsh_market and loads the bundled
index.html?view=dsh_market entry. It does not mount the main provider/gateway
shell, start gateway status polling, or enable global frontend diagnostics.
It can call exactly these application commands:

~~~text
dsh_market_get_catalog
dsh_market_refresh
dsh_market_open_source
~~~

The main window additionally owns dsh_open_marketplace. The market has no
provider, credential, runtime-control, settings-write, plugin-install or broad
core permissions. Both the Tauri capability file and command dispatch enforce
the distinction. DSH runtime WebUI windows still have no AAG command authority.

Navigation stays on the owned application origin and market route. Native
popups/downloads are denied. Source-opening requests pass a catalog entry ID
and source kind to Rust; the UI cannot hand arbitrary URLs to an OS shell.
Resolved links must be HTTPS GitHub source URLs. Browser-demo anchors apply
the same host/scheme checks and use noopener/noreferrer.

## Modules

- src-tauri/src/services/dsh/market_catalog.rs: validated cache and explicit
  refresh orchestration.
- market_normalize.rs and market_tests.rs: schema, source and classification
  contracts.
- src-tauri/src/commands/dsh_market.rs: narrow IPC surface.
- src-tauri/src/dsh_market_window.rs: singleton market window, navigation and
  source-link handoff.
- ui/src/components/dsh_market/: independent storefront and provenance drawer.
- ui/src/lib/api/dsh_market.ts: typed contracts.
- ui/src/lib/mock/dsh_market.ts: lazy demo snapshot loading.
- scripts/dsh_market/: collector, fixtures, tests, source notice and native test.

The approximately 8.3 MB generated browser snapshot chunk is lazy-loaded only
when the market is opened; the initial frontend bundle remains approximately
198 KB. This is a complete offline catalog, not an initial-page dependency.
Pagination bounds rendered cards, but parsing/search still uses the full
catalog. The build size warning is retained rather than hidden.

## Verification

See [marketplace validation](dsh_marketplace_validation.md) for fresh results.
The existing browser suite retains its original DSH and gateway checks and adds
market-specific checks. A separate opt-in Windows test checks the actual Tauri
button, distinct native window, singleton reuse, language switching and denied
credential/execution commands using disposable application/WebView2 state.

The in-app Browser tool could not initialize because the host's Windows sandbox
helper failed before browser startup. Project-owned automated browser tests and
screenshots are recorded as that evidence path; they are not presented as a
successful in-app Browser session.
