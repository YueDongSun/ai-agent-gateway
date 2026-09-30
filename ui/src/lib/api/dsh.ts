import { invoke } from '@tauri-apps/api/core';

/** DSH IPC DTOs contain configuration references and evidence, never credentials. */
export interface Dsh_settings {
  node_path: string;
  dsh_entry_path: string;
  pnpm_entry_path: string;
  port: number;
  startup_timeout_secs: number;
  operation_timeout_secs: number;
  theme: 'system' | 'light' | 'dark';
  upstream_id: string | null;
  model_id: string | null;
}

export interface Dsh_plugin {
  plugin_id: string;
  package_name: string;
  display_name: string;
  category: string;
  description: string;
  source_url: string;
  installed_version: string | null;
  enabled: boolean;
  essential: boolean;
  runtime_phase: string;
}

export interface Dsh_receipt {
  operation_id: string;
  action: string;
  package_name: string | null;
  version: string | null;
  status: 'succeeded' | 'failed' | 'cancelled' | 'timed_out' | 'rollback_failed';
  error_code: string | null;
  started_at_ms: number;
  finished_at_ms: number;
  rollback_status: 'not_needed' | 'restored' | 'failed';
  restart_required: boolean;
}

export interface Dsh_snapshot {
  settings: Dsh_settings;
  settings_digest: string;
  initialized: boolean;
  managed_home: string;
  plugins: Dsh_plugin[];
  runtime: {
    phase: 'stopped' | 'starting' | 'web_ready' | 'failed';
    pid: number | null;
    web_url: string | null;
    error_code: string | null;
  };
  job: { operation_id: string; action: string; package_name: string | null; phase: string } | null;
  receipts: Dsh_receipt[];
}

export interface Dsh_operation_request {
  action: 'initialize' | 'install' | 'remove' | 'enable' | 'disable';
  package_name: string | null;
  version: string | null;
}

export interface Dsh_preview {
  preview_id: string;
  action: string;
  package_name: string | null;
  version: string | null;
  changes: string[];
  warnings: string[];
  expires_at_ms: number;
}

export function dsh_get_snapshot(): Promise<Dsh_snapshot> {
  return invoke('dsh_get_snapshot');
}

export function dsh_save_settings(settings: Dsh_settings, expected_digest: string): Promise<Dsh_snapshot> {
  return invoke('dsh_save_settings', { settings, expected_digest });
}

export function dsh_preview_operation(request: Dsh_operation_request): Promise<Dsh_preview> {
  return invoke('dsh_preview_operation', { request });
}

export function dsh_execute_operation(preview_id: string): Promise<Dsh_receipt> {
  return invoke('dsh_execute_operation', { preview_id });
}

export function dsh_cancel_operation(): Promise<boolean> {
  return invoke('dsh_cancel_operation');
}

export function dsh_start(): Promise<Dsh_snapshot> {
  return invoke('dsh_start');
}

export function dsh_stop(): Promise<Dsh_snapshot> {
  return invoke('dsh_stop');
}

export function dsh_open_web_ui(): Promise<void> {
  return invoke('dsh_open_web_ui');
}

export interface Dsh_upstream {
  id: string;
  name: string;
  protocol: string;
  models: string[];
  source_kind: 'claude_native' | 'proxy_pool' | 'gateway';
  available: boolean;
  reason?: string | null;
}

export function dsh_list_upstreams(): Promise<Dsh_upstream[]> {
  return invoke('dsh_list_upstreams');
}

/** Discovery inspects existing paths and package metadata; it does not execute them. */
export interface Dsh_runtime_candidate {
  candidate_id: string;
  kind: 'node' | 'dsh' | 'pnpm';
  path: string;
  source: string;
  version: string | null;
  compatible: boolean;
  validation: string;
  reason: string | null;
}

export interface Dsh_runtime_paths {
  node_path: string;
  dsh_entry_path: string;
  pnpm_entry_path: string;
}

export interface Dsh_discovery {
  candidates: Dsh_runtime_candidate[];
  recommended: Dsh_runtime_paths;
  issues: string[];
  scanned_locations: number;
  scan_limited: boolean;
}

export interface Dsh_setup_request {
  expected_digest: string;
  upstream_id?: string | null;
  model_id?: string | null;
}

export interface Dsh_setup_plan {
  plan_id: string;
  expected_digest: string;
  expires_at_ms: number;
  settings: Dsh_settings;
  changes: string[];
  warnings: string[];
  blockers: string[];
  source_reason: string;
  can_apply: boolean;
}

let pending_discovery: Promise<Dsh_discovery> | null = null;
export function dsh_discover_runtime(): Promise<Dsh_discovery> {
  pending_discovery ??= invoke<Dsh_discovery>('dsh_discover_runtime').finally(() => { pending_discovery = null; });
  return pending_discovery;
}

export function dsh_preview_setup(request: Dsh_setup_request): Promise<Dsh_setup_plan> {
  return invoke('dsh_preview_setup', { request });
}

export function dsh_apply_setup(plan_id: string): Promise<Dsh_snapshot> {
  return invoke('dsh_apply_setup', { plan_id });
}
