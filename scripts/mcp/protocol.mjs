import { feature_scopes, safety_workflows, verification_plan } from '../verification/feature_catalog.mjs';

export const protocol_version = '2025-11-25';
const supported_versions = [protocol_version, '2025-06-18', '2025-03-26'];
const annotations = { readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false };
const tools = [
  { name: 'aag_capabilities', description: 'Read the AAG companion capability and safety boundary. Does not inspect live application state.', inputSchema: { type: 'object', properties: {}, additionalProperties: false }, annotations },
  { name: 'aag_verification_plan', description: 'Return allowlisted local verification commands for a feature. Plans only; cannot execute commands or approve writes.', inputSchema: { type: 'object', properties: { scope: { type: 'string', enum: feature_scopes } }, required: ['scope'], additionalProperties: false }, annotations },
  { name: 'aag_workflow', description: 'Read an AAG MCP, DSH, or verification workflow and its safety boundaries. No file access.', inputSchema: { type: 'object', properties: { topic: { type: 'string', enum: Object.keys(safety_workflows) } }, required: ['topic'], additionalProperties: false }, annotations },
];
const object = (value) => Boolean(value) && typeof value === 'object' && !Array.isArray(value);
const rpc_error = (id, code, message) => ({ jsonrpc: '2.0', id, error: { code, message } });
const tool_result = (value) => ({ content: [{ type: 'text', text: JSON.stringify(value, null, 2) }], isError: false });

export function create_session() {
  let state = 'new';
  return function handle(message) {
    if (!object(message) || message.jsonrpc !== '2.0' || typeof message.method !== 'string') return rpc_error(null, -32600, 'Invalid Request');
    const notification = !Object.hasOwn(message, 'id');
    if (!notification && !(typeof message.id === 'string' || Number.isSafeInteger(message.id))) return rpc_error(null, -32600, 'Invalid Request');
    const id = message.id;
    const params = message.params ?? {};
    if (!object(params)) return notification ? null : rpc_error(id, -32602, 'Invalid params');
    if (notification) {
      if (message.method === 'notifications/initialized' && state === 'initializing') state = 'ready';
      return null;
    }
    if (message.method === 'ping') return { jsonrpc: '2.0', id, result: {} };
    if (message.method === 'initialize') {
      if (state !== 'new') return rpc_error(id, -32600, 'Already initialized');
      if (typeof params.protocolVersion !== 'string' || !object(params.capabilities) || !object(params.clientInfo) || typeof params.clientInfo.name !== 'string' || typeof params.clientInfo.version !== 'string') return rpc_error(id, -32602, 'Invalid initialization params');
      state = 'initializing';
      return { jsonrpc: '2.0', id, result: { protocolVersion: supported_versions.includes(params.protocolVersion) ? params.protocolVersion : protocol_version,
        capabilities: { tools: { listChanged: false } }, serverInfo: { name: 'aag_companion', version: '1.0.0' },
        instructions: 'Read-only repository assistance. Plans are not authorization. No live AAG state, credentials, commands, network requests, or configuration writes are available.' } };
    }
    if (state !== 'ready') return rpc_error(id, -32002, 'Initialize before using tools');
    if (message.method === 'tools/list') {
      if (params.cursor !== undefined) return rpc_error(id, -32602, 'Pagination is not supported');
      return { jsonrpc: '2.0', id, result: { tools } };
    }
    if (message.method !== 'tools/call') return rpc_error(id, -32601, 'Method not found');
    const definition = tools.find((tool) => tool.name === params.name);
    if (!definition) return rpc_error(id, -32602, 'Unknown tool');
    const args = params.arguments ?? {};
    if (!object(args) || Object.keys(args).some((key) => !Object.hasOwn(definition.inputSchema.properties, key))) return rpc_error(id, -32602, 'Invalid tool arguments');
    let value;
    if (params.name === 'aag_capabilities') value = { schema_version: 1, mode: 'read_only', tools: tools.map((tool) => tool.name), scopes: feature_scopes,
      prohibited_actions: ['execute_commands', 'read_arbitrary_files', 'read_live_state', 'read_credentials', 'write_configuration', 'approve_operations', 'network_requests', 'install_packages'] };
    else if (params.name === 'aag_verification_plan') {
      if (!feature_scopes.includes(args.scope)) return rpc_error(id, -32602, 'Invalid scope');
      value = verification_plan(args.scope);
    } else {
      if (typeof args.topic !== 'string' || !Object.hasOwn(safety_workflows, args.topic)) return rpc_error(id, -32602, 'Invalid topic');
      value = safety_workflows[args.topic];
    }
    return { jsonrpc: '2.0', id, result: tool_result(value) };
  };
}

export const parse_error = () => rpc_error(null, -32700, 'Parse error');
export const size_error = () => rpc_error(null, -32600, 'Message exceeds the 64 KiB limit');
