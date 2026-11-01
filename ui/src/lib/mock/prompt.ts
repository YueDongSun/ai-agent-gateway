/// Browser-demo prompt-enhancement backend.
///
/// The demo never calls a model: both stages return deterministic synthetic
/// text so the page, its history and its copy buttons stay exercisable in a
/// plain browser. Native behaviour is owned by src-tauri/src/services/prompt.
import type { Prompt_defaults, Prompt_stage, Prompt_stage_output, Prompt_target } from '../api/prompt.js';
import { mock_workspace_receipt } from './prompt_workspace.js';

/// Demo templates are intentionally labelled as browser-only; they are NOT the
/// shipped native templates (the native text lives in the Rust service).
export const DEMO_PROMPT_DEFAULTS: Prompt_defaults = {
  zhSystem: 'Browser demo only: expand a short Chinese requirement into a complete Chinese brief.',
  enSystem: 'Browser demo only: turn the Chinese brief into one optimized English prompt.',
  zhSettingKey: 'prompt_enhance_zh_system',
  enSettingKey: 'prompt_enhance_en_system',
  maxSystemPromptChars: 8192,
  maxShortInputChars: 32000,
  maxDraftChars: 64000,
  maxInstructionChars: 1000,
  defaultMaxTokens: 1200,
  defaultEnMaxTokens: 8192,
  minTokens: 64,
  maxTokensLimit: 8192,
  defaultTemperature: 0.6,
};

interface Demo_request {
  stage?: unknown;
  text?: unknown;
  target?: unknown;
  workspaceContext?: unknown;
}

function demo_target(target: unknown): Prompt_target {
  const candidate = (target ?? {}) as { kind?: unknown; model?: unknown; providerId?: unknown };
  if (candidate.kind === 'provider') {
    return {
      kind: 'provider',
      providerId: String(candidate.providerId ?? 'demo-provider'),
      model: String(candidate.model ?? 'demo-model'),
    };
  }
  return { kind: 'gateway', model: String(candidate.model ?? 'demo-route') };
}

export function mock_prompt_command(command: string, payload: Record<string, unknown>): unknown {
  if (command === 'prompt_enhance_defaults') return DEMO_PROMPT_DEFAULTS;
  if (command !== 'prompt_enhance_run') throw new Error('demo_command_unsupported');

  const request = (payload.request ?? {}) as Demo_request;
  const stage = String(request.stage ?? '');
  if (stage !== 'zh_expand' && stage !== 'en_optimize') {
    throw new Error('prompt_enhance_unknown_stage');
  }
  const text = String(request.text ?? '').trim();
  if (!text) throw new Error('prompt_enhance_empty_input');
  const limit = stage === 'zh_expand'
    ? DEMO_PROMPT_DEFAULTS.maxShortInputChars
    : DEMO_PROMPT_DEFAULTS.maxDraftChars;
  if (text.length > limit) throw new Error('prompt_enhance_input_too_long');

  const target = demo_target(request.target);
  const workspace_context = mock_workspace_receipt(request.workspaceContext);
  const output = stage === 'zh_expand'
    ? '（浏览器演示数据）围绕「' + text + '」的中文扩写：明确交付物、输入范围、输出格式、约束条件与验收标准，并说明需要避免的做法。'
    : 'Browser demo output only. Task: ' + text + ' State the deliverable, the input, the required output format, the constraints, and the acceptance criteria.';
  const result: Prompt_stage_output = {
    stage: stage as Prompt_stage,
    output,
    complete: true,
    attempts: 1,
    reviewStatus: 'demo_only',
    model: target.model,
    provider: target.kind === 'provider' ? target.providerId : 'demo-gateway',
    targetLabel: target.kind === 'provider'
      ? 'provider:' + target.providerId + '/' + target.model
      : 'gateway:' + target.model,
    elapsedMs: 320,
    workspaceContext: workspace_context,
    usage: {
      promptTokens: text.length,
      completionTokens: output.length,
      totalTokens: text.length + output.length,
    },
  };
  return result;
}
