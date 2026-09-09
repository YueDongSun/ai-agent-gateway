/// Typed Tauri command wrappers. All commands are defined in
/// `src-tauri/src/lib.rs` `invoke_handler!`.
import { invoke } from '@tauri-apps/api/core';
import type { LiveWriteResult, Provider } from './types';

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