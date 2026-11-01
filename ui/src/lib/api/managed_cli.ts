import { invoke } from '@tauri-apps/api/core';
import type { AppType } from './types';

export type Managed_cli_app_type = Extract<AppType, 'kilo_cli' | 'cline_cli'>;

export interface Managed_cli_status {
  appType: Managed_cli_app_type;
  available: boolean;
  binary: string | null;
  installUrl: string;
}

export interface Managed_cli_open_receipt {
  appType: Managed_cli_app_type;
  binary: string;
  /** Backend-redacted environment receipt. */
  env: Array<[string, string]>;
}

export function managed_cli_status(app_type: Managed_cli_app_type): Promise<Managed_cli_status> {
  return invoke<Managed_cli_status>('managed_cli_status', { appType: app_type });
}

export function managed_cli_open_terminal(app_type: Managed_cli_app_type): Promise<Managed_cli_open_receipt> {
  return invoke<Managed_cli_open_receipt>('managed_cli_open_terminal', { appType: app_type, cwd: null });
}
