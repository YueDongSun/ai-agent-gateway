/// Wrapper around the `import_cc_switch_providers` Tauri command.
/// Mirrors `src-tauri/src/services/import_cc_switch/mod.rs`.
import { invoke } from '@tauri-apps/api/core';
import type { ImportReport } from './types';

/// One-shot import of every provider from the cc-switch SQLite DB at
/// `~/.cc-switch/cc-switch.db` (or `$CC_SWITCH_DB_PATH`). Idempotent:
/// rows whose `(id, app_type)` already exist in AAG are skipped, not
/// overwritten. cc-switch's `is_current` bit is dropped — imported
/// rows are available but not auto-activated.
export async function importCcSwitchProviders(): Promise<ImportReport> {
  return invoke<ImportReport>('import_cc_switch_providers');
}
