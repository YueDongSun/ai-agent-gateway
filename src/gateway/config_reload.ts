/**
 * Hot config reload for the long-running Node gateway (Rust runtime
 * parity: the desktop proxy rebuilds its routing snapshot when the config
 * changes; the Node gateway rebuilds its Router). A file that fails to
 * parse or validate keeps the previous config serving — a broken file on
 * disk must not take a running gateway down.
 */
import { watch, readFileSync, type FSWatcher } from 'node:fs';
import { basename, dirname } from 'node:path';
import { AppConfig, type AppConfig as AppConfigType } from '../config/schema.js';
import { logger, type Reload_reason } from '../util/log.js';

/**
 * Debounce window coalescing the write/rename burst an atomic save emits.
 * The directory (not the file) is watched: rename-based atomic writes
 * invalidate file-level watchers, on Windows especially.
 */
export const CONFIG_RELOAD_DEBOUNCE_MS = 250;

export interface config_watcher {
  close(): void;
}

/**
 * Watch the JSON config file and call `reload` with each freshly parsed,
 * validated config. Events for other files in the directory are ignored;
 * watcher errors (directory removed, permissions) stop the watcher with a
 * warning instead of crashing the gateway.
 */
export function start_config_watcher(params: {
  config_path: string;
  reload: (cfg: AppConfigType) => void;
  debounce_ms?: number;
}): config_watcher {
  const debounce_ms = params.debounce_ms ?? CONFIG_RELOAD_DEBOUNCE_MS;
  const watched_name = basename(params.config_path).toLowerCase();
  let timer: NodeJS.Timeout | undefined;

  const fire = () => {
    let reason: Reload_reason = 'read_failed';
    try {
      const raw = readFileSync(params.config_path, 'utf8');
      reason = 'invalid_json';
      const parsed: unknown = JSON.parse(raw);
      reason = 'invalid_config';
      const cfg = AppConfig.parse(parsed);
      reason = 'apply_failed';
      params.reload(cfg);
    } catch {
      logger.warn('config', 'config_reload_rejected', {
        reload_reason: reason, reload_status: 'rejected',
      });
    }
  };

  let watcher: FSWatcher;
  try {
    watcher = watch(dirname(params.config_path), (_event, filename) => {
      if (typeof filename !== 'string') return;
      if (filename.toLowerCase() !== watched_name) return;
      if (timer) clearTimeout(timer);
      timer = setTimeout(fire, debounce_ms);
    });
  } catch {
    logger.warn('config', 'config_watcher_failed', { reload_reason: 'watch_failed', reload_status: 'failed' });
    throw new Error('config watcher could not start; automatic reload is unavailable');
  }
  watcher.on('error', () => {
    logger.warn('config', 'config_watcher_failed', { reload_reason: 'watch_failed', reload_status: 'failed' });
    if (timer) clearTimeout(timer);
    watcher.close();
  });

  return {
    close() {
      if (timer) clearTimeout(timer);
      watcher.close();
    },
  };
}
