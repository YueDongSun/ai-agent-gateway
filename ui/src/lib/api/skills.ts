/// Skills Tauri command wrappers.
/// Commands defined in `src-tauri/src/lib.rs` (skill_list, skill_save, skill_delete).
import { invoke } from '@tauri-apps/api/core';

export interface InstalledSkill {
  id: string;
  name: string;
  description?: string | null;
  directory: string;
  repoOwner?: string | null;
  repoName?: string | null;
  repoBranch?: string | null;
  contentHash?: string | null;
  enabledClaude: boolean;
  enabledCodex: boolean;
  installedAt: number;
  updatedAt: number;
}

export async function listSkills(): Promise<InstalledSkill[]> {
  return invoke<InstalledSkill[]>('skill_list');
}

export async function saveSkill(skill: InstalledSkill): Promise<void> {
  await invoke('skill_save', { skill });
}

export async function deleteSkill(id: string): Promise<void> {
  await invoke('skill_delete', { id });
}