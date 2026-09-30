import test from 'node:test';
import { request as http_request } from 'node:http';
import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { spawn } from 'node:child_process';
import net from 'node:net';
import { randomBytes } from 'node:crypto';
import { pathToFileURL } from 'node:url';
import { once } from 'node:events';

async function free_port() {
  const listener = net.createServer();
  listener.listen(0, '127.0.0.1');
  await once(listener, 'listening');
  const port = listener.address().port;
  await new Promise((resolve_close) => listener.close(resolve_close));
  return port;
}
async function upgrade_status(port, headers, request_line = 'GET /socket HTTP/1.1') {
  return new Promise((resolve_result, reject) => {
    const socket = net.connect(port, '127.0.0.1');
    socket.setTimeout(3000, () => socket.destroy(new Error('upgrade_timeout')));
    socket.on('error', reject);
    socket.on('connect', () => socket.write(request_line + '\r\nHost: 127.0.0.1:' + port + '\r\n' + (request_line.startsWith('GET ') ? 'Connection: Upgrade\r\nUpgrade: websocket\r\n' : '') + headers + '\r\n'));
    socket.once('data', (chunk) => { resolve_result(chunk.toString().split('\r\n')[0]); socket.destroy(); });
  });
}
test('DSH transport authenticates actual HTTP and websocket traffic', { timeout: 20000 }, async () => {
  const directory = await mkdtemp(join(tmpdir(), 'aag_dsh_transport_'));
  const entry = join(directory, 'server.mjs');
  const port = await free_port();
  const token = randomBytes(32).toString('hex');
  const runtime_id = randomBytes(16).toString('hex');
  await writeFile(entry, "import http from 'node:http'; const server=http.createServer((req,res)=>{res.setHeader('content-type','text/html');res.end('<html>managed_fixture</html>')}); server.on('upgrade',(req,socket)=>socket.end('HTTP/1.1 101 Switching Protocols\\r\\nConnection: Upgrade\\r\\nUpgrade: websocket\\r\\n\\r\\n')); server.on('checkContinue',(req,res)=>res.end('fixture_continue')); server.on('checkExpectation',(req,res)=>res.end('fixture_expectation')); server.on('connect',(req,socket)=>socket.end('HTTP/1.1 200 Connection Established\\r\\n\\r\\n')); server.listen(" + port + ",'127.0.0.1',()=>process.stdout.write(JSON.stringify({ready:true,control_env:process.env.AAG_DSH_CONTROL_TOKEN??null})+'\\n'));");
  const child = spawn(process.execPath, ['--import', pathToFileURL(resolve('src-tauri/resources/dsh_preload.mjs')).href, entry], {
    windowsHide: true,
    env: { ...process.env, AAG_DSH_PORT: String(port), AAG_DSH_CONTROL_TOKEN: token, AAG_DSH_RUNTIME_ID: runtime_id },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  const exited = once(child, 'exit');
  try {
    const output = await Promise.race([
      once(child.stdout, 'data').then(([buffer]) => buffer.toString()),
      once(child, 'error').then(([error]) => { throw error; }),
      exited.then(() => { throw new Error('fixture_exited_before_ready'); }),
      new Promise((_, reject) => { const deadline=setTimeout(()=>reject(new Error('fixture_start_timeout')),5000); deadline.unref(); }),
    ]);
    assert.deepEqual(JSON.parse(output), { ready: true, control_env: null });
    assert.equal(output.includes(token), false);
    const origin = 'http://127.0.0.1:' + port;
    assert.equal((await fetch(origin + '/')).status, 403);
    assert.equal((await fetch(origin + '/__aag_health')).status, 403);
    assert.equal((await fetch(origin + '/__aag_health', { headers: { Authorization: 'Bearer wrong' } })).status, 403);
    const headers = { Authorization: 'Bearer ' + token };
    assert.deepEqual(await (await fetch(origin + '/__aag_health', { headers })).json(), { runtime_id, ready: true });
    assert.match(await (await fetch(origin + '/', { headers })).text(), /managed_fixture/);
    assert.equal((await fetch(origin + '/', { headers: { ...headers, Origin: 'https://evil.invalid' } })).status, 403);
    assert.equal(await new Promise((resolve_status, reject) => {
      const request = http_request(origin + '/', { headers: { ...headers, Host: 'evil.invalid' } }, (response) => {
        response.resume(); resolve_status(response.statusCode);
      });
      request.on('error', reject); request.end();
    }), 403);
    assert.equal((await fetch(origin + '/__aag_bootstrap?token=wrong', { redirect: 'manual' })).status, 403);
    const bootstrap = await fetch(origin + '/__aag_bootstrap?token=' + token, { redirect: 'manual' });
    assert.equal(bootstrap.status, 303);
    assert.equal(bootstrap.headers.get('location'), '/');
    assert.match(bootstrap.headers.get('set-cookie'), /HttpOnly; SameSite=Strict/);
    const cookie = bootstrap.headers.get('set-cookie').split(';')[0];
    assert.equal((await fetch(origin + '/', { headers: { Cookie: cookie } })).status, 200);
    assert.equal(await upgrade_status(port, ''), 'HTTP/1.1 403 Forbidden');
    assert.equal(await upgrade_status(port, 'Cookie: ' + cookie + '\r\nOrigin: https://evil.invalid\r\n'), 'HTTP/1.1 403 Forbidden');
    assert.equal(await upgrade_status(port, 'Cookie: ' + cookie + '\r\nOrigin: ' + origin + '\r\n'), 'HTTP/1.1 101 Switching Protocols');
    assert.equal(await upgrade_status(port, 'Expect: 100-continue\r\nContent-Length: 0\r\n', 'POST /api HTTP/1.1'), 'HTTP/1.1 403 Forbidden');
    assert.equal(await upgrade_status(port, 'Expect: fixture\r\nContent-Length: 0\r\n', 'POST /api HTTP/1.1'), 'HTTP/1.1 403 Forbidden');
    assert.equal(await upgrade_status(port, 'Authorization: Bearer ' + token + '\r\nExpect: 100-continue\r\nContent-Length: 0\r\n', 'POST /api HTTP/1.1'), 'HTTP/1.1 200 OK');
    assert.equal(await upgrade_status(port, 'Authorization: Bearer ' + token + '\r\n', 'CONNECT fixture.invalid:443 HTTP/1.1'), 'HTTP/1.1 403 Forbidden');
    assert.equal(await upgrade_status(port, 'Cookie: ' + cookie + '\r\n', 'GET http://evil.invalid/socket HTTP/1.1'), 'HTTP/1.1 403 Forbidden');
  } finally {
    child.kill();
    await exited;
    await rm(directory, { recursive: true, force: true });
  }
});
test('native commands preserve manager authority and exact market read-only scope', async () => {
  const source = await readFile('src-tauri/src/lib.rs', 'utf8');
  const registered = source.match(/generate_handler!\[([\s\S]*?)\]/)[1].split(',').map((part)=>part.trim().split('::').at(-1)).filter(Boolean);
  const build = await readFile('src-tauri/build.rs', 'utf8');
  const capability = JSON.parse(await readFile('src-tauri/capabilities/default.json', 'utf8'));
  assert.deepEqual(capability.windows, ['main']);
  assert.equal(capability.remote, undefined);
  for (const command of registered) {
    assert.ok(build.includes('"' + command + '"'), 'command not in app manifest: ' + command);
    assert.ok(capability.permissions.includes('allow-' + command.replaceAll('_', '-')), 'command not allowed for manager: ' + command);
  }
  assert.match(source, /dsh_market_window::allowed_command\(\s*invoke\.message\.webview_ref\(\)\.label\(\),\s*invoke\.message\.command\(\),?\s*\)/);
  const market = JSON.parse(await readFile('src-tauri/capabilities/dsh_market.json', 'utf8'));
  assert.deepEqual(market.windows, ['dsh_market']);
  assert.equal(market.remote, undefined);
  assert.deepEqual([...market.permissions].sort(), [
    'allow-dsh-market-get-catalog', 'allow-dsh-market-open-source', 'allow-dsh-market-refresh',
  ]);
  assert.equal(market.permissions.some((permission) => permission.startsWith('core:')), false);
});
