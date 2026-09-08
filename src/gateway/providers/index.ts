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
    case 'gemini':
    default:
      return new OpenAIProvider(cfg);
  }
}

export { BaseProvider } from './base.js';
