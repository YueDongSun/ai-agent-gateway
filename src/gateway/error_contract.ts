import { randomUUID } from 'node:crypto';
import type { ProviderError } from './canonical.js';
import { publicProviderErrorMessage } from './canonical.js';

export interface Public_error_envelope {
  error: {
    code: string;
    message: string;
    field: string | null;
    retryable: boolean;
    correlation_id: string;
    retriable: boolean;
  };
  request_id: string;
  upstream_model?: string;
}

export interface Public_error_options {
  status?: number;
  request_id?: string;
  upstream_model?: string;
  field?: string | null;
}

export function make_public_error_envelope(error: ProviderError, options: Public_error_options = {}): Public_error_envelope {
  const request_id = options.request_id ?? randomUUID();
  const status = options.status ?? error.status;
  const code = error.code === 'upstream_transport_error' && status === 502
    ? 'upstream_unavailable': error.code;
  const field = options.field ?? (code === 'invalid_model' ? 'model' : null);
  const message = code === 'upstream_unavailable'
    ? 'The upstream model is temporarily unavailable, please retry shortly.'
    : code === 'invalid_model'
      ? 'The requested model is unavailable. Choose an available model and try again.'
      : publicProviderErrorMessage(code, status);
  const retryable = code === 'upstream_unavailable' || (code === 'all_hops_exhausted' && (status ?? 0) >= 500) ? true : error.retriable;
  return {
    error: { code, message, field, retryable, correlation_id: request_id, retriable: retryable },
    request_id,
    upstream_model: code === 'invalid_model' || code === 'no_route' ? undefined : options.upstream_model,
  };
}
