import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { chmodSync, existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:http';
import net from 'node:net';
import { tmpdir } from 'node:os';
import { basename, join } from 'node:path';
import { read_cdp_endpoint } from './cdp_endpoint.mjs';
import {
  find_browser,
  format_error,
  read_cdp_origin,
  stop_owned_browser,
  terminate_windows_profile_processes,
  validate_profile_directory,
  wait_for_cdp_origin,
  windows_powershell_path,
  windows_taskkill_path,
} from './smoke_runtime.mjs';

const io_error = (code) => Object.assign(new Error('fixture_' + code), { code });

test('browser discovery uses only candidates for the requested operating system', () => {
  for (const [platform, expected] of [
    ['win32', 'C:/Program Files/Google/Chrome/Application/chrome.exe'],
    ['darwin', '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome'],
    ['linux', '/usr/bin/chromium'],
  ]) {
    assert.equal(find_browser(platform, (candidate) => candidate === expected), expected);
    assert.equal(find_browser(platform, () => false), null);
  }
  assert.equal(find_browser('unsupported', () => true), null);
});

test('mac browser discovery includes user Applications and absolute PATH entries', () => {
  const home = '/fixture/home';
  const expected = home + '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
  assert.equal(find_browser('darwin', (candidate) => candidate === expected, { home, path: '', browser_path: '' }), expected);
  assert.equal(find_browser('darwin', (candidate) => candidate === '/fixture/bin/chromium', {
    home, path: '.:relative:/fixture/bin', browser_path: '',
  }), '/fixture/bin/chromium');
  assert.equal(find_browser('darwin', (candidate) => candidate.startsWith('relative'), {
    home, path: '.:relative', browser_path: '',
  }), null);
});

test('explicit browser requires an executable file and never falls back silently', { skip: process.platform === 'win32' }, () => {
  const root = mkdtempSync(join(tmpdir(), 'aag_browser_discovery_'));
  const executable = join(root, 'browser');
  try {
    mkdirSync(join(root, 'directory'));
    writeFileSync(executable, '#!/bin/sh\nexit 0\n', { mode: 0o600 });
    const find = (browser_path) => find_browser('darwin', undefined, { home: root, path: '', browser_path });
    assert.equal(find(executable), null);
    assert.equal(find(join(root, 'directory')), null);
    assert.equal(find('relative/browser'), null);
    chmodSync(executable, 0o700);
    assert.equal(find(executable), executable);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test('Windows taskkill resolution is independent of PATH', () => {
  assert.equal(
    windows_taskkill_path('system_root_fixture', () => true),
    join('system_root_fixture', 'System32', 'taskkill.exe'),
  );
});

test('Windows PowerShell resolution is independent of PATH', () => {
  assert.equal(
    windows_powershell_path('system_root_fixture', () => true),
    join('system_root_fixture', 'System32', 'WindowsPowerShell', 'v1.0', 'powershell.exe'),
  );
});

test('nested smoke errors retain startup and cleanup causes', () => {
  const error = new AggregateError([
    new Error('devtools endpoint timeout'),
    new Error('profile lockfile is busy'),
  ], 'browser startup and cleanup failed');
  assert.equal(
    format_error(error),
    'browser startup and cleanup failed: devtools endpoint timeout; profile lockfile is busy',
  );
});

test('DevToolsActivePort EBUSY retries through the actual bounded startup loop', async () => {
  let clock = 0; let reads = 0; let pauses = 0;
  const origin = await wait_for_cdp_origin('owned_profile_fixture', { exitCode: null }, {
    read_file: (path, encoding) => {
      assert.equal(basename(path), 'DevToolsActivePort');
      assert.equal(encoding, 'utf8');
      reads += 1;
      if (reads === 1) throw io_error('EBUSY');
      return '9222\r\n/devtools/browser/fixture\r\n';
    },
    now: () => clock,
    pause: async (duration) => { assert.equal(duration, 100); clock += duration; pauses += 1; },
  });
  assert.equal(origin, 'http://127.0.0.1:9222');
  assert.equal(reads, 2);
  assert.equal(pauses, 1);
});

test('missing or temporarily busy port files remain not ready', () => {
  for (const code of ['ENOENT', 'EBUSY']) {
    assert.equal(read_cdp_origin('owned_profile_fixture', () => { throw io_error(code); }), null);
  }
});

test('persistent EBUSY cannot extend the existing twenty-second deadline', async () => {
  let clock = 0; let reads = 0;
  await assert.rejects(wait_for_cdp_origin('owned_profile_fixture', { exitCode: null }, {
    read_file: () => { reads += 1; throw io_error('EBUSY'); },
    now: () => clock,
    pause: async (duration) => { clock += duration; },
  }), /within 20s/);
  assert.equal(clock, 20_000);
  assert.equal(reads, 200);
});

test('permanent read errors retain their identity and do not retry', async () => {
  const denied = io_error('EACCES'); let pauses = 0;
  await assert.rejects(wait_for_cdp_origin('owned_profile_fixture', { exitCode: null }, {
    read_file: () => { throw denied; },
    pause: async () => { pauses += 1; },
  }), (error) => error === denied);
  assert.equal(pauses, 0);
});

test('invalid or out-of-range port contents never produce a CDP origin', () => {
  for (const contents of ['', 'not_ready', '0', '65536', '-1', ' 9222', '9222x', '9'.repeat(400)]) {
    assert.equal(read_cdp_origin('owned_profile_fixture', () => contents), null, contents);
  }
  assert.equal(read_cdp_origin('owned_profile_fixture', () => '65535\n'), 'http://127.0.0.1:65535');
});

test('spawn failures still escape immediately for the existing cleanup owner', async () => {
  const failed = io_error('ENOEXEC'); let pauses = 0;
  await assert.rejects(wait_for_cdp_origin('owned_profile_fixture', { exitCode: null }, {
    read_file: () => { throw io_error('ENOENT'); },
    read_spawn_error: () => failed,
    pause: async () => { pauses += 1; },
  }), (error) => error === failed);
  assert.equal(pauses, 0);
});

function listen(server) {
  return new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
}

function close_server(server, sockets) {
  for (const socket of sockets) socket.destroy();
  return new Promise((resolve, reject) => server.close((error) => error ? reject(error) : resolve()));
}

test('CDP endpoint reader aborts a stalled response body within its deadline', async () => {
  const sockets = new Set();
  const server = createServer((request, response) => {
    response.writeHead(200, { 'content-type': 'application/json' });
    response.write('[');
  });
  server.on('connection', (socket) => sockets.add(socket));
  await listen(server);
  const address = server.address();
  const started_at = Date.now();
  try {
    await assert.rejects(
      read_cdp_endpoint('http://127.0.0.1:' + address.port, '/json/list', { timeout_ms: 100 }),
      /aborted|timeout/i,
    );
    assert.ok(Date.now() - started_at < 1_000);
  } finally {
    await close_server(server, sockets);
  }
});

test('CDP endpoint reader aborts before stalled response headers arrive', async () => {
  const sockets = new Set();
  const server = createServer(() => {});
  server.on('connection', (socket) => sockets.add(socket));
  await listen(server);
  const address = server.address();
  const started_at = Date.now();
  try {
    await assert.rejects(
      read_cdp_endpoint('http://127.0.0.1:' + address.port, '/json/list', { timeout_ms: 100 }),
      /aborted|timeout/i,
    );
    assert.ok(Date.now() - started_at < 1_000);
  } finally {
    await close_server(server, sockets);
  }
});

test('CDP endpoint reader returns JSON from a bounded local endpoint', async () => {
  const server = createServer((request, response) => {
    response.writeHead(200, { 'content-type': 'application/json' });
    response.end('[{"type":"page"}]');
  });
  await listen(server);
  const address = server.address();
  try {
    assert.deepEqual(
      await read_cdp_endpoint('http://127.0.0.1:' + address.port, '/json/list', { timeout_ms: 1_000 }),
      [{ type: 'page' }],
    );
  } finally {
    await new Promise((resolve, reject) => server.close((error) => error ? reject(error) : resolve()));
  }
});

test('profile cleanup refuses an unexpected directory without deleting it', async () => {
  const profile_directory = mkdtempSync(join(tmpdir(), 'unexpected_smoke_profile_'));
  writeFileSync(join(profile_directory, 'sentinel.txt'), 'retain');
  try {
    assert.equal(validate_profile_directory(profile_directory), false);
    await assert.rejects(
      stop_owned_browser({ pid: undefined, exitCode: 0, signalCode: null }, profile_directory),
      /refusing to stop browser for unexpected profile path/,
    );
    assert.equal(existsSync(profile_directory), true);
  } finally {
    const { rmSync } = await import('node:fs');
    rmSync(profile_directory, { recursive: true, force: true });
  }
});

test('profile cleanup removes an already-exited owned browser profile', async () => {
  const profile_directory = mkdtempSync(join(tmpdir(), 'aag_ui_smoke_'));
  writeFileSync(join(profile_directory, 'sentinel.txt'), 'owned');
  await stop_owned_browser({ pid: undefined, exitCode: 0, signalCode: null }, profile_directory);
  assert.equal(existsSync(profile_directory), false);
});

test('profile cleanup terminates only its spawned child and removes its profile', async () => {
  const profile_directory = mkdtempSync(join(tmpdir(), 'aag_ui_smoke_'));
  const child = spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000)'], { windowsHide: true });
  await new Promise((resolve, reject) => {
    child.once('spawn', resolve);
    child.once('error', reject);
  });
  try {
    await stop_owned_browser(child, profile_directory);
    assert.ok(child.exitCode !== null || child.signalCode !== null);
    assert.equal(existsSync(profile_directory), false);
  } finally {
    if (child.exitCode === null && child.signalCode === null) child.kill();
  }
});

function wait_for_line(stream) {
  const deadline_ms = process.platform === 'win32' ? 6_000 : 3_000;
  return new Promise((resolve, reject) => {
    let pending = '';
    const timer = setTimeout(() => reject(new Error('owned descendant did not report readiness')), deadline_ms);
    stream.on('data', (chunk) => {
      pending += chunk;
      const newline = pending.indexOf('\n');
      if (newline < 0) return;
      clearTimeout(timer);
      resolve(pending.slice(0, newline).trim());
    });
    stream.once('error', reject);
  });
}

function wait_for_exit(child, timeout_ms = 3_000) {
  if (child.exitCode !== null || child.signalCode !== null) return Promise.resolve();
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      child.removeListener('exit', on_exit);
      reject(new Error('child did not exit within test timeout'));
    }, timeout_ms);
    const on_exit = () => { clearTimeout(timer); resolve(); };
    child.once('exit', on_exit);
    child.once('error', (error) => { clearTimeout(timer); reject(error); });
  });
}

