/// Typed Tauri command wrappers for the cc-switch coexistence detector.
///
/// Mirrors the Rust commands in `src-tauri/src/lib.rs`:
/// - `detect_cc_switch()`            -> `CcSwitchStatus`
/// - `set_cc_switch_override(b)`     -> `void`
/// - `get_cc_switch_blocking()`      -> `boolean`
/// - `set_cc_switch_blocking(b)`     -> `void`
import { invoke } from '@tauri-apps/api/core';
import type { CcSwitchStatus } from './types';

/// Run the four-signal cc-switch detector and return the structured
/// status. Cheap (one TCP probe + a few file probes); safe to call on
/// every UI mount.
export async function detectCcSwitch(): Promise<CcSwitchStatus> {
  return invoke<CcSwitchStatus>('detect_cc_switch');
}

/// Toggle the "ignore cc-switch conflicts" override. When enabled,
/// `provider_switch` / `*_open_cli_terminal` skip the conflict check
/// (advanced users opting out of the safety dialog).
export async function setCcSwitchOverride(enabled: boolean): Promise<void> {
  await invoke('set_cc_switch_override', { enabled });
}

/// Whether the opt-in hard-block mode is on. Default false. When on,
/// AAG blocks provider switches + terminal launches that would clobber
/// cc-switch's own URL while cc-switch is running.
export async function getCcSwitchBlocking(): Promise<boolean> {
  return invoke<boolean>('get_cc_switch_blocking');
}

/// Toggle the opt-in hard-block mode. See `getCcSwitchBlocking`.
export async function setCcSwitchBlocking(enabled: boolean): Promise<void> {
  await invoke('set_cc_switch_blocking', { enabled });
}

/// Whether `proxy_start` should auto-pull new claude_code providers out
/// of the cc-switch DB into AAG. Default false. Toggled in the
/// Providers page; takes effect on the next proxy_start.
export async function getSyncWithCcSwitch(): Promise<boolean> {
  return invoke<boolean>('get_sync_with_ccswitch');
}

export async function setSyncWithCcSwitch(enabled: boolean): Promise<void> {
  await invoke('set_sync_with_ccswitch', { enabled });
}

/// Toggle `providers.sync_locked` so the auto-sync path stops touching
/// this row even when the user keeps auto-sync on.
export async function setProviderSyncLocked(
  id: string,
  appType: string,
  locked: boolean,
): Promise<void> {
  await invoke('set_provider_sync_locked', { id, appType, locked });
}
