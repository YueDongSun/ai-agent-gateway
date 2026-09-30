import process from 'node:process';
import { createHmac, randomBytes } from 'node:crypto';

export type LogLevel = 'debug' | 'info' | 'warn' | 'error';

const ORDER: Record<LogLevel, number> = { debug: 10, info: 20, warn: 30, error: 40 };

let currentLevel: LogLevel = 'info';

const SAFE_TAGS = new Set([
  'anthropic',
  'circuit',
  'gateway',
  'openai',
  'provider',
  'router',
  'switch',
]);

const SAFE_EVENTS = new Set([
  'all_hops_exhausted',
  'breaker_closed',
  'breaker_half_open',
  'breaker_opened',
  'gateway_endpoint_ready',
  'gateway_listening',
  'gateway_unhandled_error',
  'mid_stream_failed',
  'provider_failed',
  'provider_skipped',
  'provider_succeeded',
  'request_failed',
  'route_not_found',
  'sse_parse_failed',
  'stream_ended',
  'stream_failed',
  'stream_succeeded',
  'switch_updated',
  'unsafe_event_redacted',
]);

const SAFE_FIELD_NAMES = new Set([
  'code',
  'data_bytes',
  'elapsed_ms',
  'message_count',
  'method',
  'model_ref',
  'port',
  'provider_ref',
  'request_id',
  'response_bytes',
  'retriable',
  'stage',
  'status',
  'stream',
  'surface',
  'tool_count',
]);

type SafeLogScalar = boolean | number | string | null;
export type LogMetadata = Record<string, unknown>;

const NUMBER_FIELDS = new Set([
  'data_bytes',
  'elapsed_ms',
  'message_count',
  'port',
  'response_bytes',
  'status',
  'tool_count',
]);
const BOOLEAN_FIELDS = new Set(['retriable', 'stream']);
const REFERENCE_FIELDS = new Set(['model_ref', 'provider_ref']);
const LOG_REFERENCE_KEY = randomBytes(32);

export function setLogLevel(level: LogLevel): void {
  currentLevel = level;
}

function ts(): string {
  return new Date().toISOString().replace('T', ' ').replace('Z', '');
}

/** Return a process-local, keyed reference for a private identifier. */
export function safeLogRef(value: string): string {
  return `ref_${createHmac('sha256', LOG_REFERENCE_KEY).update(value).digest('hex').slice(0, 12)}`;
}

function safeTag(tag: string): string {
  return SAFE_TAGS.has(tag) ? tag : 'provider';
}

function normalizeLegacyEvent(message: string): string {
  if (SAFE_EVENTS.has(message)) return message;
  if (message.startsWith('breaker OPENED')) return 'breaker_opened';
  if (message === 'breaker -> half-open (probing)') return 'breaker_half_open';
  if (message === 'breaker CLOSED (recovered)') return 'breaker_closed';
  if (message.includes('-> gateway') || message.includes('-> direct')) return 'switch_updated';
  return 'unsafe_event_redacted';
}

function safeMetadata(extra: LogMetadata | undefined): Record<string, SafeLogScalar> | undefined {
  if (!extra) return undefined;
  const safe: Record<string, SafeLogScalar> = {};
  for (const [key, value] of Object.entries(extra)) {
    if (!SAFE_FIELD_NAMES.has(key)) continue;
    if (NUMBER_FIELDS.has(key) && typeof value === 'number' && Number.isFinite(value)) {
      safe[key] = value;
    } else if (BOOLEAN_FIELDS.has(key) && typeof value === 'boolean') {
      safe[key] = value;
    } else if (
      REFERENCE_FIELDS.has(key)
      && typeof value === 'string'
      && /^ref_[a-f0-9]{12}$/.test(value)
    ) {
      safe[key] = value;
    } else if (
      (key === 'code' || key === 'stage')
      && typeof value === 'string'
      && /^[a-z][a-z0-9_]{0,63}$/.test(value)
    ) {
      safe[key] = value;
    } else if (
      key === 'request_id'
      && typeof value === 'string'
      && /^(?:req_[a-f0-9_]+|[a-f0-9-]{32,36})$/.test(value)
    ) {
      safe[key] = value;
    } else if (
      key === 'method'
      && typeof value === 'string'
      && ['GET', 'POST', 'PUT', 'DELETE', 'PATCH'].includes(value)
    ) {
      safe[key] = value;
    } else if (
      key === 'surface'
      && typeof value === 'string'
      && ['chat', 'claude', 'codex', 'health', 'models'].includes(value)
    ) {
      safe[key] = value;
    }
  }
  return Object.keys(safe).length ? safe : undefined;
}

function log(level: LogLevel, tag: string, event: string, extra?: LogMetadata): void {
  if (ORDER[level] < ORDER[currentLevel]) return;
  const line = `${ts()} [${level.toUpperCase()}] ${safeTag(tag)}: ${normalizeLegacyEvent(event)}`;
  const stream = level === 'error' || level === 'warn' ? process.stderr : process.stdout;
  stream.write(line + '\n');
  const metadata = safeMetadata(extra);
  if (metadata !== undefined) {
    stream.write(JSON.stringify(metadata) + '\n');
  }
}

export const logger = {
  setLevel: setLogLevel,
  debug: (tag: string, event: string, extra?: LogMetadata) => log('debug', tag, event, extra),
  info: (tag: string, event: string, extra?: LogMetadata) => log('info', tag, event, extra),
  warn: (tag: string, event: string, extra?: LogMetadata) => log('warn', tag, event, extra),
  error: (tag: string, event: string, extra?: LogMetadata) => log('error', tag, event, extra),
};
