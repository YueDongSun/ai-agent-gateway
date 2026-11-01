/// Prompt-enhancement pipeline IPC. Mirrors Rust src-tauri/src/services/prompt.
///
/// The backend runs ONE stage per call. The page therefore owns the small
/// orchestration loop (Chinese expansion, then English optimization) so it can
/// render the Chinese brief while the English stage is still running, and let
/// the user edit that brief before re-running only the English stage.
import { invoke } from '@tauri-apps/api/core';
import type { Workspace_context_preview, Workspace_context_selection } from './prompt_workspace.js';

export type Prompt_stage = 'zh_expand' | 'en_optimize';

/// Which local model backend serves a stage. 'gateway' routes through the
/// running AAG gateway (route aliases, failover, breakers); 'provider' calls
/// one configured upstream provider directly, which also works while the
/// gateway is stopped.
export type Prompt_target =
  | { kind: 'gateway'; model: string }
  | { kind: 'provider'; providerId: string; model: string };

export interface Prompt_stage_request {
  stage: Prompt_stage;
  text: string;
  target: Prompt_target;
  instruction?: string;
  temperature?: number;
  maxTokens?: number;
  workspaceContext?: Workspace_context_selection;
  idempotencyKey?: string;
}

export interface Prompt_usage {
  promptTokens: number;
  completionTokens: number;
  totalTokens: number;
}

export interface Prompt_stage_output {
  stage: Prompt_stage;
  output: string;
  complete?: boolean;
  failureCode?: string | null;
  attempts?: number;
  reviewStatus?: string;
  model: string;
  provider?: string | null;
  targetLabel: string;
  elapsedMs: number;
  usage?: Prompt_usage | null;
  workspaceContext?: Workspace_context_preview;
}

/// Built-in stage templates, override setting keys, and validation limits.
export interface Prompt_defaults {
  zhSystem: string;
  enSystem: string;
  zhSettingKey: string;
  enSettingKey: string;
  maxSystemPromptChars: number;
  maxShortInputChars: number;
  maxDraftChars: number;
  maxInstructionChars: number;
  defaultMaxTokens: number;
  defaultEnMaxTokens: number;
  minTokens: number;
  maxTokensLimit: number;
  defaultTemperature: number;
}

export const runPromptStage = (request: Prompt_stage_request) =>
  invoke<Prompt_stage_output>('prompt_enhance_run', { request });

export const getPromptDefaults = () =>
  invoke<Prompt_defaults>('prompt_enhance_defaults');

/// Backend error codes this module knows how to explain to the user.
const ADMISSION_ERRORS: Record<string, string> = {
  provider_admission_excluded: 'This provider is excluded from the pool. Review its subscription and screening result in Suppliers.',
  provider_admission_missing: 'This provider has not passed screening. Open Suppliers and recheck it before generating a prompt.',
  provider_admission_configuration_changed: 'The provider configuration changed after screening. Recheck it in Suppliers.',
  provider_admission_unavailable: 'The current admission status could not be verified. Refresh its screening status in Suppliers.',
  provider_admission_conflict: 'The provider changed during validation. Refresh its screening status in Suppliers.',
  provider_subscription_unavailable: 'The provider subscription could not be verified. Review its current status in Suppliers.',
  provider_subscription_missing: 'No verified subscription status is available for this provider. Recheck it in Suppliers.',
  provider_subscription_source_changed: 'The subscription source changed. Recheck this provider in Suppliers.',
  provider_subscription_source_missing: 'The subscription source is missing. Review the provider connection in Suppliers.',
  provider_subscription_source_unavailable: 'The subscription source is unavailable. Review the provider connection in Suppliers.',
  provider_subscription_conflict: 'Subscription information changed during validation. Recheck the provider in Suppliers.',
  provider_screening_model_unconfigured: 'This model is no longer configured for the provider. Refresh the model selection.',
};

const PROMPT_ERROR_CODES = new Set<string>([
  ...Object.keys(ADMISSION_ERRORS),
  'prompt_enhance_unknown_stage',
  'prompt_enhance_empty_input',
  'prompt_enhance_input_too_long',
  'prompt_enhance_instruction_too_long',
  'prompt_enhance_system_prompt_too_long',
  'prompt_enhance_settings_unavailable',
  'prompt_enhance_gateway_not_running',
  'prompt_enhance_provider_not_found',
  'prompt_enhance_provider_disabled',
  'prompt_enhance_providers_unavailable',
  'prompt_enhance_provider_build_failed',
  'prompt_enhance_target_model_required',
  'prompt_enhance_target_provider_required',
  'prompt_enhance_empty_response',
  'prompt_enhance_output_limit',
  'prompt_enhance_context_budget_exhausted',
  'prompt_enhance_recovery_budget_exhausted',
  'prompt_enhance_continuation_stalled',
  'prompt_enhance_incomplete_response',
  'prompt_enhance_unexpected_termination',
  'prompt_enhance_upstream_timeout',
  'prompt_enhance_fallback_ineligible',
  'prompt_enhance_upstream_failed',
  'prompt_workspace_not_found',
  'prompt_workspace_registry_changed',
  'prompt_workspace_context_changed',
  'prompt_workspace_context_unavailable',
  'prompt_workspace_preview_required',
  'prompt_workspace_path_unsafe',
  'prompt_workspace_path_unavailable',
  'prompt_workspace_registry_invalid',
  'prompt_workspace_storage_unavailable',
  'prompt_workspace_worker_failed',
]);

