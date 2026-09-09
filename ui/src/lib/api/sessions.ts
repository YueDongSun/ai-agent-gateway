/// Sessions Tauri command wrappers.
/// Commands defined in `src-tauri/src/lib.rs` (session_list, session_get, session_delete).
import { invoke } from '@tauri-apps/api/core';

export interface SessionMeta {
  id: string;
  appId: string;
  sessionId: string;
  title?: string | null;
  summary?: string | null;
  projectDir?: string | null;
  sourcePath?: string | null;
  resumeCommand?: string | null;
  messageCount: number;
  startedAt?: number | null;
  lastActiveAt?: number | null;
}

export async function listSessions(appId?: string): Promise<SessionMeta[]> {
  return invoke<SessionMeta[]>('session_list', { appId: appId ?? null });
}

export async function getSession(id: string): Promise<SessionMeta | null> {
  return invoke<SessionMeta | null>('session_get', { id });
}

export async function deleteSession(id: string): Promise<void> {
  await invoke('session_delete', { id });
}