import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { logger, safeLogRef, setLogLevel } from '../src/util/log.js';

let stdout_lines: string[];
let stderr_lines: string[];
let stdout_spy: ReturnType<typeof vi.spyOn>;
let stderr_spy: ReturnType<typeof vi.spyOn>;

beforeEach(() => {
  stdout_lines = [];
  stderr_lines = [];
  stdout_spy = vi.spyOn(process.stdout, 'write').mockImplementation(((chunk: unknown) => {
    stdout_lines.push(String(chunk));
    return true;
  }) as typeof process.stdout.write);
  stderr_spy = vi.spyOn(process.stderr, 'write').mockImplementation(((chunk: unknown) => {
    stderr_lines.push(String(chunk));
    return true;
  }) as typeof process.stderr.write);
  setLogLevel('info');
});

afterEach(() => {
  stdout_spy.mockRestore();
  stderr_spy.mockRestore();
});

describe('logger event allowlist', () => {
  it('passes an allowlisted event through unchanged', () => {
    logger.info('gateway', 'gateway_listening', { port: 8787 });
    expect(stdout_lines.join('')).toContain('gateway: gateway_listening');
    expect(stdout_lines.join('')).toContain('"port":8787');
  });

  it('redacts an unknown event name', () => {
    logger.info('gateway', 'secret message with keys', { apiKey: 'sk-abc' });
    const out = stdout_lines.join('');
    expect(out).toContain('gateway: unsafe_event_redacted');
    expect(out).not.toContain('secret message');
  });

  it('keeps the shutdown event and its signal field', () => {
    logger.info('gateway', 'gateway_shutdown', { signal: 'sigint' });
    const out = stdout_lines.join('');
    expect(out).toContain('gateway: gateway_shutdown');
    expect(out).toContain('"signal":"sigint"');
  });

  it('drops an allowlisted key carrying a disallowed value', () => {
    logger.info('gateway', 'gateway_shutdown', { signal: 'SIGHUP injected' });
    const out = stdout_lines.join('');
    expect(out).toContain('gateway_shutdown');
    expect(out).not.toContain('SIGHUP');
  });

  it('routes warn and error to stderr', () => {
    logger.warn('circuit', 'breaker_opened');
    logger.error('gateway', 'gateway_unhandled_error');
    expect(stdout_lines.join('')).not.toContain('breaker_opened');
    expect(stderr_lines.join('')).toContain('breaker_opened');
    expect(stderr_lines.join('')).toContain('gateway_unhandled_error');
  });

  it('suppresses debug below the current level', () => {
    logger.debug('gateway', 'gateway_listening');
    expect(stdout_lines).toHaveLength(0);
    setLogLevel('debug');
    logger.debug('gateway', 'gateway_listening');
    expect(stdout_lines.join('')).toContain('gateway_listening');
  });
});

describe('safeLogRef', () => {
  it('produces a stable process-local reference that hides the original value', () => {
    const first = safeLogRef('https://user:secret@upstream.invalid/v1');
    const second = safeLogRef('https://user:secret@upstream.invalid/v1');
    expect(first).toMatch(/^ref_[a-f0-9]{12}$/);
    expect(first).toBe(second);
    expect(first).not.toContain('secret');
  });
});