export interface Prompt_error {
  code: string;
  message?: string;
  field?: string | null;
  retryable?: boolean;
  request_id?: string | null;
  upstream_model?: string | null;
  detail?: string | null;
}

/// Reduce a backend rejection to a known code plus an optional content-free
/// detail. Arbitrary backend text is never forwarded: only the upstream
/// code/status pair this backend constructs itself is allowed through, so a
/// provider error body can never leak into the UI.
export function prompt_error(error: unknown): Prompt_error {
  if (typeof error === 'object' && error !== null) {
    const value = error as {
      error?: { code?: unknown; message?: unknown; field?: unknown; retryable?: unknown };
      code?: unknown; message?: unknown; field?: unknown; retryable?: unknown;
      request_id?: unknown; requestId?: unknown; upstream_model?: unknown; upstreamModel?: unknown;
    };
    const source = value.error && typeof value.error === 'object' ? value.error : value;
    const raw_code = typeof source.code === 'string' && source.code ? source.code : 'prompt_enhance_unknown';
    const raw_message = typeof source.message === 'string' ? source.message : undefined;
    const safe_message = raw_message && !/(?:\b(?:HTTP\s*)?5\d\d\b|\bat\s+[^\s]+\([^\n]+\))/i.test(raw_message)
      ? raw_message
      : undefined;
    return {
      code: PROMPT_ERROR_CODES.has(raw_code) ? raw_code : 'prompt_enhance_unknown',
      message: safe_message,
      field: typeof source.field === 'string' ? source.field : null,
      retryable: typeof source.retryable === 'boolean' ? source.retryable : raw_code.includes('upstream') || raw_code.includes('timeout'),
      request_id: typeof (value.request_id ?? value.requestId) === 'string' ? (value.request_id ?? value.requestId) as string : null,
      upstream_model: typeof (value.upstream_model ?? value.upstreamModel) === 'string' ? (value.upstream_model ?? value.upstreamModel) as string : null,
      detail: null,
    };
  }
  const raw = String(error ?? '');
  if (raw.trim().startsWith('{')) {
    try { return prompt_error(JSON.parse(raw)); } catch { /* fall through to the legacy code parser */ }
  }
  const parts = raw.split(':');
  const code = (parts.shift() ?? '').trim();
  if (!PROMPT_ERROR_CODES.has(code)) {
    return { code: 'prompt_enhance_unknown', detail: null };
  }
  if (code !== 'prompt_enhance_upstream_failed') {
    return { code, detail: null };
  }
  const detail = parts.join(':');
  return { code, retryable: true, request_id: null, detail: /^[a-z0-9_]+:(\d+|none)$/.test(detail) ? detail : null };
}

/// Fixed recovery messages; partial generated text is displayed in its normal
/// output field, never interpolated into an error or diagnostics payload.
export function prompt_recovery_message(code: string): string | null {
  if (ADMISSION_ERRORS[code]) return ADMISSION_ERRORS[code];
  const messages: Record<string, string> = {
    prompt_enhance_context_budget_exhausted: 'The available model context cannot fit the source and a continuation. Completed work is preserved below.',
    prompt_enhance_recovery_budget_exhausted: 'Automatic continuation reached its bounded generation budget. The unfinished result is preserved below.',
    prompt_enhance_continuation_stalled: 'The model repeated its previous output instead of completing it. The unfinished result is preserved below.',
    prompt_enhance_incomplete_response: 'The provider did not confirm a complete response. Available output is preserved below.',
    prompt_enhance_unexpected_termination: 'The provider ended with a filtered response or an unexpected tool request. Completed work is preserved below.',
    prompt_enhance_upstream_timeout: 'The provider timed out. Any completed output is preserved below.',
    prompt_enhance_fallback_ineligible: 'The configured alternative cannot handle this continuation. Completed work is preserved below.',
  };
  return messages[code] ?? null;
}
