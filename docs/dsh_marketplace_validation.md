# DSH marketplace validation

Date: 2026-08-30. Status: **READY_FOR_REVIEW**, local implementation.
Baseline: master at 1fa8e287b9d5eb9926948b0c7bc321de5659656d plus the existing
uncommitted DSH implementation and this marketplace slice. No commit, push,
release, scheduled job, live DSH configuration change or plugin installation
was performed for this slice.

## Fresh results

| Check | Result | Boundary |
| --- | --- | --- |
| Node typecheck | Passed | Independent headless runtime remains type-correct |
| npm test | 28 passed | Existing Node regressions with disposable configuration |
| npm run test:desktop_runner | 7 passed | Desktop startup/build configuration |
| npm run test:dsh_market_catalog | 11/11 passed | Collector, provenance, classifications and hostile-input contracts |
| npm run test:dsh_transport | 2/2 passed | Real owned transport plus exact native permission coverage |
| UI typecheck and native build | Passed | Market route and current native frontend assets |
| npm run ui:smoke | 94/94 passed | Source-served synthetic IPC; previous 74 checks retained |
| npm run ui:smoke_demo | 94/94 passed | Built demo; same full check set |
| Targeted Rust market tests | 11 passed, 1 ignored | Eight catalog checks and three window/authority checks |
| Explicit GitHub refresh acceptance | 1/1 passed, 25.12 s | Two actual public GitHub GETs and temporary cache readback |
| cargo build --locked --offline --bin ai-agent-gateway | Passed | Current Windows native executable |
| npm run test:dsh_market_native | 11/11 passed | Actual Tauri/WebView2 windows and native IPC |
| cargo check --locked --offline | Passed | Current Rust source and generated native permissions |
| Full serial Rust suite | 176 passed, 0 failed, 4 ignored; 12.99 s | Current integrated source, disposable configuration |
| Release preflight | Passed | Existing version/asset gate, not installation/signing |

The normal Rust suite leaves four explicit integration tests ignored:
the real-provider canary, prior DSH-runtime acceptance, prior pnpm acceptance,
and public GitHub catalog refresh. The new GitHub refresh test was separately
executed in this slice. Prior-turn acceptance remains recorded in
[dsh_validation.md](dsh_validation.md), not represented as a fresh run here.

## Catalog identity

- Source registry: YELEBAI/dsh-plugin-marketplace.
- Registry revision: 33af32809ed33ee0588ab2acac454a0d8c4a4de3.
- Collected at: 2026-08-30T13:27:51.996Z.
- Normalized entries: 8,874.
- Normalized file size: 11,102,557 bytes.
- Function categories: ten; use-case filters are independent, with multiple
  labels allowed per entry.
- Classifications are derived from metadata rules. Source commits and licenses
  are registry-reported metadata, not independently audited plugin behavior.

The collector resolves the branch once, downloads the immutable revision and
validates all records before replacement. Tests reject duplicate IDs, hostile
URLs, malformed/empty/oversized feeds, invalid UTF-8 and arbitrary source
installation instructions. Failed refresh preserves the previous catalog.
The real network acceptance used only a temporary cache and cleaned it up.

## Browser and visual coverage

The twenty added market cases cover actual new-tab creation, noopener,
independent page initialization with no provider/gateway calls, both languages,
function/use-case intersections, search, empty results, sorting and pagination,
source/registry SHA separation, refresh failure and recovery, malicious metadata
as inert text, keyboard focus restoration and light/dark layouts at
1200, 900 and 420 pixels.

The existing startup delay was replaced with bounded actual UI readiness.
The market popup also waits for its new document, rather than accepting a target
whose URL changed while its runtime is still about:blank. This fixes timing
assumptions without relaxing the expected UI or permission behavior.
A real detail-focus restoration defect under StrictMode was fixed and rechecked.

The standalone snapshot is lazy-loaded: approximately 8.3 MB generated JS
(about 1.5 MB gzip), separate from the approximately 198 KB initial bundle and
32 KB market UI. The size warning was retained. The complete catalog is not
rendered at once; the grid contains twelve entries per page.

Actual screenshots were inspected for layout, text clipping and theme contrast:

- tmp/dsh_market_light.png: English, light theme, full desktop page.
- tmp/dsh_market_dark.png: Chinese, dark theme, full desktop page.
- tmp/dsh_market_mobile.png: Chinese, 420-pixel detail drawer.
- tmp/dsh_market_native.png: actual native WebView2 market viewport.

The in-app Browser kernel failed before browser initialization with the host's
Windows sandbox ACL helper error. Computer Use uses the same unavailable
JavaScript kernel. The evidence above comes from project-owned browser/native
test processes and screenshots, not a claimed successful Browser tool session.

## Actual Tauri acceptance

[scripts/dsh_market/native_window_smoke.mjs](../scripts/dsh_market/native_window_smoke.mjs)
requires Windows and a successfully built binary, refuses to run beside another
AAG process, and uses fresh AAG configuration/home and WebView2 profile paths.
The process starts tray-only, without DSH or a provider.

The final native test verified:

1. The application is using native IPC, not the demo backend.
2. Visiting DSH does not automatically open the market.
3. Clicking the actual DSH button creates a separate native WebView.
4. That WebView renders the actual bundled catalog.
5. Provider credential reads receive an explicit permission denial.
6. Plugin execution receives an explicit permission denial, rather than an
   unrelated missing-preview error.
7. Source opening reaches the validated entry-ID contract.
8. The market switches to Chinese.
9. Repeated opening reuses one window.
10. Reopening preserves the market's language state.
11. There are no native market render exceptions.

The test application and profile were closed and removed. An initial native
test was mistakenly started before the build had finished, against a stale
executable; it timed out and temporarily blocked executable replacement.
That attempt is excluded from acceptance. After cleanup, the build completed
successfully and the final native test passed, including a second run with
stricter permission-error assertions.

## Final boundaries

Browsing/refreshing does not authorize or perform plugin installation, source
execution, live provider calls, or automatic changes to existing DSH/Claude
configuration. The market's source links do not certify compatibility or safety.
No broad native capabilities were granted to the marketplace or runtime WebUI.

The repository-specific release inspector is unavailable. Native Git review,
whitespace/ignore checks, source-level credential/private-path pattern review,
the existing release preflight and the test results above provide local review
evidence. They are not a third-party dependency security audit.

Not tested here: macOS/Linux native windows, installer upgrades/signing,
actual execution of catalog plugins, or automatic translation of source
descriptions that do not include the selected language.


Final hygiene: 70 changed/new files were scanned for the targeted credential
and private-path patterns; zero finding files. Git whitespace checks passed.
The native acceptance left zero AAG processes. Generated permission files and
screenshots remain ignored; the public catalog and MIT attribution are intended
source artifacts.
