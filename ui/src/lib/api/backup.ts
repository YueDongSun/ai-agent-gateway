/// Backup Tauri command wrappers.
/// Commands are defined in `src-tauri/src/lib.rs` (backup_create, backup_list, backup_delete).
import { invoke } from '@tauri-apps/api/core';

export interface BackupEntry {
  id: string;
  label: string;
  sizeBytes: number;
  createdAt: string; // ISO 8601
}

export async function createBackup(label?: string): Promise<BackupEntry> {
  return invoke<BackupEntry>('backup_create', { label: label ?? null });
}

export async function listBackups(): Promise<BackupEntry[]> {
  return invoke<BackupEntry[]>('backup_list');
}

export async function deleteBackup(id: string): Promise<void> {
  await invoke('backup_delete', { id });
}
