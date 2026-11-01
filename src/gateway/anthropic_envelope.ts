/** Internal source envelopes live outside enumerable/public protocol objects. */
import type { ChatCompletionRequest } from '../config/schema.js';
import type { OAIChatResponse, OAIStreamChunk } from './canonical.js';
import type { AnthropicRequest, AnthropicResponse } from './converter.js';

export const anthropic_requests = new WeakMap<ChatCompletionRequest, AnthropicRequest>();
export const anthropic_responses = new WeakMap<OAIChatResponse, AnthropicResponse>();
export const anthropic_events = new WeakMap<OAIStreamChunk, Record<string, any>>();
