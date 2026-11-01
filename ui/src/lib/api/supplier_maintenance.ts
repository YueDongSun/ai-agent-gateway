import { invoke } from '@tauri-apps/api/core';

export interface Supplier_maintenance_result {
  generation: number;
  removedOauthSuppliers: number;
  removedOauthProviders: number;
  mergedSuppliers: number;
  backupCreated: boolean;
  preservedStaticSuppliers?: number;
  mergeConflictCount?: number;
  mergeConflictCodes?: string[];
  setupError?: string | null;
}

/** Only application-owned imported records are eligible. The native service
 * makes a private recovery snapshot before its transactional mutation. */
export function supplier_import_maintain(): Promise<Supplier_maintenance_result> {
  return invoke<Supplier_maintenance_result>('supplier_import_maintain');
}
