import assert from 'node:assert/strict';
import { once } from 'node:events';
import { EventEmitter } from 'node:events';
import { spawn } from 'node:child_process';
import { mkdtemp, mkdir, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { StdioClientTransport } from '@modelcontextprotocol/sdk/client/stdio.js';
import test from 'node:test';
import { workflow_owner_request } from './ipc.mjs';
import { create_workflow_server } from './server.mjs';

const node_executable = process.execPath;
const instance_id = '0123456789abcdef0123456789abcdef';

class Linked_transport {
  peer = null;
  onmessage = null;
  onclose = null;
  onerror = null;
  async start() {}
  async send(message) { queueMicrotask(() => this.peer?.onmessage?.(message)); }
  async close() { this.onclose?.(); }
}

function linked_transports() {
  const server_transport = new Linked_transport();
  const client_transport = new Linked_transport();
  server_transport.peer = client_transport;
  client_transport.peer = server_transport;
  return { server_transport, client_transport };
}

class Fixture_socket extends EventEmitter {
  destroyed = false;
  constructor(response_factory, connect = true) {
    super();
    this.response_factory = response_factory;
    if (connect) queueMicrotask(() => this.emit('connect'));
  }
  write(bytes) {
    const request = JSON.parse(bytes.toString('utf8'));
    const response = this.response_factory(request);
    if (response) queueMicrotask(() => this.emit('data', Buffer.from(`${JSON.stringify(response)}\n`)));
  }
  destroy() { this.destroyed = true; }
}

async function write_descriptor(directory) {
  const runtime_dir = path.join(directory, 'runtime');
  await mkdir(runtime_dir, { recursive: true });
  const descriptor_file = path.join(runtime_dir, 'workflow_ipc.json');
  await writeFile(descriptor_file, JSON.stringify({
    schema_version: 1,
    instance_id,
    pipe_name: `\\\\.\\pipe\\aag_workflow_${instance_id}`,
    process_id: process.pid,
  }), { mode: 0o600 });
  return descriptor_file;
}

async function run_stdio_facade() {
  const root = await mkdtemp(path.join(os.tmpdir(), 'aag_workflow_stdio_'));
  const descriptor_file = path.join(root, 'missing.json');
  const transport = new StdioClientTransport({ command: node_executable, args: ['scripts/workflow/server.mjs', '--descriptor-file', descriptor_file], cwd: process.cwd(), stderr: 'pipe' });
  const client = new Client({ name: 'workflow_facade_test', version: '1.0.0' }, { capabilities: {} });
  try {
    await client.connect(transport);
    const listing = await client.listTools();
    assert.ok(listing.tools.every((tool) => tool.annotations?.readOnlyHint === true && tool.annotations?.destructiveHint === false));
    assert.deepEqual(listing.tools.map((tool) => tool.name).sort(), [
      'workflow_get_health', 'workflow_list_capabilities', 'workflow_route_preview',
    ]);
    const missing_owner = await client.callTool({ name: 'workflow_get_health', arguments: {} });
    assert.equal(missing_owner.isError, true);
    assert.equal(missing_owner.content[0].text, 'workflow:owner_unavailable');
    const unknown_arguments = await client.callTool({ name: 'workflow_route_preview', arguments: { task: 'hi', path: 'ignored?' } });
    assert.equal(unknown_arguments.isError, true);
    const mutation = await client.callTool({ name: 'workflow_probe_capability', arguments: { capability_id: 'x' } });
    assert.equal(mutation.isError, true);
    const oversized_task = await client.callTool({ name: 'workflow_route_preview', arguments: { task: '界'.repeat(3000) } });
    assert.equal(oversized_task.isError, true);
    assert.equal(oversized_task.content[0].text, 'workflow:invalid_params');
  } finally {
    await client.close().catch(() => {});
    await rm(root, { recursive: true, force: true });
  }
}

async function test_owner_identity_and_schema() {
  const root = await mkdtemp(path.join(os.tmpdir(), 'aag_workflow_ipc_'));
  try {
    const descriptor_file = await write_descriptor(root);
    let fixture_socket;
    const result = await workflow_owner_request('get_health', {}, {
      descriptor_file,
      platform: 'win32',
      process_check: () => true,
      request_id_factory: () => 'request_1',
      connect: () => (fixture_socket = new Fixture_socket((request) => ({
        schema_version: 1,
        instance_id,
        request_id: request.request_id,
        ok: true,
        result: { schema_version: 1, owner_status: 'connected', observations: [] },
      }))),
    });
    assert.equal(result.owner_status, 'connected');
    assert.equal(fixture_socket.destroyed, true);

    const route_result = await workflow_owner_request('route_preview', { task: 'evidence test' }, {
      descriptor_file,
      platform: 'win32',
      process_check: () => true,
      request_id_factory: () => 'request_2',
      connect: () => new Fixture_socket((request) => ({
        schema_version: 1,
        instance_id,
        request_id: request.request_id,
        ok: true,
        result: {
          schema_version: 1,
          difficulty: 'moderate',
          model: 'final-route',
          fallback_model: '',
          final_gate_model: '',
          reasoning_effort: 'medium',
          score: 0.8,
          signals: [],
          advice_status: 'advisory',
          summary: 'deterministic route retained',
          model_advice: { status: 'accepted', source: 'TypeSafe', model: 'independent-check', confidence: 0.7, summary: 'fallback evidence' },
          jev_evidence: { status: 'low_confidence', source: 'Jev', model: 'jev-choice', confidence: 0.4, summary: 'original evidence' },
        },
      })),
    });
    assert.equal(route_result.jev_evidence.source, 'Jev');
    assert.equal(route_result.jev_evidence.confidence, 0.4);
    assert.equal(route_result.model_advice.source, 'TypeSafe');

    let registry_error_socket;
    await assert.rejects(workflow_owner_request('list_capabilities', {}, {
      descriptor_file, platform: 'win32', process_check: () => true,
      connect: () => (registry_error_socket = new Fixture_socket((request) => ({
        schema_version: 1, instance_id, request_id: request.request_id, ok: false,
        error: { code: 'mcp_registry_unavailable', message: 'arbitrary details are ignored' },
      }))),
    }), /workflow:mcp_registry_unavailable/);
    assert.equal(registry_error_socket.destroyed, true);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
}

async function test_owner_rejects_identity_mismatch_and_times_out() {
  const root = await mkdtemp(path.join(os.tmpdir(), 'aag_workflow_timeout_'));
  try {
    const descriptor_file = await write_descriptor(root);
    let fixture_socket;
    await assert.rejects(workflow_owner_request('get_health', {}, {
      descriptor_file, platform: 'win32', process_check: () => true,
      request_id_factory: () => 'request_2',
      connect: () => (fixture_socket = new Fixture_socket((request) => ({
        schema_version: 1, instance_id, request_id: `${request.request_id}_wrong`, ok: true,
        result: { schema_version: 1, owner_status: 'connected', observations: [] },
      }))),
    }), /workflow:response_invalid/);
    assert.equal(fixture_socket.destroyed, true);

    let oversized_socket;
    await assert.rejects(workflow_owner_request('get_health', {}, {
      descriptor_file, platform: 'win32', process_check: () => true,
      connect: () => (oversized_socket = new Fixture_socket((request) => ({
        schema_version: 1, instance_id, request_id: request.request_id, ok: true,
        result: { schema_version: 1, owner_status: 'x'.repeat(70_000), observations: [] },
      }))),
    }), /workflow:response_too_large/);
    assert.equal(oversized_socket.destroyed, true);

    const stalled_socket = new Fixture_socket(() => null, false);
    await assert.rejects(workflow_owner_request('get_health', {}, {
      descriptor_file, platform: 'win32', process_check: () => true,
      connect_timeout_ms: 5, connect: () => stalled_socket,
    }), /workflow:connect_timeout/);
    assert.equal(stalled_socket.destroyed, true);

    let read_stall;
    await assert.rejects(workflow_owner_request('get_health', {}, {
      descriptor_file, platform: 'win32', process_check: () => true,
      read_timeout_ms: 5, connect: () => (read_stall = new Fixture_socket(() => null)),
    }), /workflow:read_timeout/);
    assert.equal(read_stall.destroyed, true);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
}

test('stdio child exposes only three strict read tools and sanitizes owner absence', async () => {
  await run_stdio_facade();
});

test('catalog and health pages pass validated cursors and preserve order without duplicates', async () => {
  const calls = [];
  const { server_transport, client_transport } = linked_transports();
  const server = create_workflow_server({ owner_request: async (method, params) => {
    calls.push({ method, params: JSON.parse(JSON.stringify(params)) });
    if (method === 'list_capabilities') {
      if (params.cursor === undefined) return { schema_version: 1, capabilities: [capability_fixture('alpha'), capability_fixture('beta')], next_cursor: '2' };
      return { schema_version: 1, capabilities: [capability_fixture('gamma')] };
    }
    if (method === 'get_health') {
      if (params.cursor === undefined) return { schema_version: 1, owner_status: 'available', observations: [health_fixture('alpha'), health_fixture('beta')], next_cursor: '2' };
      return { schema_version: 1, owner_status: 'available', observations: [health_fixture('gamma')] };
    }
    throw new Error(`unexpected method: ${method}`);
  } });
  const client = new Client({ name: 'workflow_paging_test', version: '1.0.0' }, { capabilities: {} });
  try {
    await server.connect(server_transport);
    await client.connect(client_transport);
    const first_page = await client.callTool({ name: 'workflow_list_capabilities', arguments: {} });
    assert.equal(first_page.structuredContent.next_cursor, '2');
    const second_page = await client.callTool({ name: 'workflow_list_capabilities', arguments: { cursor: first_page.structuredContent.next_cursor, limit: 1 } });
    assert.equal(second_page.structuredContent.next_cursor, undefined);
    const all_ids = [...first_page.structuredContent.capabilities, ...second_page.structuredContent.capabilities].map((capability) => capability.id);
    assert.deepEqual(all_ids, ['alpha', 'beta', 'gamma']);
    assert.deepEqual(new Set(all_ids).size, all_ids.length);
    assert.deepEqual(calls, [
      { method: 'list_capabilities', params: {} },
      { method: 'list_capabilities', params: { cursor: '2', limit: 1 } },
    ]);

    const first_health_page = await client.callTool({ name: 'workflow_get_health', arguments: {} });
    assert.equal(first_health_page.structuredContent.next_cursor, '2');
    const second_health_page = await client.callTool({ name: 'workflow_get_health', arguments: { cursor: '2', limit: 1 } });
    assert.equal(second_health_page.structuredContent.next_cursor, undefined);
    const observation_ids = [...first_health_page.structuredContent.observations, ...second_health_page.structuredContent.observations]
      .map((observation) => observation.capability_id);
    assert.deepEqual(observation_ids, ['alpha', 'beta', 'gamma']);
    assert.deepEqual(new Set(observation_ids).size, observation_ids.length);
    assert.deepEqual(calls.slice(-2), [
      { method: 'get_health', params: {} },
      { method: 'get_health', params: { cursor: '2', limit: 1 } },
    ]);

    assert.equal((await client.callTool({ name: 'workflow_get_health', arguments: { limit: 17 } })).isError, true);
    assert.equal((await client.callTool({ name: 'workflow_list_capabilities', arguments: { cursor: 'offset' } })).isError, true);
    assert.equal((await client.callTool({ name: 'workflow_list_capabilities', arguments: { cursor: '1\n' } })).isError, true);
    assert.equal(calls.length, 4);
  } finally {
    await client.close().catch(() => {});
    await server.close().catch(() => {});
  }
});

test('pipe fixture binds instance and request identity and validates DTOs', async () => {
  await test_owner_identity_and_schema();
});

test('pipe fixture enforces identity, frame bounds, deadlines, and socket cleanup', async () => {
  await test_owner_rejects_identity_mismatch_and_times_out();
});

function capability_fixture(id) {
  return {
    id, name: id, kind: 'workflow', group: 'test', owner: 'fixture', access_method: 'read',
    host_scope: 'local', source_ref: 'fixture', mcp_server_id: null, summary: 'fixture',
    probe_supported: false, observations: [],
  };
}

function health_fixture(capability_id) {
  return {
    capability_id, stage: 'installed', status: 'unknown', observed_at: null, expires_at: null,
    evidence_source: 'fixture', summary: 'fixture observation',
  };
}

test('oversized raw stdio frame closes cleanly without writing protocol noise', async () => {
  const child = spawn(node_executable, ['scripts/workflow/server.mjs', '--descriptor-file', path.join(os.tmpdir(), 'missing_workflow_descriptor.json')], {
    cwd: process.cwd(),
    stdio: ['pipe', 'pipe', 'pipe'],
    windowsHide: true,
  });
  const stdout_chunks = [];
  const stderr_chunks = [];
  child.stdout.on('data', (chunk) => stdout_chunks.push(chunk));
  child.stderr.on('data', (chunk) => stderr_chunks.push(chunk));
  child.stdin.on('error', () => {});
  child.stdin.end(Buffer.alloc(70 * 1024, 0x78));
  let timeout_handle;
  const exit_result = await Promise.race([
    once(child, 'exit'),
    new Promise((_, reject) => { timeout_handle = setTimeout(() => reject(new Error('oversized stdio child did not terminate')), 3000); }),
  ]).finally(() => clearTimeout(timeout_handle));
  assert.notEqual(exit_result[0], null);
  assert.equal(Buffer.concat(stdout_chunks).length, 0);
  assert.equal(Buffer.concat(stderr_chunks).length, 0);
});
