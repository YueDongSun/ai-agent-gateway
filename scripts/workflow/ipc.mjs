import { randomUUID } from 'node:crypto';
import net from 'node:net';
import { z } from 'zod';
import { read_descriptor, validate_process_alive, workflow_config_limits, cursor_schema, instance_id_pattern, Workflow_error } from './config.mjs';

const schema_version = z.literal(1);
const observation_schema = z.object({
  stage: z.string(),
  status: z.string(),
  observed_at: z.string().nullable(),
  expires_at: z.string().nullable(),
  evidence_source: z.string(),
  summary: z.string(),
}).strict();
const capability_schema = z.object({
  id: z.string(), name: z.string(), kind: z.string(), group: z.string(), owner: z.string(),
  access_method: z.string(), host_scope: z.string(), source_ref: z.string(),
  mcp_server_id: z.string().nullable(), summary: z.string(), probe_supported: z.boolean(),
  observations: z.array(observation_schema),
}).strict();
const capability_list_schema = z.object({
  schema_version,
  capabilities: z.array(capability_schema).max(16),
  next_cursor: cursor_schema.optional(),
}).strict();
const health_observation_schema = observation_schema.extend({ capability_id: z.string() }).strict();
const health_schema = z.object({
  schema_version,
  owner_status: z.string(),
  observations: z.array(health_observation_schema).max(16),
  next_cursor: cursor_schema.optional(),
}).strict();
const model_advice_schema = z.object({
  status: z.string(), source: z.string(), model: z.string(), confidence: z.number().nullable(), summary: z.string(),
}).strict();
const route_preview_schema = z.object({
  schema_version,
  difficulty: z.string(), model: z.string(), fallback_model: z.string(), final_gate_model: z.string(),
  reasoning_effort: z.string(), score: z.number(), signals: z.array(z.string()), advice_status: z.string(),
  summary: z.string(), model_advice: model_advice_schema.optional(), jev_evidence: model_advice_schema.optional(),
}).strict();
const workflow_page_params_schema = z.object({
  cursor: cursor_schema.optional(),
  limit: z.number().int().min(1).max(16).optional(),
}).strict();
const response_schema = z.object({
  schema_version,
  instance_id: z.string().regex(instance_id_pattern),
  request_id: z.string().min(1).max(64),
  ok: z.boolean(),
  result: z.unknown().optional(),
  error: z.object({ code: z.string().regex(/^[a-z_]{1,64}$/), message: z.string().max(256) }).strict().optional(),
}).strict().refine((value) => value.ok ? value.result !== undefined && value.error === undefined : value.error !== undefined && value.result === undefined);
const allowed_methods = Object.freeze({
  list_capabilities: capability_list_schema,
  get_health: health_schema,
  route_preview: route_preview_schema,
});
const stable_owner_codes = new Set([
  'owner_unavailable', 'connect_timeout', 'read_timeout', 'write_timeout', 'response_invalid',
  'response_too_large', 'malformed_frame', 'malformed_request', 'unsupported_version', 'stale_instance',
  'invalid_request_id', 'invalid_params', 'method_not_allowed', 'truncated_frame', 'read_failed',
  'write_failed', 'frame_too_large', 'dispatch_timeout', 'invalid_cursor',
  'mcp_registry_unavailable', 'state_unavailable', 'catalog_invalid', 'catalog_schema_invalid',
  'capability_id_invalid', 'capability_not_found', 'unsupported_probe_id', 'config_invalid', 'config_unreadable',
  'config_too_large', 'config_not_private', 'config_path_invalid', 'configured_executable_invalid',
  'jev_model_invalid', 'jev_confidence_floor_invalid', 'project_dir_invalid', 'process_timeout',
  'process_spawn_failed', 'process_failed', 'process_output_limit', 'process_output_failed', 'process_stdin_failed',
  'process_stdin_unavailable', 'process_stdout_unavailable', 'process_wait_failed', 'route_worker_failed',
  'process_job_assign_failed', 'process_job_create_failed', 'process_temp_unavailable', 'runtime_root_unavailable',
  'descriptor_write_failed', 'instance_id_unavailable', 'pipe_acl_unavailable', 'pipe_create_failed',
  'pipe_identity_unavailable', 'probe_worker_failed', 'owner_status_unavailable', 'descriptor_invalid',
]);

