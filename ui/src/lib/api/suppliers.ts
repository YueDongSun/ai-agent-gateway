/// Typed Tauri command wrappers for supplier management.
///
/// Command names and payload shapes mirror `src-tauri/src/commands/suppliers.rs`
/// and `src-tauri/src/services/suppliers/types.rs`. Credential values are
/// write-only: reads never carry keys, and `apiKey` / `apiKeyOverride`
/// omission preserves the stored value while an explicit empty string clears
/// it (see `suppliers_logic.ts`).
import { invoke } from '@tauri-apps/api/core';
import type {
  Model_catalog_entry,
  Supplier_catalog_status,
  Supplier_catalog_view,
  Supplier_discover_models_input,
  Supplier_get_result,
  Supplier_import_apply_result,
  Supplier_import_preview,
  Supplier_list_view,
  Supplier_models_update_input,
  Supplier_mutation_result,
  Supplier_delete_result,
  Supplier_save_input,
  Supplier_status,
} from './suppliers_logic.js';

export * from './suppliers_logic.js';

// ---------------------------------------------------------------------------
// Implemented IPC commands (registered by the application integration layer)
// ---------------------------------------------------------------------------

/// Secret-free supplier list plus the configuration generation snapshot.
export async function supplier_list(): Promise<Supplier_list_view> {
  return invoke<Supplier_list_view>('supplier_list');
}

export async function supplier_get(id: string): Promise<Supplier_get_result> {
  return invoke<Supplier_get_result>('supplier_get', { id });
}

/// Optimistic-generation upsert. Omit `id` to create; omit `apiKey` to keep
/// the stored key; pass `apiKey: ''` to clear it.
export async function supplier_save(input: Supplier_save_input): Promise<Supplier_mutation_result> {
  return invoke<Supplier_mutation_result>('supplier_save', { input });
}

export async function supplier_delete(
  id: string,
  expectedGeneration: number,
): Promise<Supplier_delete_result> {
  return invoke<Supplier_delete_result>('supplier_delete', { id, expectedGeneration });
}

/// Replace one endpoint's model inventory and manual capability overrides.
export async function supplier_models_update(
  input: Supplier_models_update_input,
): Promise<Supplier_mutation_result> {
  return invoke<Supplier_mutation_result>('supplier_models_update', { input });
}

/// Probe one protocol endpoint's advertised model catalog without writing.
/// With `supplierId`, omitted fields (credentials included) fall back to the
/// stored supplier/endpoint state; without it, only the typed values are
/// used. Returns exact model ids, sorted and deduplicated.
export async function supplier_discover_models(
  input: Supplier_discover_models_input,
): Promise<string[]> {
  return invoke<string[]>('supplier_discover_models', { input });
}

export async function supplier_status(): Promise<Supplier_status> {
  return invoke<Supplier_status>('supplier_status');
}

// ---------------------------------------------------------------------------
// Import and public catalog commands. Import previews are read-only; catalog
// assessments use cached public documents and never trigger paid requests.
// ---------------------------------------------------------------------------

/// Redacted, zero-write preview of a cc-switch import. `source_path`
/// overrides the default cc-switch location (tests and non-default installs).
export async function supplier_import_preview(source_path?: string): Promise<Supplier_import_preview> {
  return invoke<Supplier_import_preview>('supplier_import_preview', {
    sourcePath: source_path ?? null,
  });
}

/// Apply a previewed plan in one transaction. The plan id binds the source
/// fingerprint and the target configuration generation; any drift is
/// refused without writing.
export async function supplier_import_apply(
  plan_id: string,
  source_path?: string,
): Promise<Supplier_import_apply_result> {
  return invoke<Supplier_import_apply_result>('supplier_import_apply', {
    planId: plan_id,
    sourcePath: source_path ?? null,
  });
}

/// Cached public catalog sources (models.dev metadata, EvalPlus scores).
export async function supplier_catalog_status(): Promise<Supplier_catalog_status> {
  return invoke<Supplier_catalog_status>('supplier_catalog_status');
}

/// Refresh one public catalog source; failures keep the previous cache and
/// mark it stale instead of dropping it.
export async function supplier_catalog_refresh(source: string): Promise<Supplier_catalog_view> {
  return invoke<Supplier_catalog_view>('supplier_catalog_refresh', { input: { source } });
}

export async function model_catalog_model(selector: string): Promise<Model_catalog_entry> {
  return invoke<Model_catalog_entry>('model_catalog_model', { selector });
}
