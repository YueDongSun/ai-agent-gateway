/// Shared API types. Mirror Rust types from `src-tauri/src/provider.rs`.

export type AppType =
  | 'claude_code'
  | 'codex'
  | 'copilot'
  | 'gemini'
  | 'opencode'
  | 'pi'
  | 'antigravity'
  | 'kilo_cli'
  | 'cline_cli'
  | 'claude_vscode'
  | 'kilo_vscode';

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
  meta?: Record<string, unknown> | null;
  inFailoverQueue?: boolean;
  syncLocked?: boolean;
}

export interface LiveWriteResult {
  app_type: string;
  provider_id: string;
  written: string[];
  backup: string | null;
}

/** Per-client outcome of the bulk "apply current provider" command. */
export interface Provider_switch_all_entry {
  app_type: string;
  provider_id: string | null;
  outcome: 'applied' | 'no_current' | 'error';
  detail: string | null;
}

/** Read-only discovery of the configuration used by provider switching. */
export interface Provider_config_status {
  appType: string;
  platform: string;
  supported: boolean;
  mode: 'config' | 'mirror' | 'environment' | 'unsupported';
  source: 'environment' | 'existing' | 'default' | 'setting' | 'unsupported';
  configPaths: string[];
  existing: boolean;
  reason: string | null;
}

/** Live config file target for the direct JSON editor (Kilo family). */
export interface Config_edit_target {
  appType: string;
  path: string;
  /** Verbatim file contents ('' when the file does not exist yet). */
  contents: string;
  existing: boolean;
}

/// One row of feedback from `import_cc_switch_providers`.
export interface ImportedProvider {
  id: string;
  appType: string;
  name: string;
}

/// One failover route auto-built from an imported provider's
/// `ANTHROPIC_DEFAULT_*_MODEL` env keys. Surfaced by the importer so
/// the UI toast can report how many routes were created alongside the
/// provider count.
export interface ImportedRoute {
  alias: string;
  provider: string;
  model: string;
}

/// Structured report returned by `import_cc_switch_providers`.
export interface ImportReport {
  sourceDb: string;
  imported: ImportedProvider[];
  skippedExisting: ImportedProvider[];
  /** Providers filtered out because they carry no API key (e.g. Codex
   *  ChatGPT-login / `default`). */
  skippedNoKey: ImportedProvider[];
  totalInSource: number;
  /** Routes auto-built from imported Claude providers' tier mappings. */
  importedRoutes: ImportedRoute[];
  /** New rows force-added to the failover queue (usable upstream found). */
  queuedProviders?: ImportedProvider[];
  /** Subscription rows (official api.anthropic.com + empty key) imported
   *  as anthropic passthrough pool rows. */
  passthroughProviders?: ImportedProvider[];
}
