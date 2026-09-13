/// Shared API types. Mirror Rust types from `src-tauri/src/provider.rs`.

export type AppType =
  | 'claude_code'
  | 'codex'
  | 'copilot'
  | 'pi'
  | 'antigravity';

export const APP_TYPES: { value: AppType; label: string }[] = [
  { value: 'claude_code', label: 'Claude Code' },
  { value: 'codex', label: 'Codex' },
  { value: 'copilot', label: 'Copilot CLI' },
  { value: 'pi', label: 'Pi (MS Coding Agent)' },
  { value: 'antigravity', label: 'Antigravity' },
];

export interface Provider {
  id: string;
  name: string;
  settingsConfig: Record<string, unknown>;
  websiteUrl?: string | null;
  category?: string | null;
  createdAt?: number | null;
  sortIndex?: number | null;
  notes?: string | null;
  icon?: string | null;
  iconColor?: string | null;
  meta?: ProviderMeta | null;
  inFailoverQueue?: boolean;
  syncLocked?: boolean;
}

export interface ProviderMeta {
  custom_endpoints?: Record<string, CustomEndpoint>;
}

export interface CustomEndpoint {
  url: string;
  added_at: number;
  last_used?: number | null;
}

export interface LiveWriteResult {
  app_type: string;
  provider_id: string;
  written: string[];
  backup: string | null;
}

/// Source of the cc-switch detection. Mirrors the Rust
/// `services::detect::cc_switch::CcSwitchSource` enum.
export type CcSwitchSource =
  | 'port'
  | 'db_file'
  | 'claude_settings'
  | 'codex_config';

/// Structured cc-switch coexistence status returned by the detector.
/// See `src-tauri/src/services/detect/cc_switch.rs`.
export interface CcSwitchStatus {
  running: boolean;
  source?: CcSwitchSource | null;
  port: number;
  liveUrlClaude?: string | null;
  liveUrlCodex?: string | null;
  checkedAt: number;
  suppressed: boolean;
}

/// One row of feedback from `import_cc_switch_providers`.
export interface ImportedProvider {
  id: string;
  appType: string;
  name: string;
}

/// Structured report returned by `import_cc_switch_providers`.
export interface ImportReport {
  sourceDb: string;
  imported: ImportedProvider[];
  skippedExisting: ImportedProvider[];
  totalInSource: number;
}