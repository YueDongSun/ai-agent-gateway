/// Vendor-side types + Tauri command wrappers.
/// Mirror Rust types from `src-tauri/src/database/dao/vendors.rs`.

import { invoke } from '@tauri-apps/api/core';

/** Protocol ids a vendor can serve. Mirrors the Rust constants. */
export const PROTOCOLS = ['anthropic', 'openai-chat', 'openai-responses'] as const;
export type Protocol = (typeof PROTOCOLS)[number];

export const PROTOCOL_LABELS: Record<Protocol, string> = {
  anthropic: 'Anthropic Messages',
  'openai-chat': 'OpenAI Chat Completions',
  'openai-responses': 'OpenAI Responses',
};

export interface Vendor {
  id: string;
  name: string;
  baseUrl: string;
  apiKey: string;
  protocols: string[];
  /** Per-protocol URL overrides; missing key falls back to baseUrl. */
  endpoints: Record<string, string>;
  createdAt?: number | null;
}

/**
 * Resolve the endpoint URL for a protocol: endpoints[protocol] ?? baseUrl.
 * Mirrors the resolution rule documented in the Rust DAO.
 */
export function resolveEndpoint(v: Vendor, protocol: string): string {
  return v.endpoints[protocol] || v.baseUrl;
}

// ----- Tauri commands (defined in src-tauri/src/lib.rs) -----

export async function listVendors(): Promise<Vendor[]> {
  return invoke<Vendor[]>('vendor_list');
}

export async function saveVendor(vendor: Vendor): Promise<void> {
  await invoke('vendor_save', { vendor });
}

export async function deleteVendor(id: string): Promise<void> {
  await invoke('vendor_delete', { id });
}