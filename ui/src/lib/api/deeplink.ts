/// Deep-link subscription helper.
/// The Rust backend (see `src-tauri/src/deeplink.rs`) emits the
/// `"aag://deeplink"` event with `{ urls: string[] }` whenever an `aag://`
/// URL is opened (macOS via the OS, Linux/Windows via the single-instance
/// argv forwarder). Real URL parsing lands in Phase 9 Step 4 — for now
/// the frontend just collects the URLs and lets the DeepLinkPage render them.
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

export const DEEPLINK_EVENT = 'aag://deeplink';

export interface DeeplinkPayload {
  urls: string[];
}

/// Frontend view of `DeeplinkAction` (camelCase tag).
export type DeeplinkAction =
  | { kind: 'switchProvider'; providerId: string; appType: string }
  | { kind: 'focusProvider'; providerId: string }
  | { kind: 'focusMcp'; serverId: string }
  | { kind: 'focusSkill'; skillId: string }
  | { kind: 'showDeeplinks' }
  | { kind: 'unknown'; raw: string; reason: string };

/// Subscribe to deep-link events. Returns an unlisten fn.
export async function subscribeDeeplink(
  handler: (urls: string[]) => void,
): Promise<UnlistenFn> {
  return listen<DeeplinkPayload>(DEEPLINK_EVENT, (e) => {
    handler(e.payload.urls ?? []);
  });
}

/// Parse a single `aag://` URL into a structured action. Backend impl in
/// `src-tauri/src/deeplink_parser.rs`.
export async function deeplinkParse(url: string): Promise<DeeplinkAction> {
  return invoke<DeeplinkAction>('deeplink_parse', { url });
}