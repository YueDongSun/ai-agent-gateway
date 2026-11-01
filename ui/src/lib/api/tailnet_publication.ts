import { invoke } from '@tauri-apps/api/core';
import { isDemo } from '../mock/backend.js';

export interface TailnetPublicationConfig {
  enabled: boolean;
  port: number;
}

export interface TailnetPublicationSnapshot {
  state: 'running' | 'stopped' | 'outcome_unknown';
  port: number | null;
  error: string | null;
}

export interface TailnetPublicationStatus {
  config: TailnetPublicationConfig;
  publication: TailnetPublicationSnapshot;
  endpoint: string | null;
  endpoint_source: 'tailscale_status_self' | null;
  metadata_error: string | null;
  synthetic?: boolean;
}

let demo_config: TailnetPublicationConfig = { enabled: false, port: 8989 };
let demo_publication: TailnetPublicationSnapshot = { state: 'stopped', port: null, error: null };

function validate_config(config: TailnetPublicationConfig): void {
  if (typeof config.enabled !== 'boolean' || !Number.isInteger(config.port) || config.port < 1 || config.port > 65535) {
    throw new Error('tailnet_publication_config_invalid');
  }
}

export async function get_tailnet_publication_config(): Promise<TailnetPublicationConfig> {
  return isDemo ? { ...demo_config } : invoke<TailnetPublicationConfig>('tailnet_publication_config_get');
}

export async function save_tailnet_publication_config(config: TailnetPublicationConfig): Promise<void> {
  validate_config(config);
  if (isDemo) {
    demo_config = { ...config };
    demo_publication = { state: 'stopped', port: null, error: null };
    return;
  }
  await invoke('tailnet_publication_config_save', { config });
}

export async function start_tailnet_publication(): Promise<TailnetPublicationSnapshot> {
  if (isDemo) {
    if (!demo_config.enabled) throw new Error('tailnet_publication_disabled');
    demo_publication = { state: 'running', port: demo_config.port, error: null };
    return { ...demo_publication };
  }
  return invoke<TailnetPublicationSnapshot>('tailnet_publication_start');
}

export async function stop_tailnet_publication(): Promise<TailnetPublicationSnapshot> {
  if (isDemo) {
    demo_publication = { state: 'stopped', port: null, error: null };
    return { ...demo_publication };
  }
  return invoke<TailnetPublicationSnapshot>('tailnet_publication_stop');
}

export async function get_tailnet_publication_status(): Promise<TailnetPublicationStatus> {
  if (isDemo) return {
    config: { ...demo_config }, publication: { ...demo_publication },
    endpoint: null, endpoint_source: null, metadata_error: null, synthetic: true,
  };
  return invoke<TailnetPublicationStatus>('tailnet_publication_status');
}
