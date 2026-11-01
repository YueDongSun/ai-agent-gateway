import { invoke } from '@tauri-apps/api/core';

/** Registry metadata is discovery evidence, never installation authority. */
export interface Market_entry {
  id: string;
  name: string;
  full_name: string;
  description: string;
  description_en: string | null;
  description_zh: string | null;
  repo_url: string;
  source_commit: string | null;
  source_commit_url: string | null;
  package_name: string | null;
  version: string | null;
  install_method: 'npm' | 'github' | 'manual' | 'unknown';
  install_spec: string | null;
  categories: string[];
  use_cases: string[];
  modules: string[];
  classification_basis: 'metadata_rules_v1' | 'metadata_rules_v2';
  author: string;
  license: string | null;
  stars: number;
  updated_at: string | null;
  has_client: boolean;
  verification: 'not_audited';
}

export interface Market_catalog {
  schema_version: 1;
  fetched_at: string;
  registry_generated_at: string;
  source_registry: string;
  registry_commit: string;
  registry_url: string;
  collector_url: string;
  entry_count: number;
  entries: Market_entry[];
  cache_status: 'bundled' | 'cached' | 'refreshed';
  warning: string | null;
}

export type Market_source_kind = 'repository' | 'commit' | 'registry' | 'collector';

let pending_catalog_request: Promise<Market_catalog> | null = null;

function request_catalog(command: 'dsh_market_get_catalog' | 'dsh_market_refresh'): Promise<Market_catalog> {
  pending_catalog_request ??= invoke<Market_catalog>(command).finally(() => { pending_catalog_request = null; });
  return pending_catalog_request;
}

export function dsh_market_get_catalog(): Promise<Market_catalog> {
  return request_catalog('dsh_market_get_catalog');
}
export function dsh_market_refresh(): Promise<Market_catalog> {
  return request_catalog('dsh_market_refresh');
}
export function dsh_market_open_source(entry_id: string, source_kind: Market_source_kind): Promise<void> {
  return invoke('dsh_market_open_source', { entry_id, source_kind });
}
export function dsh_open_marketplace(locale: 'en' | 'zh', theme: 'light' | 'dark'): Promise<void> {
  return invoke('dsh_open_marketplace', { locale, theme });
}
