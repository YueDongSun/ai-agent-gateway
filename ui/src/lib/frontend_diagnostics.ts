import { diagnostics_record_frontend_error } from './api/diagnostics.js';

let installed = false;
let error_sequence = 0;

export interface Frontend_error_record {
  kind: 'window_error' | 'unhandled_rejection' | 'react_error_boundary' | 'frontend_error';
  message_chars: number;
  stack_chars: number;
  correlation_id: string;
}

function safe_kind(kind: string): Frontend_error_record['kind'] {
  if (
    kind === 'window_error'
    || kind === 'unhandled_rejection'
    || kind === 'react_error_boundary'
  ) {
    return kind;
  }
  return 'frontend_error';
}

function bounded_chars(value: unknown): number {
  if (typeof value === 'string') return Math.min(Array.from(value).length, 512);
  return 0;
}

function next_correlation_id(): string {
  error_sequence += 1;
  return `ui_${Date.now().toString(16)}_${error_sequence.toString(16)}`;
}

export function safe_frontend_error_record(
  kind: string,
  value: unknown,
  component_stack?: string,
): Frontend_error_record {
  const error = value instanceof Error ? value : null;
  return {
    kind: safe_kind(kind),
    message_chars: bounded_chars(error?.message ?? (typeof value === 'string' ? value : '')),
    stack_chars: bounded_chars(component_stack ?? error?.stack ?? ''),
    correlation_id: next_correlation_id(),
  };
}

function report(kind: string, value: unknown) {
  const record = safe_frontend_error_record(kind, value);
  void diagnostics_record_frontend_error(
    record.kind,
    record.message_chars,
    record.stack_chars,
    record.correlation_id,
  ).catch(() => {
    // Browser demo and pre-Tauri bootstrap failures have no native logger.
  });
}

export function install_frontend_diagnostics() {
  if (installed) return;
  installed = true;
  window.addEventListener('error', (event) => {
    report('window_error', event.error ?? event.message);
  });
  window.addEventListener('unhandledrejection', (event) => {
    report('unhandled_rejection', event.reason);
  });
}

export function report_react_error(error: Error, component_stack: string) {
  const record = safe_frontend_error_record(
    'react_error_boundary',
    error,
    component_stack,
  );
  void diagnostics_record_frontend_error(
    record.kind,
    record.message_chars,
    record.stack_chars,
    record.correlation_id,
  ).catch(() => {
    // Keep the local fallback visible even if native logging is unavailable.
  });
}
