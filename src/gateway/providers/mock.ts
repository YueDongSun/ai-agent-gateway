import { randomBytes } from 'node:crypto';
import type { ChatCompletionRequest } from '../../config/schema.js';
import type { OAIChatResponse, OAIStreamChunk } from '../canonical.js';
import { BaseProvider, makeError } from './base.js';

const CREATED = Math.floor(Date.now() / 1000);

function rid(): string {
  return 'chatcmpl-mock-' + randomBytes(6).toString('hex');
}

/**
 * Mock provider. Requires no network / keys.
 *
 * Special model names:
 *  - "mock-fail"      : always fail (5xx) — demonstrates circuit-breaker + failover
 *  - "mock-slow"      : 2s delay per token, succeeds
 *  - anything else    : returns a canned echo of the last user message
 */
export class MockProvider extends BaseProvider {
  async complete(req: ChatCompletionRequest): Promise<OAIChatResponse> {
    const model = req.model;
    if (model === 'mock-fail') {
      throw makeError(`mock provider ${this.id} forced failure for model "${model}"`, {
        status: 503,
        retriable: true,
      });
    }
    if (model === 'mock-slow') {
      await sleep(2000);
    }
    const text = BaseProvider.extractText(req) || '(empty)';
    const reply = `MOCK[${this.id}] :: ${model}\n\nYou said: ${text}`;
    return {
      id: rid(),
      object: 'chat.completion',
      created: CREATED,
      model,
      choices: [
        {
          index: 0,
          message: { role: 'assistant', content: reply },
          finish_reason: 'stop',
        },
      ],
      usage: { prompt_tokens: text.length, completion_tokens: reply.length, total_tokens: text.length + reply.length },
    };
  }

  async *stream(req: ChatCompletionRequest): AsyncGenerator<OAIStreamChunk> {
    const model = req.model;
    if (model === 'mock-fail') {
      // Fail before emitting anything so the gateway can failover cleanly.
      throw makeError(`mock ${this.id} forced stream failure`, { status: 503, retriable: true });
    }
    const id = rid();
    const text = BaseProvider.extractText(req) || '(empty)';
    const reply = `MOCK[${this.id}] :: ${model}\n\nYou said: ${text}`;
    const tokens = reply.match(/(\S+\s*|\s+)/g) ?? [reply];

    // leading role chunk
    yield {
      id,
      object: 'chat.completion.chunk',
      created: CREATED,
      model,
      choices: [{ index: 0, delta: { role: 'assistant' }, finish_reason: null }],
    };
    for (const tok of tokens) {
      if (model === 'mock-slow') await sleep(120);
      yield {
        id,
        object: 'chat.completion.chunk',
        created: CREATED,
        model,
        choices: [{ index: 0, delta: { content: tok }, finish_reason: null }],
      };
    }
    yield {
      id,
      object: 'chat.completion.chunk',
      created: CREATED,
      model,
      choices: [{ index: 0, delta: {}, finish_reason: 'stop' }],
    };
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}
