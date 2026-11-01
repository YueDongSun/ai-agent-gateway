import type { ProviderConfig } from '../../config/schema.js';
import { BaseProvider } from './base.js';
import { MockProvider } from './mock.js';
import { OpenAIProvider } from './openai.js';
import { AnthropicProvider } from './anthropic.js';

export function createProvider(cfg: ProviderConfig): BaseProvider {
  switch (cfg.type) {
    case 'mock':
      return new MockProvider(cfg);
    case 'anthropic':
      return new AnthropicProvider(cfg);
    case 'openai':
      return new OpenAIProvider(cfg);
    default:
      // The schema rejects `gemini` at config load; this guard keeps a
      // programmatically-built row from silently riding the OpenAI wire.
      throw new Error(`provider type '${(cfg as { type: string }).type}' has no Node gateway provider`);
  }
}

export { BaseProvider } from './base.js';
