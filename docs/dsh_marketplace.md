# DSH plugin marketplace

The DSH page now includes a **Plugin marketplace** button. In Tauri it opens
a dedicated local window; repeated clicks restore and focus the same window
and fetch the latest catalog without resetting its filters. Browser demo mode opens a separate same-origin
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
- the feed update and fetch times, with missing revisions marked unknown;
- links to the repository, supplied plugin revision and live marketplace feed.

Source and registry revisions are separate facts. A registry commit must never
be substituted for a plugin commit. Registry inclusion or upstream verification
labels are not an AAG security audit. The market deliberately does not expand
the existing managed installer allowlist.

## Live catalog and provenance

Every standalone marketplace open requests
[the public dsh.market feed](https://dsh.market/plugins.json), the same endpoint
used by [dsh.market](https://dsh.market). The native adapter performs the request
through Rust; the browser adapter uses credential-free browser fetch. Neither
runtime imports a bundled catalog for display. Entry counts, classifications,
search results and sorting are recomputed from each validated response using
the [shared classification rules](../scripts/dsh_market/classification_rules.json).

The UI shows both the feed's `generatedAt` and the actual fetch time. The site
does not provide a registry commit, so that revision remains explicitly unknown.
Plugin revisions are shown only when the feed supplies them. Canonical GitHub
repository links are derived from validated repository identities; AAG does not
execute installation commands, readmes, collectors or plugin code.

## Refresh and failure behavior

Opening a new window, reopening the existing native window, retrying a failed
initial load and clicking **Refresh catalog** all request the live feed.
Concurrent requests in one frontend are coalesced; completed responses are not
reused on the next open. Both adapters enforce a thirty-second deadline,
a 64 MiB response limit and a 20,000-entry limit. The native adapter permits
only the exact marketplace endpoint and disables redirects.

Initial load failures show an error and retry action without displaying an old
bundled list. If an already-open window has a successful catalog, a failed
refresh keeps that list visible with an error. The native cache stores only
validated successful responses, separately from DSH runtime configuration.
Source links resolve from the current in-memory response even if saving the
disk cache fails; cache reads do not replace the live open request.

Browser demo mode also fetches the live public feed. Desktop-only actions remain
simulated in that mode. Browser regression tests inject responses at the fetch
boundary to verify updates and failures independently of public network uptime.
No recurring job or background polling is involved.

The historical repository snapshot and its maintainer collector remain separate
from the displayed marketplace. To maintain that optional artifact explicitly:

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
Resolved links must be HTTPS GitHub source URLs or the exact public marketplace
feed URL. Browser-demo anchors apply
the same host/scheme checks and use noopener/noreferrer.

## Modules

- src-tauri/src/services/dsh/market_catalog.rs: bounded live requests and
  validated cache persistence.
- market_normalize.rs and market_tests.rs: schema, source and classification
  contracts.
- src-tauri/src/commands/dsh_market.rs: narrow IPC surface.
- src-tauri/src/dsh_market_window.rs: singleton market window, navigation and
  source-link handoff.
- ui/src/components/dsh_market/: independent storefront and provenance drawer.
- ui/src/lib/api/dsh_market.ts: typed contracts.
- ui/src/lib/api/dsh_market_config.ts: public source and request limits.
- ui/src/lib/api/dsh_market_normalize.ts: pure feed validation and classification.
- ui/src/lib/api/dsh_market_feed.ts: bounded credential-free browser requests.
- ui/src/lib/mock/dsh_market.ts: browser-demo dispatch to the live feed adapter.
- scripts/dsh_market/: collector, fixtures, tests, source notice and native test.

The frontend bundle contains presentation code and classification rules. Catalog
entries arrive through the live request when the marketplace opens. Pagination
bounds rendered cards, while validation, parsing and search use the full
bounded response. No generated offline catalog chunk is loaded for display.

## Verification

The [historical marketplace validation](dsh_marketplace_validation.md) records
the earlier bundled-catalog implementation and is not current live-feed evidence.
The existing browser suite retains its original DSH and gateway checks and adds
market-specific checks. A separate opt-in Windows test checks the actual Tauri
button, distinct native window, singleton reuse, language switching and denied
credential/execution commands using disposable application/WebView2 state.

The in-app Browser tool could not initialize because the host's Windows sandbox
helper failed before browser startup. Project-owned automated browser tests and
screenshots are recorded as that evidence path; they are not presented as a
successful in-app Browser session.
