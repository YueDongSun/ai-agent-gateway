import { execFile, spawn } from 'node:child_process';
import { mkdtemp, mkdir, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, dirname, basename, resolve } from 'node:path';
import { promisify } from 'node:util';
import { canonical_temporary_directory, inspect_desktop_environment } from '../desktop_environment.mjs';

const command_timeout_ms = 15 * 60 * 1000;
const cleanup_timeout_ms = 10_000;
const cleanup_scope = process.platform === 'win32'
  ? 'foreground_and_best_effort_windows_tree'
  : 'foreground_and_owned_unix_process_group';
const cleanup_evidence = { cleanup_scope, descendant_cleanup: 'not_verified', command_evidence: 'foreground_exit_only' };

const foreground_exited = (child) => child.exitCode !== null || child.signalCode !== null;
const exec_file = promisify(execFile);

async function inspect_macos_groups() {
  const { stdout } = await exec_file('/bin/ps', ['-axo', 'pgid=,stat='], {
    timeout: 2_000, maxBuffer: 1024 * 1024, killSignal: 'SIGKILL',
    env: { ...process.env, LC_ALL: 'C' },
  });
  return stdout;
}

export async function macos_group_is_inactive(group_id, inspect = inspect_macos_groups) {
  const rows = (await inspect()).trim().split('\n');
  // A failed/truncated/unrecognized process table cannot prove cleanup. Only
  // numeric process groups and state metadata are read; never command lines.
  const states = rows.map((row) => row.trim().match(/^(\d+)\s+([A-Za-z][A-Za-z0-9+<>=-]*)$/));
  if (states.some((row) => !row)) throw new Error('AAG_CLEANUP_INSPECTION_FAILED');
  return states.every((row) => Number(row[1]) !== group_id || row[2].startsWith('Z'));
}

/** Detached helpers remain the trusted test suite's responsibility. */
export async function stop_owned_child(child, {
  platform = process.platform, kill = process.kill, inactive_group = macos_group_is_inactive,
} = {}) {
  if (!child.pid) return;
  if (platform === 'win32') {
    // taskkill cannot establish descendant ownership after the parent exits.
    if (foreground_exited(child)) return;
    await new Promise((resolve_stop, reject_stop) => {
      const killer = spawn(join(process.env.SystemRoot || 'C:\\Windows', 'System32', 'taskkill.exe'), ['/PID', String(child.pid), '/T', '/F'], { shell: false, windowsHide: true, stdio: 'ignore' });
      const timer = setTimeout(() => { killer.kill(); reject_stop(new Error('AAG_CLEANUP_TIMEOUT')); }, cleanup_timeout_ms);
      killer.once('error', () => { clearTimeout(timer); reject_stop(new Error('AAG_CLEANUP_FAILED')); });
      killer.once('exit', (code) => { clearTimeout(timer); if (code === 0 || foreground_exited(child)) resolve_stop(); else reject_stop(new Error('AAG_CLEANUP_FAILED')); });
    });
  } else {
    // The owned group may outlive its leader, including after a successful exit.
    try { kill(-child.pid, 'SIGKILL'); }
    catch (error) {
      if (error.code === 'ESRCH') return;
      // Darwin returns EPERM (including for signal 0) when a group contains
      // only unreaped zombies. Require a completed leader AND independent
      // bounded evidence that no live member remains; never waive denial.
      if (platform === 'darwin' && error.code === 'EPERM' && foreground_exited(child)) {
        try { if (await inactive_group(child.pid)) return; } catch { /* Fail closed below. */ }
      }
      throw Object.assign(new Error('AAG_CLEANUP_FAILED'), { code: error.code });
    }
  }
}

async function bounded_exit(exit, timeout_ms) {
  let timer;
  try { return await Promise.race([exit, new Promise((resolve_wait) => { timer = setTimeout(() => resolve_wait(null), timeout_ms); })]); }
  finally { clearTimeout(timer); }
}

