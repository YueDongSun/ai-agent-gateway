import type { AppConfig, AgentTarget, AgentId } from '../config/schema.js';
import { existsSync } from 'node:fs';
import { defaultAgentTargets, createAdapter, type AgentAdapter, type SwitchResult } from './adapters.js';
import { logger } from '../util/log.js';

/**
 * The Switcher rewrites agent config files to point either at the local
 * gateway or at a direct provider. Conceptually like cc-switch, but aware
 * of every supported agent.
 */
export class Switcher {
  constructor(private cfg: AppConfig) {
    if (!cfg.agents.length) cfg.agents = defaultAgentTargets();
  }

  listAgents(): AgentTarget[] {
    return this.cfg.agents;
  }

  getAgent(id: AgentId): AgentTarget | undefined {
    return this.cfg.agents.find((a) => a.id === id);
  }

  adapter(id: AgentId): AgentAdapter {
    const t = this.getAgent(id);
    if (!t) throw new Error(`unknown agent "${id}"`);
    return createAdapter(t);
  }

  /** Point an agent at the local gateway. */
  pointToGateway(id: AgentId, model: string): SwitchResult {
    const adapter = this.adapter(id);
    const path = adapter.resolveConfigPath();
    if (!path) return { ok: false, agent: id, path: '', message: 'no config path resolved' };

    const gwURL = `http://${this.cfg.gateway.host}:${this.cfg.gateway.port}`;

    try {
      const content = adapter.buildGatewayConfig(gwURL, this.cfg.gateway.authToken, model);
      adapter.write(content);
      const t = this.getAgent(id)!;
      t.pointedAtGateway = true;
      t.activeProfileId = '';
      logger.info('switch', `${id} -> gateway (${gwURL}) at ${path}`);
      return { ok: true, agent: id, path, message: `pointed at gateway ${gwURL}` };
    } catch (e) {
      return { ok: false, agent: id, path, message: (e as Error).message };
    }
  }

  /** Point an agent directly at a provider's API. */
  pointToProvider(id: AgentId, baseURL: string, apiKey: string, model: string): SwitchResult {
    const adapter = this.adapter(id);
    const path = adapter.resolveConfigPath();
    if (!path) return { ok: false, agent: id, path: '', message: 'no config path resolved' };

    try {
      const content = adapter.buildDirectConfig(baseURL, apiKey, model);
      adapter.write(content);
      const t = this.getAgent(id)!;
      t.pointedAtGateway = false;
      t.activeProfileId = '';
      logger.info('switch', `${id} -> direct ${baseURL} at ${path}`);
      return { ok: true, agent: id, path, message: `pointed at ${baseURL}` };
    } catch (e) {
      return { ok: false, agent: id, path, message: (e as Error).message };
    }
  }

  /** Apply a profile to an agent (profile carries baseURL+key+model). */
  pointToProfile(id: AgentId, profileId: string, model: string): SwitchResult {
    const profile = this.cfg.profiles.find((p) => p.id === profileId);
    if (!profile) return { ok: false, agent: id, path: '', message: `profile "${profileId}" not found` };

    const result = this.pointToProvider(id, profile.baseURL, profile.apiKey, model);
    if (result.ok) {
      const t = this.getAgent(id)!;
      t.activeProfileId = profileId;
    }
    return result;
  }

  /** Show the current target for an agent. */
  status(id: AgentId): { pointedAtGateway: boolean; baseURL?: string; path?: string } {
    const adapter = this.adapter(id);
    const t = this.getAgent(id)!;
    return {
      pointedAtGateway: t.pointedAtGateway,
      baseURL: adapter.detectCurrentBaseURL(),
      path: t.configPaths.find((x) => existsSync(x)),
    };
  }

  /** Point ALL detected agents at the gateway. */
  pointAllToGateway(model: string): SwitchResult[] {
    return this.cfg.agents.map((a) => this.pointToGateway(a.id, model));
  }
}
