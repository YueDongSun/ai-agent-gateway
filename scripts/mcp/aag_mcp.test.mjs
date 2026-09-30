import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { create_session, protocol_version } from './protocol.mjs';
import { verification_plan } from '../verification/feature_catalog.mjs';

const rpc = (id, method, params = {}) => ({ jsonrpc: '2.0', id, method, params });
const initialize = rpc(1, 'initialize', { protocolVersion: protocol_version, capabilities: {}, clientInfo: { name: 'test_client', version: '1' } });
const initialized = { jsonrpc: '2.0', method: 'notifications/initialized' };
function ready() { const session = create_session(); session(initialize); session(initialized); return session; }

test('requires initialize/initialized and negotiates a declared protocol', () => {
  const session = create_session();
  assert.equal(session(rpc(0, 'tools/list')).error.code, -32002);
  assert.equal(session(initialize).result.protocolVersion, protocol_version);
  assert.equal(session(rpc(2, 'tools/list')).error.code, -32002);
  assert.equal(session(initialized), null);
  assert.equal(session(rpc(2, 'tools/list')).result.tools.length, 3);
  assert.equal(session(initialize).error.code, -32600);
  const future = create_session()({ ...initialize, params: { ...initialize.params, protocolVersion: '2099-01-01' } });
  assert.equal(future.result.protocolVersion, protocol_version);
});
test('read-only tools share the same authoritative verification plan', () => {
  const session = ready();
  const definitions = session(rpc(2, 'tools/list')).result.tools;
  assert.ok(definitions.every((tool) => tool.annotations.readOnlyHint && !tool.annotations.openWorldHint));
  const result = session(rpc(3, 'tools/call', { name: 'aag_verification_plan', arguments: { scope: 'dsh' } })).result;
  assert.deepEqual(JSON.parse(result.content[0].text), verification_plan('dsh'));
  assert.equal(result.isError, false);
});
test('rejects shell, file paths, approval forgery, invalid enums, and unknown methods without echoing input', () => {
  const session = ready();
  const secret = 'synthetic_sensitive_marker';
  const bad = [
    rpc(2, 'tools/call', { name: 'execute_shell', arguments: { command: secret } }),
    rpc(3, 'tools/call', { name: 'aag_capabilities', arguments: { path: secret } }),
    rpc(4, 'tools/call', { name: 'aag_verification_plan', arguments: { scope: 'mcp', approved: true } }),
    rpc(5, 'tools/call', { name: 'aag_verification_plan', arguments: { scope: secret } }),
    rpc(6, 'tools/call', { name: 'aag_workflow', arguments: { topic: '__proto__' } }),
    rpc(7, 'resources/read', { uri: 'file:///private/' + secret }),
    rpc(8, 'tools/call', { name: 'aag_workflow', arguments: [] }),
  ];
  for (const request of bad) { const result = session(request); assert.ok(result.error); assert.equal(JSON.stringify(result).includes(secret), false); }
  assert.ok(session(null).error); assert.ok(session([initialize]).error);
  assert.equal(session({ jsonrpc: '2.0', method: 'tools/call', params: { name: 'execute_shell' } }), null);
});
test('real stdio handles initialization, malformed JSON, oversize recovery and EOF without non-protocol output', async () => {
  const child = spawn(process.execPath, [fileURLToPath(new URL('./aag_mcp.mjs', import.meta.url))], { stdio: ['pipe', 'pipe', 'pipe'], shell: false, windowsHide: true });
  let output = ''; let errors = '';
  child.stdout.on('data', (chunk) => { output += chunk; }); child.stderr.on('data', (chunk) => { errors += chunk; });
  const completion = new Promise((resolve, reject) => { child.once('error', reject); child.once('exit', resolve); });
  const timer = setTimeout(() => child.kill(), 5000);
  try {
    child.stdin.end(JSON.stringify(initialize) + '\n' + JSON.stringify(initialized) + '\n' + JSON.stringify(rpc(2, 'tools/list')) + '\n{broken\n' + 'x'.repeat(100000) + '\n' + JSON.stringify(rpc(3, 'ping')) + '\n' + '{partial');
    assert.equal(await completion, 0);
    const messages = output.trim().split('\n').map((line) => JSON.parse(line));
    assert.equal(messages.length, 6);
    assert.equal(messages[0].result.protocolVersion, protocol_version);
    assert.equal(messages[1].result.tools.length, 3);
    assert.equal(messages[2].error.code, -32700);
    assert.equal(messages[3].error.code, -32600);
    assert.deepEqual(messages[4].result, {});
    assert.equal(messages[5].error.code, -32700);
    assert.equal(errors, '');
  } finally { clearTimeout(timer); if (child.exitCode === null) child.kill(); }
});
