import { useMemo } from 'react';
import { Arena_chart as Model_arena_chart } from '../arena/arena_chart.js';
import type { Supplier_view } from '../../lib/api/suppliers.js';

/** Inventory projection stays in Suppliers; the shared chart receives model selectors only. */
export function Arena_chart({ suppliers, on_state }: { suppliers: Supplier_view[]; on_state?: (state: string) => void }) {
  const model_options = useMemo(() => [...new Set(suppliers.flatMap((supplier) =>
    supplier.endpoints.flatMap((endpoint) => endpoint.models.map((model) => model.modelId))))]
    .sort().map((model) => ({ selector: `precise:${model}`, label: model })), [suppliers]);
  return <Model_arena_chart model_options={model_options} on_state={on_state} />;
}