function is_loopback_port_open(port) {
  return new Promise((resolve) => {
    const socket = net.createConnection({ host: '127.0.0.1', port });
    const timer = setTimeout(() => { socket.destroy(); resolve(false); }, 500);
    socket.once('connect', () => { clearTimeout(timer); socket.destroy(); resolve(true); });
    socket.once('error', () => { clearTimeout(timer); resolve(false); });
  });
}

async function wait_for_loopback_port_closed(port) {
  const deadline = Date.now() + 3_000;
  while (Date.now() < deadline) {
    if (!await is_loopback_port_open(port)) return;
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error('owned descendant port remained open after cleanup');
}

test('profile cleanup terminates an owned Windows child process tree', { skip: process.platform !== 'win32' }, async () => {
  const profile_directory = mkdtempSync(join(tmpdir(), 'aag_ui_smoke_'));
  const descendant_source = [
    "const net = require('node:net');",
    "const server = net.createServer();",
    "server.listen(0, '127.0.0.1', () => console.log(server.address().port));",
    'setTimeout(() => process.exit(0), 6_000);',
  ].join('');
  const parent_source = [
    "const { spawn } = require('node:child_process');",
    'const child = spawn(process.execPath, [\'-e\', process.argv[1]], { stdio: [\'ignore\', \'pipe\', \'ignore\'], windowsHide: true });',
    'child.stdout.pipe(process.stdout);',
    'setTimeout(() => process.exit(0), 8_000);',
  ].join('');
  let parent;
  let cleanup_completed = false;
  let test_error;
  try {
    parent = spawn(process.execPath, ['-e', parent_source, descendant_source], { stdio: ['ignore', 'pipe', 'ignore'], windowsHide: true });
    const port = Number(await wait_for_line(parent.stdout));
    assert.ok(Number.isInteger(port) && port > 0);
    assert.equal(await is_loopback_port_open(port), true);
    await stop_owned_browser(parent, profile_directory);
    await wait_for_loopback_port_closed(port);
    assert.equal(existsSync(profile_directory), false);
    cleanup_completed = true;
  } catch (error) {
    test_error = error;
  }
  let cleanup_error;
  if (!cleanup_completed) {
    try {
      if (parent) await stop_owned_browser(parent, profile_directory);
      else rmSync(profile_directory, { recursive: true, force: true });
    } catch (error) {
      cleanup_error = error;
    }
  }
  if (test_error && cleanup_error) {
    throw new AggregateError([test_error, cleanup_error], 'smoke child test and cleanup failed');
  }
  if (test_error) throw test_error;
  if (cleanup_error) throw cleanup_error;
});

test('Windows profile cleanup terminates matching processes without touching others', { skip: process.platform !== 'win32' }, async () => {
  const profile_directory = mkdtempSync(join(tmpdir(), 'aag_ui_smoke_'));
  const matching = spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000)', profile_directory], { windowsHide: true });
  const unrelated = spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000)'], { windowsHide: true });
  try {
    await Promise.all([
      new Promise((resolve, reject) => { matching.once('spawn', resolve); matching.once('error', reject); }),
      new Promise((resolve, reject) => { unrelated.once('spawn', resolve); unrelated.once('error', reject); }),
    ]);
    await terminate_windows_profile_processes(profile_directory);
    await wait_for_exit(matching);
    assert.equal(unrelated.exitCode, null);
    assert.equal(unrelated.signalCode, null);
  } finally {
    if (matching.exitCode === null && matching.signalCode === null) {
      matching.kill();
      await wait_for_exit(matching).catch(() => {});
    }
    if (unrelated.exitCode === null && unrelated.signalCode === null) {
      unrelated.kill();
      await wait_for_exit(unrelated).catch(() => {});
    }
    rmSync(profile_directory, { recursive: true, force: true });
  }
});
