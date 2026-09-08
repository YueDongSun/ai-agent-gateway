/// App-level Tauri command wrappers (Phase 9 Step 3 + Phase 10 Step 1).
/// `app_info` lives in `types.ts` as the interface; the actual invoke call
/// is in `App.tsx`. This file just exposes the auto-update check.
import { invoke } from '@tauri-apps/api/core';

/// Optional override for the updater endpoint. Useful for testing against
/// a dev / staging release server without rebuilding.
export interface AppCheckUpdateOptions {
  endpoint?: string | null;
  pubkey?: string | null;
}

/// Returns the latest version string if an update is available, or `null`
/// if the app is up to date or no updater endpoint is configured.
export async function appCheckUpdate(
  options: AppCheckUpdateOptions = {},
): Promise<string | null> {
  return invoke<string | null>('app_check_update', {
    endpoint: options.endpoint ?? null,
    pubkey: options.pubkey ?? null,
  });
}