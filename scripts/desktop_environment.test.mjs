import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { test } from 'node:test';
import { join } from 'node:path';
import { promisify } from 'node:util';
import { inspect_desktop_environment, probe_command } from './desktop_environment.mjs';

const exec_file = promisify(execFile);

function fixture_options(overrides = {}) {
  return {
    root: process.cwd(), platform: 'darwin', arch: 'arm64', node_version: '22.0.0', env: {},
    path_exists: () => true,
    temporary_directory: async () => '/fixture/temp',
    browser: () => '/Applications/Fixture Browser.app/Contents/MacOS/Fixture Browser',
    probe: async (executable, args) => ({ status: 'passed', stdout: executable.endsWith('xcode-select') ? '/fixture/developer' : executable.endsWith('xcrun') ? '27.0' : args.some((arg) => arg.includes('tauri.js')) ? 'tauri-cli 2.11.4' : '22.0.0' }),
    ...overrides,
  };
}

test('macOS doctor inspects the complete native toolchain without returning local paths', async () => {
  const calls = [];
  const options = fixture_options();
  const receipt = await inspect_desktop_environment({ ...options, probe: async (executable, args, context) => {
    calls.push([executable, args]);
    return options.probe(executable, args, context);
  } });
  assert.equal(receipt.status, 'passed');
  for (const id of ['node', 'npm', 'temporary_directory', 'git', 'vite', 'browser', 'tauri', 'rustc', 'cargo', 'xcode_tools', 'macos_clang', 'macos_sdk']) {
    assert.ok(receipt.checks.some((check) => check.id === id && check.status === 'passed'), id);
  }
  assert.ok(calls.some(([executable, args]) => executable === '/usr/bin/xcrun' && args.includes('macosx')));
  assert.equal(JSON.stringify(receipt).includes('/fixture/'), false);
  assert.equal(JSON.stringify(receipt).includes('/Applications/'), false);
});

test('Node and UI scopes do not require unrelated native tools', async () => {
  for (const scope of ['node', 'ui']) {
    const receipt = await inspect_desktop_environment(fixture_options({ scope }));
    assert.equal(receipt.status, 'passed');
    assert.equal(receipt.checks.some((check) => ['rustc', 'cargo', 'tauri', 'macos_sdk'].includes(check.id)), false);
    assert.equal(receipt.checks.some((check) => check.id === 'browser'), scope === 'ui');
    assert.equal(receipt.checks.some((check) => check.id === 'git'), scope === 'ui');
    assert.equal(receipt.checks.some((check) => check.id === 'node_typescript' || check.id === 'node_vitest'), scope === 'node');
  }
});

test('UI scope succeeds without unrelated root TypeScript and Vitest dependencies', async () => {
  const missing = ['node_modules/typescript/bin/tsc', 'node_modules/vitest/vitest.mjs'].map((path) => join(process.cwd(), path));
  const options = fixture_options({ path_exists: (path) => !missing.includes(path) });
  assert.equal((await inspect_desktop_environment({ ...options, scope: 'ui' })).status, 'passed');
  assert.equal((await inspect_desktop_environment({ ...options, scope: 'node' })).status, 'failed');
});

test('locked native dependencies require Rust and Cargo 1.88 or newer', async () => {
  const options = fixture_options();
  for (const [version, expected] of [['1.87.0', 'failed'], ['1.88.0', 'passed']]) {
    const receipt = await inspect_desktop_environment({ ...options,
      probe: async (executable, args, context) => ['rustc', 'cargo'].includes(executable)
        ? { status: 'passed', stdout: `${executable} ${version}` } : options.probe(executable, args, context),
    });
    assert.equal(receipt.status, expected);
    for (const id of ['rustc', 'cargo']) assert.equal(receipt.checks.find((check) => check.id === id).status, expected);
  }
});

test('every privacy-audit scope requires Git while the Node scope does not', async () => {
  const options = fixture_options();
  for (const scope of ['ui', 'mcp', 'dsh', 'all', 'node']) {
    const receipt = await inspect_desktop_environment({ ...options, scope,
      probe: async (executable, args, context) => executable === 'git'
        ? { status: 'unavailable', stdout: '' } : options.probe(executable, args, context),
    });
    assert.equal(receipt.status, scope === 'node' ? 'passed' : 'failed');
    if (scope !== 'node') assert.equal(receipt.checks.find((check) => check.id === 'git').status, 'failed');
  }
});

test('missing dependencies, old versions and failed SDK probes remain failures', async () => {
  const options = fixture_options();
  const receipt = await inspect_desktop_environment({ ...options, node_version: '18.20.0',
    path_exists: () => false, browser: () => null,
    probe: async (executable, args, context) => executable.endsWith('xcrun') || executable === 'cargo'
      ? { status: 'unavailable', stdout: '' } : executable === 'rustc' ? { status: 'passed', stdout: 'rustc 1.77.1' } : options.probe(executable, args, context),
  });
  assert.equal(receipt.status, 'failed');
  for (const id of ['node', 'node_typescript', 'node_vitest', 'browser', 'rustc', 'cargo', 'macos_sdk']) {
    const check = receipt.checks.find((item) => item.id === id);
    assert.equal(check.status, 'failed', id);
    assert.equal(typeof check.remediation, 'string');
  }
});

test('other platforms never probe macOS tools and invalid scopes are rejected', async () => {
  for (const platform of ['win32', 'linux']) {
    const receipt = await inspect_desktop_environment(fixture_options({ platform }));
    assert.equal(receipt.status, 'passed');
    assert.equal(receipt.checks.some((check) => check.id.startsWith('macos_') || check.id === 'xcode_tools'), false);
  }
  await assert.rejects(inspect_desktop_environment(fixture_options({ scope: 'all; exit' })), /AAG_INVALID_SCOPE/);
});

test('bounded probes handle missing programs, timeout and excessive output without exposing stderr', async () => {
  assert.equal((await probe_command('__aag_missing_probe__', [])).status, 'unavailable');
  assert.equal((await probe_command(process.execPath, ['-e', 'setInterval(() => {}, 1000)'], { timeout_ms: 100 })).status, 'timed_out');
  assert.equal((await probe_command(process.execPath, ['-e', "process.stdout.write('x'.repeat(100000))"])).status, 'output_limit');
  const failure = await probe_command(process.execPath, ['-e', "process.stderr.write('private fixture message');process.exit(2)"]);
  assert.equal(failure.status, 'failed');
  assert.equal(JSON.stringify(failure).includes('private fixture message'), false);
});

test('plan-only CLI works without toolchain executables and doctor JSON reports missing tools', async () => {
  const env = { ...process.env, PATH: '' };
  const plan = JSON.parse((await exec_file(process.execPath, ['scripts/verify_features.mjs', '--scope', 'all'], { cwd: process.cwd(), env })).stdout);
  assert.equal(plan.mode, 'plan_only');
  assert.equal(plan.preflight.mode, 'on_execution_only');
  let result;
  try { await exec_file(process.execPath, ['scripts/desktop_doctor.mjs', '--json'], { cwd: process.cwd(), env }); }
  catch (error) { result = error; }
  assert.equal(result?.code, 1);
  const receipt = JSON.parse(result.stdout);
  assert.equal(receipt.status, 'failed');
  assert.equal(receipt.checks.find((check) => check.id === 'cargo').status, 'failed');
});
