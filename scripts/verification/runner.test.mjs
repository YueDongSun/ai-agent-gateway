import { test } from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { mkdtemp, mkdir, realpath, rm, symlink } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { basename, dirname, join, resolve } from 'node:path';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { execute_plan, macos_group_is_inactive, run_check, stop_owned_child } from './runner.mjs';
import { verification_plan } from './feature_catalog.mjs';

const fixture_preflight = async () => ({ status: 'passed', checks: [] });
const execute_fixture_plan = (plan, options) => execute_plan(plan, { preflight: fixture_preflight, ...options });

test('Darwin cleanup accepts EPERM only after exited-leader and no-live-member evidence', async () => {
  const exited = { pid: 321, exitCode: 0, signalCode: null };
  const denied = () => { throw Object.assign(new Error('permission denied'), { code: 'EPERM' }); };
  for (const table of ['100 S\n321 Z\n321 Z+', '100 S']) {
    await stop_owned_child(exited, { platform: 'darwin', kill: denied,
      inactive_group: (group) => macos_group_is_inactive(group, async () => table) });
  }
  for (const table of ['321 S\n321 Z', '321 T', '', 'unrecognized output']) {
    await assert.rejects(stop_owned_child(exited, { platform: 'darwin', kill: denied,
      inactive_group: (group) => macos_group_is_inactive(group, async () => table) }), { code: 'EPERM' });
  }
  await assert.rejects(stop_owned_child(exited, { platform: 'darwin', kill: denied,
    inactive_group: async () => { throw new Error('ps unavailable'); } }), { code: 'EPERM' });
  for (const [platform, child] of [['linux', exited], ['darwin', { ...exited, exitCode: null }]]) {
    let inspected = false;
    await assert.rejects(stop_owned_child(child, { platform, kill: denied,
      inactive_group: async () => { inspected = true; return true; } }), { code: 'EPERM' });
    assert.equal(inspected, false);
  }
});

test('orchestrator isolates config, strips live DB opt-in, fails fast, and removes owned state', async () => {
  let temporary_home;
  const seen = [];
  const report = await execute_fixture_plan(verification_plan('node'), { root: process.cwd(), run: async (check, context) => {
    temporary_home = context.env.AAG_HOME_DIR;
    assert.ok(existsSync(temporary_home));
    assert.ok(existsSync(context.env.AAG_CONFIG_DIR));
    assert.equal(context.env.AAG_TEST_CCSWITCH_DB, undefined);
    assert.notEqual(context.env.AAG_HOME_DIR, process.env.AAG_HOME_DIR);
    seen.push(check.id);
    return { status: seen.length === 1 ? 'passed' : 'failed', exit_code: seen.length === 1 ? 0 : 1, duration_ms: 1, foreground_exit: 'confirmed' };
  } });
  assert.equal(report.status, 'failed'); assert.equal(seen.length, 2);
  assert.equal(report.results.length, 2); assert.ok(report.unrun_checks.length > 0);
  assert.equal(existsSync(temporary_home), false);
  assert.equal(JSON.stringify(report).includes(temporary_home), false);
  assert.equal(report.runtime_state, 'removed');
  assert.equal(report.command_evidence, 'foreground_exit_only');
  assert.equal(report.descendant_cleanup, 'not_verified');
});
test('pre-cancelled plan never executes a check', async () => {
  const controller = new AbortController(); controller.abort();
  const report = await execute_fixture_plan(verification_plan('all'), { root: process.cwd(), signal: controller.signal, run: () => { throw new Error('must not run'); } });
  assert.equal(report.status, 'cancelled'); assert.equal(report.results.length, 0);
});
test('real owned subprocess passes, reports failure, and cleans up on timeout', async () => {
  const context = { root: process.cwd(), env: { ...process.env } };
  const child = (source) => ({ runtime: 'node', cwd: '.', args: ['-e', source] });
  const passed = await run_check(child('process.exit(0)'), context);
  assert.equal(passed.status, 'passed');
  assert.equal(passed.foreground_exit, 'confirmed');
  assert.equal(passed.descendant_cleanup, 'not_verified');
  assert.equal((await run_check(child('process.exit(7)'), context)).exit_code, 7);
  const timed_out = await run_check(child('setInterval(() => {}, 1000)'), { ...context, timeout_ms: 200 });
  assert.equal(timed_out.status, 'timed_out');
  assert.equal(timed_out.foreground_exit, 'confirmed');
});
test('running subprocess cancellation settles instead of reporting success', async () => {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 200);
  try {
    const result = await run_check({ runtime: 'node', cwd: '.', args: ['-e', 'setInterval(() => {}, 1000)'] }, { root: process.cwd(), env: { ...process.env }, signal: controller.signal });
    assert.equal(result.status, 'cancelled');
    assert.equal(result.foreground_exit, 'confirmed');
  } finally { clearTimeout(timer); }
});

