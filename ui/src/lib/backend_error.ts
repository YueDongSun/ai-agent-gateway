/// Display mapping for backend IPC failures.
///
/// Rust commands reduce failures to stable, secret-free tokens before
/// crossing IPC (`external_message` in `src-tauri/src/error.rs`, plus the
/// literal codes commands return directly such as
/// `proxy_provider_not_found`). Those tokens are safe to render but opaque,
/// so this module is the single place that turns them into sentences.
///
/// Anything that is not a known token — a demo-mock message, a UI validation
/// string, an HTTP status from a direct fetch — is shown verbatim: it was
/// produced on this side of the IPC boundary and carries no backend
/// internals.

import { supplier_discovery_error_text } from './supplier_discovery_text.js';

export type Error_translator = (
  key: string,
  vars?: Record<string, string | number>,
) => string;

const TOKEN_KEYS: Record<string, string> = {
  // `AppError::external_message` fallbacks.
  operation_failed: 'error.operation_failed',
  io_error: 'error.io',
  json_error: 'error.json',
  database_error: 'error.database',
  lock_error: 'error.lock',
  configuration_error: 'error.configuration',
  invalid_input: 'error.invalid_input',
  cc_switch_conflict: 'error.cc_switch_conflict',
  // Uppercase AAG_* codes preserved by `stable_detail`.
  AAG_CLIENT_CONFIG_PATHS_INVALID: 'error.client_config_paths_invalid',
  AAG_PROVIDER_NOT_FOUND: 'error.provider_not_found',
  AAG_UNKNOWN_APP_TYPE: 'error.unknown_app_type',
  AAG_SUPPLIER_PROJECTION_IN_USE: 'error.supplier_projection_in_use',
  // Literal command codes (commands return these strings directly).
  proxy_provider_not_found: 'error.proxy_provider_not_found',
  proxy_provider_build_failed: 'error.proxy_provider_build_failed',
  proxy_state_lock_failed: 'error.worker_failed',
  proxy_shutdown_signal_failed: 'error.worker_failed',
  proxy_server_task_join_failed: 'error.worker_failed',
  client_config_read_worker_failed: 'error.worker_failed',
  client_config_save_worker_failed: 'error.worker_failed',
  provider_config_worker_failed: 'error.worker_failed',
  launcher_worker_failed: 'error.worker_failed',
  desktop_mirror_models_unavailable: 'error.desktop_models_unavailable',
  desktop_mirror_restart_required: 'error.desktop_restart_required',
};

const MODELS_HTTP_PATTERN = /^proxy_models_http_error_(\d{3})$/;

/** Raw text of an IPC rejection: Tauri rejects with the command's `Err`
 *  string, while JS-side failures arrive as `Error` objects. */
function error_text(error: unknown): string {
  if (typeof error === 'string') return error.trim();
  if (error instanceof Error) return error.message.trim();
  if (error === null || error === undefined) return '';
  return String(error).trim();
}

/** Human display text for a backend/mutation failure. */
export function backend_error_text(error: unknown, t: Error_translator): string {
  const raw = error_text(error).replace(/^Error:\s*/, '');
  if (raw === '') return t('error.operation_failed');
  const key = TOKEN_KEYS[raw];
  if (key) return t(key);
  const http = MODELS_HTTP_PATTERN.exec(raw);
  if (http) return t('error.proxy_models_http', { status: http[1] });
  const discovery = supplier_discovery_error_text(raw, t);
  if (discovery !== null) return discovery;
  return raw;
}