function stable_error(code) {
  const normalized = stable_owner_codes.has(code) ? code : 'owner_unavailable';
  return new Workflow_error(`workflow:${normalized}`);
}

function validate_result(method, result) {
  const parsed = allowed_methods[method].safeParse(result);
  if (!parsed.success) throw stable_error('response_invalid');
  return parsed.data;
}

function read_bounded_line(socket, timeout_ms, max_bytes) {
  return new Promise((resolve, reject) => {
    let bytes = 0;
    let chunks = [];
    const timer = setTimeout(() => finish(new Error('read_timeout')), timeout_ms);
    function cleanup() {
      clearTimeout(timer);
      socket.off('data', on_data);
      socket.off('error', on_error);
      socket.off('end', on_end);
    }
    function finish(error, value) {
      cleanup();
      error ? reject(error) : resolve(value);
    }
    function on_data(chunk) {
      const newline = chunk.indexOf(10);
      if (newline !== -1 && newline !== chunk.length - 1) return finish(new Error('malformed_frame'));
      const line_part = newline === -1 ? chunk : chunk.subarray(0, newline);
      bytes += line_part.length + (newline === -1 ? 0 : 1);
      if (bytes > max_bytes) return finish(new Error('response_too_large'));
      chunks.push(line_part);
      if (newline !== -1) finish(null, Buffer.concat(chunks, bytes).subarray(0, bytes - 1));
    }
    function on_error() { finish(new Error('read_failed')); }
    function on_end() { finish(new Error('truncated_frame')); }
    socket.on('data', on_data);
    socket.once('error', on_error);
    socket.once('end', on_end);
  });
}

export async function workflow_owner_request(method, params, options = {}) {
  if (process.platform !== 'win32' && options.platform !== 'win32') throw stable_error('owner_unavailable');
  const descriptor_file = options.descriptor_file;
  let descriptor;
  try {
    descriptor = await read_descriptor(descriptor_file, options);
  } catch {
    throw stable_error('owner_unavailable');
  }
  const process_check = options.process_check ?? validate_process_alive;
  if (!process_check(descriptor.process_id)) throw stable_error('owner_unavailable');
  if (!Object.hasOwn(allowed_methods, method)) throw stable_error('method_not_allowed');
  if ((method === 'list_capabilities' || method === 'get_health') && !workflow_page_params_schema.safeParse(params).success) throw stable_error('invalid_params');
  const request_id = (options.request_id_factory ?? randomUUID)().replaceAll('-', '').slice(0, 32);
  const request = {
    schema_version: 1,
    instance_id: descriptor.instance_id,
    request_id,
    method,
    params,
  };
  const request_bytes = Buffer.from(`${JSON.stringify(request)}\n`, 'utf8');
  if (request_bytes.length > workflow_config_limits.max_frame_bytes) throw stable_error('invalid_params');

  const connect = options.connect ?? ((pipe_name) => net.createConnection(pipe_name));
  const request_started_at = Date.now();
  const socket = connect(descriptor.pipe_name);
  const connect_timeout = options.connect_timeout_ms ?? 5000;
  const read_timeout = method === 'route_preview'
    ? () => Math.max(1, (options.route_timeout_ms ?? 12000) - (Date.now() - request_started_at))
    : () => options.read_timeout_ms ?? 5000;
  try {
    await new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error('connect_timeout')), connect_timeout);
      socket.once('connect', () => { clearTimeout(timer); resolve(); });
      socket.once('error', () => { clearTimeout(timer); reject(new Error('connect_failed')); });
    });
    socket.write(request_bytes);
    const response_bytes = await read_bounded_line(socket, read_timeout(), workflow_config_limits.max_frame_bytes);
    let response_value;
    try { response_value = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(response_bytes)); }
    catch { throw stable_error('response_invalid'); }
    const response_result = response_schema.safeParse(response_value);
    if (!response_result.success || response_result.data.instance_id !== descriptor.instance_id || response_result.data.request_id !== request_id) {
      throw stable_error('response_invalid');
    }
    if (!response_result.data.ok) throw stable_error(response_result.data.error.code);
    return validate_result(method, response_result.data.result);
  } catch (error) {
    if (error instanceof Workflow_error) throw error;
    const code = error?.message;
    throw stable_error(stable_owner_codes.has(code) ? code : 'owner_unavailable');
  } finally {
    socket.destroy();
  }
}
