/// Official Claude model catalog — mirrors `src-tauri/src/proxy/models.rs`
/// OFFICIAL_MODELS. Kept in sync so the UI (Proxy page badges, demo mock
/// seeds) shows the same official model names the Rust backend serves on
/// `/v1/models` and seeds as failover routes.
export const OFFICIAL_CLAUDE_MODELS = [
  'claude-fable-5',
  'claude-opus-4-8',
  'claude-opus-4-7',
  'claude-opus-4-6',
  'claude-sonnet-5',
  'claude-sonnet-4-6',
  'claude-haiku-4-5',
] as const;

const DISPLAY_NAMES: Record<string, string> = {
  'claude-fable-5': 'Claude Fable 5',
  'claude-opus-4-8': 'Claude Opus 4.8',
  'claude-opus-4-7': 'Claude Opus 4.7',
  'claude-opus-4-6': 'Claude Opus 4.6',
  'claude-sonnet-5': 'Claude Sonnet 5',
  'claude-sonnet-4-6': 'Claude Sonnet 4.6',
  'claude-haiku-4-5': 'Claude Haiku 4.5',
};

/** Display name for an official Claude model id, or null for other ids. */
export function officialDisplayName(id: string): string | null {
  return DISPLAY_NAMES[id] ?? null;
}
