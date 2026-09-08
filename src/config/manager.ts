import { randomBytes } from 'node:crypto';
import type { AppConfig, Profile, ProviderConfig, RouteConfig, RouteHop } from './schema.js';

/**
 * In-memory mutation helpers. Callers load -> mutate -> save.
 * Every method returns the *new* config (immutable-ish) so callers can
 * `saveConfig(mgr.someOp(cfg))`.
 */
export class ConfigManager {
  constructor(private cfg: AppConfig) {}

  get raw(): AppConfig {
    return this.cfg;
  }

  /* ---- gateway settings ----------------------------------------- */
  setGateway(opts: Partial<AppConfig['gateway']>) {
    this.cfg.gateway = { ...this.cfg.gateway, ...opts };
    return this.cfg;
  }

  /* ---- providers ------------------------------------------------ */
  addProvider(p: Omit<ProviderConfig, 'id'> & { id?: string }): ProviderConfig {
    const id = p.id ?? `prov-${randomBytes(3).toString('hex')}`;
    const prov = { ...p, id } as ProviderConfig;
    this.cfg.providers.push(prov);
    return prov;
  }

  updateProvider(id: string, patch: Partial<ProviderConfig>) {
    const i = this.cfg.providers.findIndex((p) => p.id === id);
    if (i < 0) throw new Error(`provider "${id}" not found`);
    this.cfg.providers[i] = { ...this.cfg.providers[i], ...patch, id };
    return this.cfg.providers[i];
  }

  removeProvider(id: string) {
    this.cfg.providers = this.cfg.providers.filter((p) => p.id !== id);
    this.cfg.routes = this.cfg.routes.map((r) => ({
      ...r,
      hops: r.hops.filter((h) => h.provider !== id),
    })).filter((r) => r.hops.length > 0);
    return this.cfg;
  }

  /* ---- routes --------------------------------------------------- */
  addRoute(alias: string, hops: RouteHop[], description = ''): RouteConfig {
    const existing = this.cfg.routes.find((r) => r.alias === alias);
    if (existing) {
      existing.hops = hops;
      existing.description = description;
      return existing;
    }
    const route: RouteConfig = { alias, hops, description };
    this.cfg.routes.push(route);
    return route;
  }

  removeRoute(alias: string) {
    this.cfg.routes = this.cfg.routes.filter((r) => r.alias !== alias);
    return this.cfg;
  }

  /* ---- profiles ------------------------------------------------- */
  addProfile(p: Omit<Profile, 'id'> & { id?: string }): Profile {
    const id = p.id ?? `prof-${randomBytes(3).toString('hex')}`;
    const prof: Profile = { ...p, id } as Profile;
    this.cfg.profiles.push(prof);
    return prof;
  }

  removeProfile(id: string) {
    this.cfg.profiles = this.cfg.profiles.filter((p) => p.id !== id);
    return this.cfg;
  }

  /* ---- agents --------------------------------------------------- */
  ensureAgents() {
    if (!this.cfg.agents.length) {
      // lazy import to avoid cycle
      const { defaultAgentTargets } = require('../agents/adapters.js') as typeof import('../agents/adapters.js');
      this.cfg.agents = defaultAgentTargets();
    }
    return this.cfg.agents;
  }
}
