/// Cloud sync Tauri command wrappers.
/// Commands are defined in `src-tauri/src/lib.rs` (sync_list, sync_save,
/// sync_delete, sync_record_result, sync_check, sync_upload, sync_download).
/// Both transports (WebDAV v1 + S3 with AWS SigV4) are now wired. The
/// backend dispatches by `config.provider`.
import { invoke } from '@tauri-apps/api/core';

export type SyncProvider = 's3' | 'webdav';

export interface SyncConfig {
  provider: SyncProvider;
  enabled: boolean;
  endpoint: string;
  /// S3 bucket name (only meaningful when `provider === 's3'`).
  bucket?: string | null;
  /// WebDAV remote directory (only meaningful when `provider === 'webdav'`).
  remotePath?: string | null;
  /// S3 access_key_id OR WebDAV username.
  accessKey: string;
  /// S3 secret_access_key OR WebDAV password.
  secretKey: string;
  /// S3 region (only meaningful when `provider === 's3'`).
  region?: string | null;
  /// Unix epoch seconds of the last sync attempt (set by the sync service).
  lastSyncAt?: number | null;
  /// Error from the last sync attempt, if any.
  lastError?: string | null;
}

export interface SyncResult {
  /// "uploaded" | "downloaded" | "tested"
  status: string;
  /// Bytes transferred.
  bytes: number;
}

export async function listSyncConfigs(): Promise<SyncConfig[]> {
  return invoke<SyncConfig[]>('sync_list');
}

export async function saveSyncConfig(config: SyncConfig): Promise<void> {
  await invoke('sync_save', { config });
}

export async function deleteSyncConfig(provider: SyncProvider): Promise<void> {
  await invoke('sync_delete', { provider });
}

/// Record the result of a sync attempt. `error === null` means success.
export async function recordSyncResult(
  provider: SyncProvider,
  error: string | null,
): Promise<void> {
  await invoke('sync_record_result', { provider, error: error ?? null });
}

/// HEAD (S3) / HEAD (WebDAV) the remote `aag.db` to verify connectivity.
/// Stamps last_sync_at.
export async function syncCheck(config: SyncConfig): Promise<void> {
  await invoke('sync_check', { config });
}

/// PUT local `aag.db` to the remote. Stamps last_sync_at + last_error.
export async function syncUpload(config: SyncConfig): Promise<SyncResult> {
  return invoke<SyncResult>('sync_upload', { config });
}

/// GET the remote `aag.db` and atomically replace the local file.
/// WARNING: this overwrites the entire AAG database. Caller must warn user.
export async function syncDownload(config: SyncConfig): Promise<SyncResult> {
  return invoke<SyncResult>('sync_download', { config });
}
