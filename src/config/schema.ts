import { z } from 'zod';

/* ------------------------------------------------------------------ */
/*  Provider configuration                                             */
/* ------------------------------------------------------------------ */

export const ProviderType = z.enum(['openai', 'anthropic', 'gemini', 'mock']);
export type ProviderType = z.infer<typeof ProviderType>;

export const CircuitBreakerConfig = z.object({
  failureThreshold: z.number().int().min(1).default(5),
  cooldownMs: z.number().int().min(1000).default(30_000),
  halfOpenRetries: z.number().int().min(1).default(1),
  successThreshold: z.number().int().min(1).default(2),
});
export type CircuitBreakerConfig = z.infer<typeof CircuitBreakerConfig>;

export const ProviderConfig = z.object({
  id: z.string().min(1),
  name: z.string().min(1),
  type: ProviderType.default('openai'),
  apiKey: z.string().default(''),
  baseURL: z.string().default(''),
  models: z.array(z.string()).default([]),
  timeoutMs: z.number().int().min(1000).default(60_000),
  enabled: z.boolean().default(true),
  circuitBreaker: CircuitBreakerConfig.default({}),
  // weight for load balancing among equally-ranked providers
  weight: z.number().int().min(1).default(1),
});
export type ProviderConfig = z.infer<typeof ProviderConfig>;

/* ------------------------------------------------------------------ */
/*  Route = model alias -> ordered provider fallback chain             */
/* ------------------------------------------------------------------ */

export const RouteHop = z.object({
  provider: z.string(), // provider id
  model: z.string(), // model name as known by that provider
});
export type RouteHop = z.infer<typeof RouteHop>;

export const RouteConfig = z.object({
  alias: z.string().min(1), // the model name clients request, e.g. "claude-sonnet-4"
  hops: z.array(RouteHop).min(1),
  description: z.string().default(''),
});
export type RouteConfig = z.infer<typeof RouteConfig>;

/* ------------------------------------------------------------------ */
/*  Agent target definitions                                           */
/* ------------------------------------------------------------------ */

export const AgentId = z.enum([
  'copilot',
  'kilo',
  'cursor',
  'zcode',
  'minimax',
  'kimi',
]);
export type AgentId = z.infer<typeof AgentId>;

export const AgentTarget = z.object({
  id: AgentId,
  name: z.string(),
  format: z.enum(['openai', 'anthropic']).default('openai'),
  // candidate config file paths (first existing/writable wins)
  configPaths: z.array(z.string()).default([]),
  // whether this agent is currently pointed at the gateway
  pointedAtGateway: z.boolean().default(false),
  // last applied profile id
  activeProfileId: z.string().default(''),
});
export type AgentTarget = z.infer<typeof AgentTarget>;

/* ------------------------------------------------------------------ */
/*  Profile = a reusable provider credential set                        */
/* ------------------------------------------------------------------ */

export const Profile = z.object({
  id: z.string().min(1),
  name: z.string().min(1),
  type: ProviderType.default('openai'),
  apiKey: z.string().default(''),
  baseURL: z.string().default(''),
  models: z.array(z.string()).default([]),
  notes: z.string().default(''),
});
export type Profile = z.infer<typeof Profile>;

/* ------------------------------------------------------------------ */
/*  Top-level application config                                        */
/* ------------------------------------------------------------------ */

export const AppConfig = z.object({
  version: z.number().default(1),
  gateway: z.object({
    host: z.string().default('127.0.0.1'),
    port: z.number().int().min(1).max(65535).default(8787),
    authToken: z.string().default(''),
    logLevel: z.enum(['debug', 'info', 'warn', 'error']).default('info'),
  }),
  providers: z.array(ProviderConfig).default([]),
  routes: z.array(RouteConfig).default([]),
  profiles: z.array(Profile).default([]),
  agents: z.array(AgentTarget).default([]),
});
export type AppConfig = z.infer<typeof AppConfig>;

/* ------------------------------------------------------------------ */
/*  Chat request / response shapes (loosely-typed for flexibility)     */
/* ------------------------------------------------------------------ */

export interface ChatMessage {
  role: string;
  content: unknown;
  [k: string]: unknown;
}

export interface ChatCompletionRequest {
  model: string;
  messages: ChatMessage[];
  stream?: boolean;
  temperature?: number;
  max_tokens?: number;
  [k: string]: unknown;
}