async function remove_owned_fixture(directory) {
  assert.equal(await realpath(dirname(resolve(directory))), await realpath(tmpdir()));
  assert.match(basename(directory), /^aag_(runner_fixture|feature_verify)_/);
  await rm(directory, { recursive: true, force: true, maxRetries: 3 });
}

function process_running(pid) {
  try { process.kill(pid, 0); }
  catch (error) { if (error.code === 'ESRCH') return false; throw error; }
  // A Unix orphan can briefly remain as a zombie until init reaps it. It can
  // no longer execute or hold a listener; do not confuse that with a live helper.
  if (process.platform === 'linux') {
    try {
      const stat = readFileSync(`/proc/${pid}/stat`, 'utf8');
      if (stat.slice(stat.lastIndexOf(')') + 2, stat.lastIndexOf(')') + 3) === 'Z') return false;
    } catch (error) { if (error.code === 'ENOENT') return false; throw error; }
  }
  return true;
}

async function wait_stopped(pid) {
  const deadline = Date.now() + 3000;
  while (process_running(pid) && Date.now() < deadline) await new Promise((resolve_wait) => setTimeout(resolve_wait, 20));
  assert.equal(process_running(pid), false, 'owned foreground/helper process remained running');
}

async function descendant_fixture(detached) {
  const directory = await mkdtemp(join(tmpdir(), 'aag_runner_fixture_'));
  const pid_path = join(directory, 'helper_pid');
  const helper = `require('node:fs').writeFileSync(${JSON.stringify(pid_path)},String(process.pid));setInterval(()=>{},1000);`;
  const parent = `const fs=require('node:fs');const child=require('node:child_process').spawn(process.execPath,['-e',${JSON.stringify(helper)}],{stdio:'ignore',windowsHide:true,detached:${detached}});child.unref();setInterval(()=>{if(fs.existsSync(${JSON.stringify(pid_path)}))process.exit(0)},10);setTimeout(()=>process.exit(8),2000);`;
  let helper_pid;
  return {
    check: { runtime: 'node', cwd: '.', args: ['-e', parent] },
    read_pid() { helper_pid = Number(readFileSync(pid_path, 'utf8')); assert.ok(Number.isSafeInteger(helper_pid) && helper_pid > 0); return helper_pid; },
    async close() {
      if (!helper_pid && existsSync(pid_path)) helper_pid = Number(readFileSync(pid_path, 'utf8'));
      if (helper_pid && process_running(helper_pid)) { process.kill(helper_pid, 'SIGKILL'); await wait_stopped(helper_pid); }
      await remove_owned_fixture(directory);
    },
  };
}

test('Unix natural exit stops remaining helpers in the owned process group', { skip: process.platform === 'win32' }, async () => {
  const fixture = await descendant_fixture(false);
  try {
    const result = await run_check(fixture.check, { root: process.cwd(), env: { ...process.env } });
    assert.equal(result.status, 'passed');
    assert.equal(result.foreground_exit, 'confirmed');
    assert.equal(result.cleanup_scope, 'foreground_and_owned_unix_process_group');
    assert.equal(result.descendant_cleanup, 'not_verified');
    await wait_stopped(fixture.read_pid());
  } finally { await fixture.close(); }
});

test('detached helpers are explicitly unverified and remain the trusted suite responsibility', async () => {
  const fixture = await descendant_fixture(true);
  try {
    const result = await run_check(fixture.check, { root: process.cwd(), env: { ...process.env } });
    assert.equal(result.status, 'passed');
    assert.equal(result.command_evidence, 'foreground_exit_only');
    assert.equal(result.descendant_cleanup, 'not_verified');
    assert.equal(result.cleanup_scope, process.platform === 'win32' ? 'foreground_and_best_effort_windows_tree' : 'foreground_and_owned_unix_process_group');
    assert.equal(process_running(fixture.read_pid()), true, 'fixture must demonstrate the detached-helper limitation');
  } finally { await fixture.close(); }
});

