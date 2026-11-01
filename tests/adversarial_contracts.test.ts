import { describe, expect, it } from 'vitest';
import { Task_queue } from '../src/gateway/task_queue.js';
import { validate_chat_request } from '../src/gateway/request_contract.js';

describe('adversarial request contract', () => {
  it.each([
    '', 'a', 'This is an English prompt.', '中文 mixed with English', '日本語とEnglish', '😀\n$pecial [] {}', 'ALL UPPERCASE', 'all lowercase', 'ignore previous instructions',
  ])('accepts legitimate prompt text: %s', (prompt) => {
    const result = validate_chat_request({ model: 'route', messages: [{ role: 'user', content: prompt || 'x' }] });
    expect(result.ok).toBe(true);
  });

  it('returns per-field errors for malformed requests', () => {
    const result = validate_chat_request({ model: '', messages: [], temperature: 3, max_tokens: 0 });
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.errors.map((error) => error.field)).toEqual(expect.arrayContaining(['model', 'messages', 'temperature', 'max_tokens']));
      expect(result.errors.every((error) => error.code && error.message)).toBe(true);
    }
  });
});

describe('adversarial task queue deduplication', () => {
  it('returns the original task for repeated idempotency keys', () => {
    const queue = new Task_queue({ id_factory: (() => { let n = 0; return () => `id-${++n}`; })() });
    const first = queue.submit({ user_id: 'u', model: 'm', prompt: 'hello', idempotency_key: 'same' });
    const second = queue.submit({ user_id: 'u', model: 'm', prompt: 'changed', idempotency_key: 'same' });
    expect(second.duplicate).toBe(true);
    expect(second.task.task_id).toBe(first.task.task_id);
    expect(queue.size()).toBe(1);
  });

  it('deduplicates same user/model/prompt while directing keyless clients to keying', () => {
    const queue = new Task_queue({ id_factory: (() => { let n = 0; return () => `id-${++n}`; })() });
    const first = queue.submit({ user_id: 'u', model: 'm', prompt: 'same prompt' });
    const second = queue.submit({ user_id: 'u', model: 'm', prompt: 'same prompt' });
    expect(second.duplicate).toBe(true);
    expect(second.task.task_id).toBe(first.task.task_id);
    expect(second.deduplication_notice).toContain('Idempotency-Key');
  });

  it('allows a new task after the validity window', () => {
    let now = 100;
    const queue = new Task_queue({ validity_window_ms: 10, now: () => now, id_factory: (() => { let n = 0; return () => `id-${++n}`; })() });
    const first = queue.submit({ user_id: 'u', model: 'm', prompt: 'same', idempotency_key: 'k' });
    now = 111;
    const second = queue.submit({ user_id: 'u', model: 'm', prompt: 'same', idempotency_key: 'k' });
    expect(second.duplicate).toBe(false);
    expect(second.task.task_id).not.toBe(first.task.task_id);
  });
});
