import { client_page_order } from './client_apps.js';
import type { AppType } from './api/types.js';

export type Workspace_view = AppType | 'dsh' | 'mcp' | 'prompt' | 'suppliers' | 'usage';
export const workspace_views: readonly Workspace_view[] = [...client_page_order, 'suppliers', 'usage', 'prompt', 'mcp', 'dsh'];

export function parse_workspace_route(hash: string): Workspace_view | null {
  const candidate = hash.replace(/^#\//, '');
  return workspace_views.find((view) => view === candidate) ?? null;
}