test('cleanup failure retains runtime state while a real foreground process is live', async () => {
  let owned_directory;
  let foreground;
  let exited;
  try {
    const report = await execute_fixture_plan(verification_plan('node'), { root: process.cwd(), run: async (check, context) => {
      owned_directory = dirname(context.env.AAG_HOME_DIR);
      foreground = spawn(process.execPath, ['-e', "process.stdout.write('ready');setInterval(()=>{},1000)"], { env: context.env, windowsHide: true, stdio: ['ignore', 'pipe', 'ignore'] });
      exited = once(foreground, 'exit');
      await once(foreground.stdout, 'data');
      return { status: 'cleanup_failed', exit_code: null, duration_ms: 1, foreground_exit: 'not_confirmed' };
    } });
    assert.equal(report.status, 'cleanup_failed');
    assert.equal(report.runtime_state, 'retained');
    assert.equal(report.retained_state_id, basename(owned_directory));
    assert.equal(report.results.length, 1);
    assert.equal(existsSync(join(owned_directory, 'config')), true);
    assert.equal(existsSync(join(owned_directory, 'home')), true);
    assert.equal(process_running(foreground.pid), true);
  } finally {
    if (foreground && !foreground.exitCode && !foreground.signalCode) foreground.kill('SIGKILL');
    if (exited) await exited;
    if (owned_directory) await remove_owned_fixture(owned_directory);
  }
});

test('unavailable executables report not-started foreground evidence', async () => {
  const result = await run_check({ runtime: 'cargo', cwd: '.', args: ['--version'] }, { root: process.cwd(), env: { PATH: '' } });
  assert.equal(result.status, 'unavailable');
  assert.equal(result.foreground_exit, 'not_started');
  assert.equal(result.descendant_cleanup, 'not_verified');
});

test('failed scope preflight reports prerequisites and executes no checks', async () => {
  let inspected_scope;
  const report = await execute_plan(verification_plan('ui'), {
    root: process.cwd(),
    preflight: async ({ scope }) => {
      inspected_scope = scope;
      return { status: 'failed', checks: [{ id: 'browser', status: 'failed' }] };
    },
    run: () => { throw new Error('must not execute'); },
  });
  assert.equal(inspected_scope, 'ui');
  assert.equal(report.status, 'failed');
  assert.equal(report.runtime_state, 'not_created');
  assert.equal(report.results.length, 0);
  assert.equal(report.unrun_checks.length, verification_plan('ui').checks.length);
  assert.equal(report.preflight.checks[0].id, 'browser');
});

test('linked temporary roots become canonical child environments and clean up safely', async () => {
  const fixture_root = await mkdtemp(join(await realpath(tmpdir()), 'aag_runner_fixture_'));
  const target = join(fixture_root, 'real_temp');
  const alias = join(fixture_root, 'alias_temp');
  await mkdir(target);
  await symlink(target, alias, process.platform === 'win32' ? 'junction' : 'dir');
  let observed_root;
  try {
    const plan = { scope: 'node', checks: [{ id: 'canonical_child', command: 'fixture', runtime: 'node', cwd: '.', args: ['-e', `
      const assert = require('node:assert/strict');
      const fs = require('node:fs');
      const os = require('node:os');
      for (const key of ['TMPDIR', 'TMP', 'TEMP', 'AAG_CONFIG_DIR', 'AAG_HOME_DIR']) {
        assert.equal(fs.realpathSync(process.env[key]), process.env[key]);
        if (process.platform !== 'win32') assert.equal(fs.statSync(process.env[key]).mode & 0o777, 0o700);
      }
      assert.equal(os.tmpdir(), process.env.TMPDIR);
      fs.mkdtempSync(require('node:path').join(os.tmpdir(), 'owned_child_'));
    `] }] };
    const report = await execute_fixture_plan(plan, {
      root: process.cwd(), temporary_directory: alias,
      run: async (check, context) => {
        observed_root = dirname(context.env.AAG_HOME_DIR);
        assert.equal(dirname(observed_root), await realpath(target));
        return run_check(check, context);
      },
    });
    assert.equal(report.status, 'passed');
    assert.equal(report.runtime_state, 'removed');
    assert.equal(existsSync(observed_root), false);
    assert.equal(existsSync(alias), true);
  } finally {
    await remove_owned_fixture(fixture_root);
  }
});