export async function run_check(check, { root, env, signal, timeout_ms = command_timeout_ms }) {
  if (signal?.aborted) return { status: 'cancelled', exit_code: null, duration_ms: 0, foreground_exit: 'not_started', ...cleanup_evidence };
  const started = Date.now();
  const executable = check.runtime === 'node' ? process.execPath : 'cargo';
  let child;
  try { child = spawn(executable, check.args, { cwd: resolve(root, check.cwd), env, shell: false, windowsHide: true, detached: process.platform !== 'win32', stdio: ['ignore', 'inherit', 'inherit'] }); }
  catch { return { status: 'unavailable', exit_code: null, duration_ms: Date.now() - started, foreground_exit: 'not_started', ...cleanup_evidence }; }
  const exit = new Promise((resolve_exit) => {
    child.once('error', () => resolve_exit({ status: 'unavailable', exit_code: null }));
    child.once('exit', (code) => resolve_exit({ status: code === 0 ? 'passed' : 'failed', exit_code: code }));
  });
  let interrupt;
  const interruption = new Promise((resolve_interrupt) => { interrupt = (status) => resolve_interrupt({ status, exit_code: null }); });
  const cancel = () => interrupt('cancelled');
  const timer = setTimeout(() => interrupt('timed_out'), timeout_ms);
  signal?.addEventListener('abort', cancel, { once: true });
  if (signal?.aborted) cancel();
  let result;
  try { result = await Promise.race([exit, interruption]); }
  finally { clearTimeout(timer); signal?.removeEventListener('abort', cancel); }

  let cleanup_failed = false;
  let cleanup_error_code;
  try { await stop_owned_child(child); }
  catch (error) {
    cleanup_failed = true;
    cleanup_error_code = typeof error.code === 'string' && /^[A-Z0-9_]{1,64}$/.test(error.code)
      ? error.code : 'AAG_CLEANUP_FAILED';
  }
  if (child.pid && !foreground_exited(child)) {
    // A failed tree operation must still try to terminate its direct child.
    if (cleanup_failed) { try { child.kill('SIGKILL'); } catch { /* Reported below. */ } }
    if (!await bounded_exit(exit, cleanup_timeout_ms) || !foreground_exited(child)) cleanup_failed = true;
  }
  const foreground_exit = !child.pid ? 'not_started' : foreground_exited(child) ? 'confirmed' : 'not_confirmed';
  if (foreground_exit === 'not_confirmed') child.unref();
  return { ...result, status: cleanup_failed ? 'cleanup_failed' : result.status, duration_ms: Date.now() - started, foreground_exit, ...cleanup_evidence,
    ...(cleanup_failed ? { cleanup_error_code: cleanup_error_code ?? 'AAG_FOREGROUND_EXIT_UNCONFIRMED' } : {}) };
}

/** Public orchestration seam: bounded allowlisted plans, no command strings. */
export async function execute_plan(plan, {
  root, signal, run = run_check, on_check = () => {},
  preflight = inspect_desktop_environment, temporary_directory = tmpdir(),
}) {
  const environment = signal?.aborted ? { status: 'cancelled', checks: [] }
    : await preflight({ root, scope: plan.scope, signal });
  if (environment.status !== 'passed') {
    return { schema_version: 1, scope: plan.scope, status: environment.status, preflight: environment,
      results: [], unrun_checks: plan.checks.map((check) => check.id), runtime_state: 'not_created',
      evidence_class: 'local_and_synthetic_only', ...cleanup_evidence };
  }
  const temporary_root = await canonical_temporary_directory(temporary_directory);
  const owned_root = await mkdtemp(join(temporary_root, 'aag_feature_verify_'));
  const config_dir = join(owned_root, 'config'); const home_dir = join(owned_root, 'home');
  const temporary_dir = join(owned_root, 'tmp');
  const results = [];
  let status = 'passed';
  let retain_state = false;
  try {
    await mkdir(config_dir, { mode: 0o700 }); await mkdir(home_dir, { mode: 0o700 }); await mkdir(temporary_dir, { mode: 0o700 });
    const env = { ...process.env, AAG_CONFIG_DIR: config_dir, AAG_HOME_DIR: home_dir,
      TMPDIR: temporary_dir, TMP: temporary_dir, TEMP: temporary_dir };
    delete env.AAG_TEST_CCSWITCH_DB;
    for (const check of plan.checks) {
      if (signal?.aborted) { status = 'cancelled'; break; }
      on_check(check);
      // An exception or missing foreground evidence is not permission to remove
      // a running command's configuration. Keep the owned state for recovery.
      retain_state = true;
      let result;
      try { result = await run(check, { root, env, signal }); }
      catch { result = { status: 'cleanup_failed', exit_code: null, foreground_exit: 'not_confirmed', error_code: 'AAG_CHECK_RUNNER_FAILED', ...cleanup_evidence }; }
      retain_state = result.status === 'cleanup_failed' || !['confirmed', 'not_started'].includes(result.foreground_exit);
      if (retain_state && result.status === 'passed') result = { ...result, status: 'cleanup_failed' };
      results.push({ id: check.id, command: check.command, ...result });
      if (result.status !== 'passed') { status = result.status; break; }
    }
  } finally {
    if (dirname(resolve(owned_root)) !== temporary_root || !basename(owned_root).startsWith('aag_feature_verify_')) throw new Error('AAG_UNEXPECTED_CLEANUP_PATH');
    if (!retain_state) await rm(owned_root, { recursive: true, force: true, maxRetries: 3 });
  }
  return { schema_version: 1, scope: plan.scope, status, preflight: environment, results, unrun_checks: plan.checks.slice(results.length).map((check) => check.id), evidence_class: 'local_and_synthetic_only', ...cleanup_evidence,
    runtime_state: retain_state ? 'retained' : 'removed', ...(retain_state ? { retained_state_id: basename(owned_root) } : {}) };
}
