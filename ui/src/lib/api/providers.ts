/// Typed Tauri command wrappers. All commands are defined in
/// `src-tauri/src/lib.rs` `invoke_handler!`.
import { invoke } from '@tauri-apps/api/core';
import type {
  Config_edit_target,
  LiveWriteResult,
  Provider,
  Provider_config_status,
  Provider_switch_all_entry,
} from './types';

/** Resolve the current OS and configuration target without writing files. */
export async function get_provider_config_status(appType: string): Promise<Provider_config_status> {
  return invoke<Provider_config_status>('provider_config_status', { appType });
}

/** Read the live Kilo config file verbatim for in-app editing. */
export async function read_client_config(appType: string): Promise<Config_edit_target> {
  return invoke<Config_edit_target>('client_config_read', { appType });
}

/** Validate edited text (JSON/JSONC, object root) and write it back atomically. */
export async function save_client_config(
  appType: string,
  contents: string,
): Promise<LiveWriteResult> {
  return invoke<LiveWriteResult>('client_config_save', { appType, contents });
}

/// List all providers for an app.
export async function listProviders(appType: string): Promise<Provider[]> {
  return invoke<Provider[]>('provider_list', { appType });
}

/// Return the id of the currently active provider for an app, or null.
export async function getCurrentProvider(appType: string): Promise<string | null> {
  return invoke<string | null>('provider_current', { appType });
}

/// Fetch a single provider.
export async function getProvider(
  id: string,
  appType: string,
): Promise<Provider | null> {
  return invoke<Provider | null>('provider_get', { id, appType });
}

/// Upsert a provider (does not activate it).
export async function saveProvider(
  appType: string,
  provider: Provider,
): Promise<void> {
  await invoke('provider_save', { appType, provider });
}

/// Delete a provider.
export async function deleteProvider(id: string, appType: string): Promise<void> {
  await invoke('provider_delete', { id, appType });
}

/// Activate a provider: set as current in DB AND write live config.
export async function switchProvider(
  id: string,
  appType: string,
): Promise<LiveWriteResult> {
  return invoke<LiveWriteResult>('provider_switch', { id, appType });
}

/// One-click fill: apply every supported client's current provider to its
/// live configuration in one action. Per-app outcome, never a total abort.
export async function switchAllProviders(): Promise<Provider_switch_all_entry[]> {
  return invoke<Provider_switch_all_entry[]>('provider_switch_all');
}

/// Reorder providers by writing a new sort_index for each id. Used by
/// the drag handle on the provider cards.
export async function reorderProviders(
  appType: string,
  orderedIds: string[],
): Promise<void> {
  await invoke('provider_reorder', { appType, orderedIds });
}

/// Toggle a provider's membership in its app's failover chain.
export async function setProviderFailoverQueue(
  id: string,
  appType: string,
  inQueue: boolean,
): Promise<void> {
  await invoke('provider_set_failover_queue', { id, appType, inQueue });
}
