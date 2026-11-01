import type { AppType } from './types.js';
import { getCurrentProvider, getProvider } from './providers.js';
import { project_router_models, type Router_model } from '../client_router_models.js';

export interface Client_router_plan {
  app_type: AppType;
  models: Router_model[];
}

/** The existing selected-record APIs remain unchanged. This boundary discards
 * all supply metadata before returning model-only data to the client view. */
export async function get_current_client_plan(app_type: AppType): Promise<Client_router_plan> {
  const selected_id = await getCurrentProvider(app_type);
  if (!selected_id) return { app_type, models: [] };
  const selected = await getProvider(selected_id, app_type);
  // A concurrent switch must not be presented as the current configuration.
  if (await getCurrentProvider(app_type) !== selected_id) throw new Error('client_plan_changed');
  return { app_type, models: project_router_models(app_type, selected?.settingsConfig) };
}
