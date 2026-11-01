# Prompt enhancement

Turn a short Chinese requirement into a precise English prompt in two visible
steps, using the models already configured in AAG. The feature is desktop-first;
the Node gateway, CLI, and TUI are not involved and no proxy route changes.

## Flow

```text
short Chinese requirement
        |  stage zh_expand   (Chinese, expanded brief)
        v
complete Chinese brief  <-- editable in the UI
        |  stage en_optimize (English, optimized prompt)
        v
final English prompt
```

Each stage is one IPC call with bounded non-streaming generation and recovery
(`prompt_enhance_run`). The UI owns the sequencing. **Generate English prompt**
starts English optimization immediately after Chinese expansion succeeds; it does
not pause for editing. To review the brief before the first English request,
choose **Expand only**, edit the brief, then choose **Optimize English**. The
English stage can also be re-run independently with the current brief.

One UI task owns both stages. History loading, target/input/output edits and
destination changes are blocked while generation runs; this does not cancel
the backend request. Retry resumes only the failed stage with its captured
brief and workspace context. Editing an idle task invalidates its old Retry
action. An unfinished Chinese result is retained and is not passed to English.
Unsaved template or workspace settings must be saved or reset before leaving.

## Choosing a model backend

| Target | Resolution | Works while the gateway is stopped |
| --- | --- | --- |
| Gateway route alias | The running router: explicit route, then providers advertising the model, then the chat queue or legacy global queue | No |
| Upstream provider + model | That provider, called directly | Yes |

Gateway targets inherit the same hops, failover, and circuit breakers the local
API already serves. A saved valid selection takes precedence; otherwise the UI
selects the first route alias, or the first direct provider/model option when no
route alias is available. A direct provider target skips failover and breakers
and uses the provider's own model id, not a route alias.

## Limits and validation

| Input | Limit |
| --- | --- |
| Source requirement (zh_expand) | 32,000 characters |
| Chinese brief (en_optimize) | 64,000 characters |
| Extra guidance | 1,000 characters |
| Stage template override | 8,192 characters |
| Per-call output budget | 64–8,192 tokens; 1,200 minimum expansion default, 8,192 English default |

Validation happens at the IPC boundary before any upstream call. Empty input,
oversized input, an unknown stage, a blank model, and a disabled or missing
provider all fail with a stable code instead of a silent no-op. A caller-supplied
output budget is clamped to the supported token range rather than rejected;
temperature defaults to 0.6 and is clamped to 0–2.

## Templates

Built-in templates ship in `src-tauri/src/services/prompt/templates.rs`. Both
wrap the source text in `<requirement>` delimiters and instruct the model to
treat that content as data rather than as directives.

The advanced panel on the page edits two `app_meta` settings rows:

- `prompt_enhance_zh_system`
- `prompt_enhance_en_system`

An empty value deletes the row and restores the built-in template. Overrides
take effect on the next stage run — no restart and no migration.

## Data handling

- Generation uses the selected backend. Long or recovered English prompts can also use the running gateway's `alias:haiku` reviewer, with one `alias:opus` escalation if unavailable or inconclusive. No review route is fabricated when the gateway lacks it.
- AAG does not persist stage input or generated response text on its backend.
  After a successful English stage, the UI stores the original input, Chinese
  brief, final prompt, target label, and timestamp in browser `localStorage`,
  capped at eight entries. Custom stage templates remain in SQLite settings.
- The current input, optional instruction, brief, output and completion/error
  state are also retained in this tab's `sessionStorage`, so returning to Prompt
  recovers the draft without carrying it into a coding-client page. A reload
  during generation reports the prior request's outcome as unknown; it does not
  automatically retry or claim backend cancellation. This local draft contains
  task text under the same browser-profile privacy boundary as history.
- Diagnostics record stage id, input/output character counts, elapsed time, and
  an opaque target reference. Prompt text, response text, credentials, and
  upstream URLs are never logged.
- Errors cross IPC as stable codes. An upstream failure is reported as
  `prompt_enhance_upstream_failed:<code>:<status>`; the upstream message is
  never forwarded.

## Evidence boundary

Rust unit tests cover validation, template overrides, delimiter composition,
output normalization, both target modes, and the stopped-gateway path using the
built-in mock provider. Browser smoke covers the page in demo mode with
synthetic IPC. Neither is evidence of a real upstream model answering: that
requires a configured provider and an authorized live request.

## Automatic budgeting, continuation and review

The former implementation rejected `finish_reason=length` before extracting
visible text. A fixed global budget and a narrow MiniMax effort adjustment could
therefore leave the user with only a generic output-limit error.

The stage now uses provider-scoped configured capabilities and cached public
model metadata to cap output and reserve input context. ASCII and non-ASCII
input estimates are conservative estimates, not exact tokenization. Unknown
limits keep a bounded policy default rather than inventing model capabilities.
A small fallback cannot lower the capacity of a capable primary; dispatch still
applies its request capability checks. User-specified token caps remain per-call
ceilings. Expansion budgets scale with source size.

Length termination and an unfinished code fence trigger continuation with the
original source, retained assistant text and a request for only missing content.
Exact overlap is removed without fuzzy rewriting. A repeated fragment advances a gateway request to the next configured,
capability-compatible candidate within the same three-attempt budget. Candidate
selection and dispatch reuse the captured router; shared routes are not mutated.
Exact-model requests remain on that model, and explicit provider selections do
not switch providers. If no eligible alternative remains, the loop stops. Missing terminal metadata is treated as incomplete. Filter/tool termination,
transport failure, context exhaustion and budget exhaustion remain distinct.
Only visible assistant content is retained; reasoning is excluded.

Each stage permits at most three generation attempts, six calls including review
and correction, and 32,768 allocated output tokens; each call has a 90-second
deadline. A completed short prompt does not incur a separate reviewer request.
A long or recovered English prompt receives a structured review through the
available Haiku route; invalid/inconclusive review can escalate once to Opus.
A concrete correction receives one rewrite. Failed correction preserves the
complete first draft and reports that review could not finish. Review output
is untrusted data and cannot override original constraints or system rules.

IPC returns `complete`, `failureCode`, `attempts`, `reviewStatus` and cumulative
reported usage. Partial generation returns `complete=false` with retained text;
the UI displays it, labels failure, and does not automatically pass an unfinished
Chinese brief to the English stage. Completed history remains separate from
partial results. Recovery is automatic within the stage; current UI progress
identifies preparation/checking but does not expose token accounting internals.

This pipeline explicitly requests non-streaming completions. It does not claim
to recover interrupted SSE transport; canonical partial responses and explicit
length termination are covered. Provider-specific errors without trustworthy
limit metadata are not guessed into a new capability value. Public metadata
and synthetic transport tests do not establish real-provider success.
