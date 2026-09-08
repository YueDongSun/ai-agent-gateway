import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import { Error_boundary } from '../ui/src/components/error_boundary.js';
import { safe_frontend_error_record } from '../ui/src/lib/frontend_diagnostics.js';
import { safe_request_log_record } from '../ui/vite.config.js';

const PRIVATE_VALUES = [
  'CANARY_PROMPT_UNIQUE_91',
  'CANARY_RESPONSE_UNIQUE_92',
  'sk-private-93',
  'C:\\Users\\private-user\\secret.txt',
  'https://user:pass@private.example.invalid/path?token=94#fragment',
  'private.person@example.invalid',
  '100.64.12.34',
  'private-node.tail123.ts.net',
];

function assert_private_values_absent(rendered: string) {
  for (const private_value of PRIVATE_VALUES) {
    expect(rendered).not.toContain(private_value);
  }
}

describe('content-free frontend diagnostics', () => {
  it('sends only bounded counts, a stable category, and a correlation id', () => {
    const error = new Error(PRIVATE_VALUES.join(' | '));
    error.stack = `${PRIVATE_VALUES.join('\n')}\u2028${PRIVATE_VALUES.join('\u2029')}`;
    const record = safe_frontend_error_record(
      'react_error_boundary',
      error,
      PRIVATE_VALUES.join('\n'),
    );
    const rendered = JSON.stringify(record);
    assert_private_values_absent(rendered);
    expect(record.kind).toBe('react_error_boundary');
    expect(record.message_chars).toBeGreaterThan(0);
    expect(record.stack_chars).toBeGreaterThan(0);
    expect(record.correlation_id).toMatch(/^ui_[a-f0-9]+_[a-f0-9]+$/);
  });

  it('renders a stable boundary state instead of the Error message', () => {
    const state = Error_boundary.getDerivedStateFromError(
      new Error(PRIVATE_VALUES.join(' | ')),
    );
    assert_private_values_absent(JSON.stringify(state));
    expect(state).toEqual({ failed: true });
  });

  it('logs only a request category without URL, auth, query, fragment, or path digest', () => {
    const rendered = safe_request_log_record({
      method: 'GET',
      url: `/src/${PRIVATE_VALUES[0]}/${encodeURIComponent(PRIVATE_VALUES[5])}.ts?token=${PRIVATE_VALUES[2]}#${PRIVATE_VALUES[1]}`,
      headers: {
        'user-agent': `WebView2 ${PRIVATE_VALUES[6]}`,
        authorization: `Bearer ${PRIVATE_VALUES[2]}`,
      },
    });
    assert_private_values_absent(rendered);
    expect(rendered).toContain('method=GET');
    expect(rendered).toContain('route=source_module');
    expect(rendered).not.toContain('path_ref=');
    expect(rendered).toContain('client=webview');

    const unknown_method = safe_request_log_record({
      method: 'CANARYSECRET',
      url: '/',
    });
    expect(unknown_method).toContain('method=UNKNOWN');
    expect(unknown_method).not.toContain('CANARYSECRET');
  });

  it('keeps demo unsupported-command logs and errors content-free', () => {
    const source = readFileSync(resolve('ui/src/lib/mock/backend.ts'), 'utf8');
    expect(source).toContain("console.warn('[aag-demo] unsupported_command')");
    expect(source).toContain("throw new Error('demo_command_unsupported')");
    expect(source).toContain("dsh_get_snapshot: 'demo_forced_dsh_get_snapshot'");
    expect(source).toContain("dsh_execute_operation: 'demo_forced_dsh_execute_operation'");
    expect(source).toContain("forcedErrorCodes[cmd] ?? 'demo_forced_error'");
    expect(source).not.toContain('unhandled command: ${cmd}');
    expect(source).not.toContain('unknown command "${cmd}"');
    expect(source).not.toContain('forced demo error for "${cmd}"');
  });

  it('keeps the pre-module HTML fallback content-free', () => {
    const source = readFileSync(resolve('ui/index.html'), 'utf8');
    expect(source).not.toContain('e.message');
    expect(source).not.toContain('e.filename');
    expect(source).not.toContain('e.reason');
    expect(source).not.toContain('String(e.reason)');
  });
});
