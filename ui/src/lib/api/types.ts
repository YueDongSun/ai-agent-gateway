/// Shared API types. Mirror Rust types from `src-tauri/src/provider.rs`.

export type AppType = 'claude_code' | 'codex' | 'copilot' | 'pi' | 'antigravity';

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
  totalInSource: number;
  /** Routes auto-built from imported Claude providers' tier mappings. */
  importedRoutes: ImportedRoute[];
}
