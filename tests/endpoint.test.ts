/**
 * Unit tests for the Node gateway endpoint normalization
 * (src/gateway/providers/endpoint.ts).
 *
 * The case tables mirror src-tauri/src/proxy/endpoint.rs tests 1:1 — the two
 * runtimes must resolve the same stored base_url to the same request URL.
 */
import { describe, it, expect } from 'vitest';
import { endpointUrl, modelsUrls } from '../src/gateway/providers/endpoint.js';

describe('endpointUrl anthropic', () => {
  const cases: Array<[string, string]> = [
    ['https://api.minimaxi.com/anthropic', 'https://api.minimaxi.com/anthropic/v1/messages'],
    ['https://open.bigmodel.cn/api/anthropic/', 'https://open.bigmodel.cn/api/anthropic/v1/messages'],
    ['http://127.0.0.1:11434/v1/', 'http://127.0.0.1:11434/v1/messages'],
    ['http://127.0.0.1:11434/v1', 'http://127.0.0.1:11434/v1/messages'],
    [
      'https://token-plan.cn-beijing.maas.aliyuncs.com/apps/anthropic/v1/messages',
      'https://token-plan.cn-beijing.maas.aliyuncs.com/apps/anthropic/v1/messages',
    ],
    [
      'https://host.test/apps/anthropic/v1/messages/',
      'https://host.test/apps/anthropic/v1/messages',
    ],
    // Query-carrying full endpoint: query preserved, not doubled.
    [
      'https://host.test/apps/anthropic/v1/messages?tenant=abc',
      'https://host.test/apps/anthropic/v1/messages?tenant=abc',
    ],
    // Bare root gets the versioned path.
    ['https://api.anthropic.com', 'https://api.anthropic.com/v1/messages'],
  ];
  it.each(cases)('never doubles messages: %s', (base, want) => {
    expect(endpointUrl(base, 'anthropic')).toBe(want);
  });

  it('empty base falls back to the default host', () => {
    expect(endpointUrl('', 'anthropic')).toBe('https://api.anthropic.com/v1/messages');
  });
});

describe('endpointUrl openai', () => {
  const cases: Array<[string, string]> = [
    ['https://api.openai.com/v1', 'https://api.openai.com/v1/chat/completions'],
    ['https://api.deepseek.com/v1/', 'https://api.deepseek.com/v1/chat/completions'],
    [
      'https://host.test/v1/chat/completions',
      'https://host.test/v1/chat/completions',
    ],
    [
      'https://host.test/v1/chat/completions/',
      'https://host.test/v1/chat/completions',
    ],
    // Bare root gains /v1 (parity with Anthropic).
    ['https://api.openai.com', 'https://api.openai.com/v1/chat/completions'],
    // Azure-style full endpoint with a mandatory query.
    [
      'https://res.openai.azure.com/openai/deployments/gpt4/chat/completions?api-version=2024-08-01',
      'https://res.openai.azure.com/openai/deployments/gpt4/chat/completions?api-version=2024-08-01',
    ],
  ];
  it.each(cases)('never doubles and supplies v1: %s', (base, want) => {
    expect(endpointUrl(base, 'openai')).toBe(want);
  });

  it('empty base falls back to the default host', () => {
    expect(endpointUrl('', 'openai')).toBe('https://api.openai.com/v1/chat/completions');
  });
});

describe('modelsUrls', () => {
  it('reduces a full endpoint to its root', () => {
    expect(
      modelsUrls(
        'https://token-plan.cn-beijing.maas.aliyuncs.com/apps/anthropic/v1/messages',
        'anthropic',
      ),
    ).toEqual([
      'https://token-plan.cn-beijing.maas.aliyuncs.com/apps/anthropic/v1/models',
      'https://token-plan.cn-beijing.maas.aliyuncs.com/apps/anthropic/models',
    ]);
  });

  it('versioned root probes {root}/models then the versionless root', () => {
    expect(modelsUrls('https://api.openai.com/v1', 'openai')).toEqual([
      'https://api.openai.com/v1/models',
      'https://api.openai.com/models',
    ]);
  });

  it('bare root yields a /v1 candidate and a plain candidate', () => {
    expect(modelsUrls('https://api.openai.com', 'openai')).toEqual([
      'https://api.openai.com/v1/models',
      'https://api.openai.com/models',
    ]);
  });

  it('unparseable base yields no candidates', () => {
    expect(modelsUrls('not a url', 'anthropic')).toEqual([]);
  });
});

describe('parseBase rejection', () => {
  it('rejects empty, non-http(s), and credential-carrying bases', () => {
    expect(modelsUrls('', 'anthropic')).toEqual([]);
    expect(modelsUrls('ftp://h.test', 'anthropic')).toEqual([]);
    expect(modelsUrls('https://user:pw@h.test', 'anthropic')).toEqual([]);
    // endpointUrl never throws on bad input — it degrades to string append.
    expect(endpointUrl('ftp://h.test', 'anthropic')).toBe('ftp://h.test/v1/messages');
  });
});
