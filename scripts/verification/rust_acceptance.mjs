import { existsSync } from 'node:fs';
import { readFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { canonical_temporary_directory, probe_command } from '../desktop_environment.mjs';
import { execute_plan, run_check } from './runner.mjs';
import { rust_qualification_plan, windows_qualification_ids } from './rust_qualification_catalog.mjs';

const repository_root = fileURLToPath(new URL('../../', import.meta.url));
const probe_script = fileURLToPath(new URL('./windows_qualification_probe.ps1', import.meta.url));
const helper_script = fileURLToPath(new URL('./rust_test_check.mjs', import.meta.url));
const minimum_rust_version = [1, 88, 0];
const probe_timeout_ms = 15_000;
const profiles = Object.freeze(['ordinary', 'windows_qualification', 'all']);
const probe_boolean_fields = Object.freeze([
  'process_token_all_access', 'thread_not_impersonating', 'primary_process_token',
  'token_has_restrictions_zero', 'is_token_restricted_false', 'privileges_query_succeeded',
  'se_backup_available', 'se_backup_enabled', 'se_restore_available', 'se_restore_enabled',
  'se_security_available', 'se_security_enabled', 'robocopy_contract', 'directory_symlink',
]);
const probe_required_boolean_fields = Object.freeze([
  'process_token_all_access', 'thread_not_impersonating', 'primary_process_token',
  'token_has_restrictions_zero', 'is_token_restricted_false', 'privileges_query_succeeded',
  'se_backup_available', 'se_restore_available', 'se_security_available',
  'robocopy_contract', 'directory_symlink',
]);
const probe_code_allowlist = new Set([
  'process_token_all_access_unavailable', 'thread_impersonation_present_or_unproven',
  'original_primary_token_unproven', 'token_has_restrictions_nonzero_or_unavailable',
  'token_is_restricted_or_unavailable', 'enabled_logon_sid_requirement_unproven',
  'privilege_query_unavailable', 'required_privilege_unavailable',
  'robocopy_contract_unproven', 'directory_symlink_unavailable', 'probe_internal_failure',
  'probe_requirements_unproven',
]);

function version_from(output) {
  const match = output.match(/\b(\d+)\.(\d+)\.(\d+)\b/);
  return match ? match.slice(1).map(Number) : null;
}

function version_satisfies(version, minimum) {
  return version && version.reduce((result, component, index) => result || Math.sign(component - minimum[index]), 0) >= 0;
}

async function inspect_rust_tools({ signal, env = process.env, probe = probe_command } = {}) {
  if (signal?.aborted) return { status: 'cancelled', checks: [{ id: 'rust_tools', status: 'cancelled', code: 'cancelled' }] };
  const checks = [];
  for (const [id, executable, args] of [
    ['rustc', 'rustc', ['--version']],
    ['cargo', 'cargo', ['--version']],
  ]) {
    const result = await probe(executable, args, { env, signal, timeout_ms: 5_000 });
    const version = result.status === 'passed' ? version_from(result.stdout) : null;
    const passed = version_satisfies(version, minimum_rust_version);
    checks.push({ id, status: passed ? 'passed' : 'failed', code: passed ? undefined : result.status === 'passed' ? 'rust_tool_version_unsupported' : `rust_tool_${result.status}` });
    if (!passed) break;
  }
  return { status: checks.every(({ status }) => status === 'passed') ? 'passed' : 'blocked', checks, evidence_class: 'local_prerequisites_only' };
}

function safe_probe_payload(value) {
  if (!value || typeof value !== 'object') return null;
  const codes = Array.isArray(value.codes)
    ? value.codes.filter((code) => typeof code === 'string' && probe_code_allowlist.has(code))
    : [];
  const valid_counts = Number.isInteger(value.logon_sid_count) && value.logon_sid_count >= 0
    && Number.isInteger(value.enabled_non_deny_logon_sid_count) && value.enabled_non_deny_logon_sid_count >= 0;
  if (value.schema_version !== 1 || !valid_counts
      || probe_boolean_fields.some((field) => typeof value[field] !== 'boolean')
      || !Array.isArray(value.codes) || codes.length !== value.codes.length) return null;
  const flags = Object.fromEntries(probe_boolean_fields.map((field) => [field, value[field]]));
  const all_requirements = probe_required_boolean_fields.every((field) => value[field])
    && value.logon_sid_count === 1
    && value.enabled_non_deny_logon_sid_count === 1;
  const status = value.status === 'passed' && all_requirements && codes.length === 0 ? 'passed' : 'blocked';
  return {
    schema_version: 1,
    status,
    evidence_class: 'local_prerequisites_only',
    ...flags,
    logon_sid_count: value.logon_sid_count,
    enabled_non_deny_logon_sid_count: value.enabled_non_deny_logon_sid_count,
    codes: codes.length ? codes : all_requirements ? [] : ['probe_requirements_unproven'],
  };
}

function system_powershell(env) {
  const system_root_entry = Object.entries(env).find(([name]) => name.toUpperCase() === 'SYSTEMROOT');
  return system_root_entry?.[1]
    ? join(system_root_entry[1], 'System32', 'WindowsPowerShell', 'v1.0', 'powershell.exe')
    : null;
}

async function run_windows_probe({ signal, env = process.env, platform = process.platform, probe = probe_command } = {}) {
  if (platform !== 'win32') return { status: 'not_applicable', codes: ['windows_only_profile'] };
  if (signal?.aborted) return { status: 'cancelled', codes: ['cancelled'] };
  const executable = system_powershell(env);
  if (!executable || !existsSync(probe_script)) return { status: 'blocked', codes: ['windows_probe_unavailable'] };
  const result = await probe(executable, ['-NoLogo', '-NoProfile', '-NonInteractive', '-File', probe_script], {
    env, signal, timeout_ms: probe_timeout_ms,
  });
  if (result.status !== 'passed') {
    const code = result.status === 'cancelled' ? 'cancelled' : `windows_probe_${result.status}`;
    return { status: result.status === 'cancelled' ? 'cancelled' : 'blocked', codes: [code] };
  }
  try {
    const payload = safe_probe_payload(JSON.parse(result.stdout));
    return payload ?? { status: 'blocked', codes: ['windows_probe_output_invalid'] };
  } catch {
    return { status: 'blocked', codes: ['windows_probe_output_invalid'] };
  }
}

async function ordinary_preflight({ signal }, { probe, platform }) {
  if (signal?.aborted) return { status: 'cancelled', checks: [{ id: 'rust_tools', status: 'cancelled', code: 'cancelled' }] };
  const rust = await inspect_rust_tools({ signal, probe });
  let temporary_directory = { status: 'passed' };
  try { await canonical_temporary_directory(); }
  catch { temporary_directory = { status: 'failed', code: 'temporary_directory_unavailable' }; }
  const checks = [
    ...rust.checks,
    { id: 'temporary_directory', ...temporary_directory },
    { id: 'platform', status: 'passed', platform },
  ];
  return {
    schema_version: 1,
    status: signal?.aborted ? 'cancelled' : rust.status === 'passed' && temporary_directory.status === 'passed' ? 'passed' : 'blocked',
    checks,
    evidence_class: 'local_prerequisites_only',
  };
}

async function windows_preflight({ signal }, { platform, probe, windows_probe }) {
  if (platform !== 'win32') {
    return {
      schema_version: 1,
      status: 'not_applicable',
      checks: [{ id: 'windows_platform', status: 'not_applicable', code: 'windows_only_profile' }],
      evidence_class: 'local_prerequisites_only',
    };
  }
  if (signal?.aborted) {
    return {
      schema_version: 1,
      status: 'cancelled',
      checks: [{ id: 'windows_probe', status: 'cancelled', code: 'cancelled' }],
      evidence_class: 'local_prerequisites_only',
    };
  }
  const raw_probe = await windows_probe({ signal, platform, probe });
  const payload = safe_probe_payload(raw_probe);
  if (!payload || payload.status !== 'passed') {
    return {
      schema_version: 1,
      status: raw_probe?.status === 'cancelled' ? 'cancelled' : 'blocked',
      checks: [{ id: 'windows_qualification_prerequisites', status: 'blocked', codes: payload?.codes ?? raw_probe?.codes ?? ['windows_probe_unproven'] }],
      ...(payload ? { probe: payload } : {}),
      evidence_class: 'local_prerequisites_only',
    };
  }
  const rust = await inspect_rust_tools({ signal, probe });
  let temporary_directory = { status: 'passed' };
  try { await canonical_temporary_directory(); }
  catch { temporary_directory = { status: 'failed', code: 'temporary_directory_unavailable' }; }
  const checks = [
    { id: 'windows_qualification_prerequisites', status: 'passed', probe: payload },
    ...rust.checks,
    { id: 'temporary_directory', ...temporary_directory },
  ];
  return {
    schema_version: 1,
    status: signal?.aborted ? 'cancelled' : rust.status === 'passed' && temporary_directory.status === 'passed' ? 'passed' : 'blocked',
    checks,
    probe: payload,
    evidence_class: 'local_prerequisites_only',
  };
}

function make_check(profile) {
  return {
    schema_version: 1,
    scope: `rust_${profile}`,
    checks: [{
      id: `rust_${profile}_tests`,
      command: `node scripts/verification/rust_test_check.mjs ${profile}`,
      runtime: 'node',
      args: [helper_script, profile],
      cwd: '.',
    }],
  };
}

function sanitize_check_result(result) {
  if (!result) return null;
  const test_receipt = result.test_receipt && typeof result.test_receipt === 'object'
    ? result.test_receipt
    : undefined;
  return {
    status: result.status,
    exit_code: Number.isInteger(result.exit_code) ? result.exit_code : null,
    duration_ms: Number.isFinite(result.duration_ms) ? result.duration_ms : null,
    foreground_exit: result.foreground_exit,
    ...(result.cleanup_error_code ? { cleanup_error_code: result.cleanup_error_code } : {}),
    ...(test_receipt ? { test_receipt } : {}),
  };
}

async function read_test_receipt(path) {
  try {
    const value = JSON.parse(await readFile(path, 'utf8'));
    if (value?.schema_version !== 1 || !['ordinary', 'windows_qualification'].includes(value.profile)
        || !['passed', 'failed', 'cancelled', 'timed_out', 'cleanup_failed', 'not_applicable'].includes(value.status)
        || value.evidence_class !== 'local-only') return null;
    return value;
  } catch {
    return null;
  }
}

async function profile_preflight(context, { profile, platform, probe, windows_probe }) {
  return profile === 'windows_qualification'
    ? windows_preflight(context, { platform, probe, windows_probe })
    : ordinary_preflight(context, { platform, probe });
}

export async function execute_rust_profile(profile, {
  platform = process.platform,
  root = repository_root,
  signal,
  probe = probe_command,
  windows_probe = run_windows_probe,
  execute = execute_plan,
  run = run_check,
  temporary_directory,
} = {}) {
  if (!['ordinary', 'windows_qualification'].includes(profile)) throw new Error('AAG_INVALID_RUST_PROFILE');
  const plan = make_check(profile);
  const preflight = (context) => profile_preflight(context, { profile, platform, probe, windows_probe });
  const isolated_run = async (check, context) => {
    const config_dir = resolve(context.env.AAG_CONFIG_DIR);
    const receipt_path = join(config_dir, 'rust_test_receipt.json');
    const child_env = { ...context.env };
    for (const name of Object.keys(child_env)) {
      if (name.toUpperCase() === 'AAG_RUST_TEST_RECEIPT_PATH') delete child_env[name];
    }
    child_env.AAG_RUST_TEST_RECEIPT_PATH = receipt_path;
    const result = await run(check, { ...context, env: child_env });
    const test_receipt = result.test_receipt ?? await read_test_receipt(receipt_path);
    if (result.status === 'passed' && !test_receipt) {
      return { ...result, status: 'failed', error_code: 'rust_test_receipt_missing' };
    }
    if (test_receipt && test_receipt.profile !== profile) {
      return { ...result, status: 'failed', error_code: 'rust_test_receipt_profile_mismatch' };
    }
    if (test_receipt && test_receipt.status !== 'passed' && result.status === 'passed') {
      return { ...result, status: 'failed', error_code: 'rust_test_receipt_not_passed', test_receipt };
    }
    return { ...result, ...(test_receipt ? { test_receipt } : {}) };
  };

  const raw = await execute(plan, {
    root,
    signal,
    preflight,
    run: isolated_run,
    ...(temporary_directory ? { temporary_directory } : {}),
  });
  const check_result = sanitize_check_result(raw.results?.[0]);
  const test_receipt = check_result?.test_receipt;
  const blocked_before_run = raw.preflight?.status !== 'passed';
  const qualification = profile === 'windows_qualification';
  const test_ids = qualification ? [...windows_qualification_ids] : undefined;
  const unrun_test_ids = qualification
    ? blocked_before_run ? test_ids : test_receipt?.unrun_test_ids ?? []
    : undefined;

  return {
    schema_version: 1,
    profile,
    mode: 'executed',
    status: raw.status,
    platform,
    evidence_class: 'local-only',
    preflight: raw.preflight,
    check: check_result,
    runtime_state: raw.runtime_state,
    ...(qualification ? {
      requested_test_ids: test_ids,
      executed_test_ids: test_receipt?.executed_test_ids ?? [],
      unrun_test_ids,
      tests_executed: test_receipt?.executed_test_ids?.length ?? 0,
    } : {
      excluded_test_ids: test_receipt?.excluded_test_ids ?? (platform === 'win32' ? rust_qualification_plan('ordinary', platform).ordinary.excluded_test_ids : []),
      tests_passed: test_receipt?.counts?.passed ?? test_receipt?.passed_count ?? 0,
      tests_ignored: test_receipt?.counts?.ignored ?? 0,
    }),
    acceptance_boundary: qualification
      ? 'Windows qualification subset only; not full product acceptance'
      : 'Ordinary Rust tests only; not full qualification or product acceptance',
  };
}

function aggregate_status(receipts) {
  const statuses = receipts.map(({ status }) => status);
  if (statuses.includes('cancelled')) return 'cancelled';
  if (statuses.includes('failed') || statuses.includes('cleanup_failed') || statuses.includes('timed_out') || statuses.includes('unavailable')) return 'failed';
  if (statuses.includes('blocked')) return 'blocked';
  if (statuses.includes('not_applicable')) return 'not_applicable';
  return statuses.every((status) => status === 'passed') ? 'passed' : 'failed';
}

function plan_receipt(profile, platform) {
  const plan = rust_qualification_plan(profile, platform);
  return {
    ...plan,
    ordinary: plan.ordinary ? {
      ...plan.ordinary,
      command: 'node scripts/verification/rust_test_check.mjs ordinary',
      preflight: 'on_execution_only',
    } : undefined,
    windows_qualification: plan.windows_qualification ? {
      ...plan.windows_qualification,
      command: 'node scripts/verification/rust_test_check.mjs windows_qualification',
      preflight: 'on_execution_only',
      platform_note: platform === 'win32' ? 'requires successful read-only Windows preflight' : 'not applicable on this platform when executed',
    } : undefined,
  };
}

function not_started_qualification(platform, code) {
  return {
    schema_version: 1,
    profile: 'windows_qualification',
    mode: 'executed',
    status: 'blocked',
    platform,
    evidence_class: 'local-only',
    preflight: {
      schema_version: 1,
      status: 'blocked',
      checks: [{ id: 'owned_child_cleanup', status: 'blocked', code }],
      evidence_class: 'local_prerequisites_only',
    },
    check: null,
    runtime_state: 'not_created',
    requested_test_ids: [...windows_qualification_ids],
    executed_test_ids: [],
    unrun_test_ids: [...windows_qualification_ids],
    tests_executed: 0,
    acceptance_boundary: 'Windows qualification subset not run; not full product acceptance',
  };
}

function parse_arguments(argv) {
  let profile = 'all';
  let profile_seen = false;
  let run = false;
  for (let index = 0; index < argv.length; index++) {
    const argument = argv[index];
    if (argument === '--run' && !run) {
      run = true;
      continue;
    }
    if (argument === '--profile' && !profile_seen && profiles.includes(argv[index + 1])) {
      profile = argv[++index];
      profile_seen = true;
      continue;
    }
    throw new Error('AAG_INVALID_ARGUMENTS');
  }
  return { profile, run };
}

function exit_for_status(status) {
  return status === 'passed' ? 0 : 1;
}

export function bind_abort_signals(controller, emitter = process) {
  const abort = () => controller.abort();
  emitter.on('SIGINT', abort);
  emitter.on('SIGTERM', abort);
  return () => {
    emitter.off('SIGINT', abort);
    emitter.off('SIGTERM', abort);
  };
}

export async function main({ argv = process.argv.slice(2), ...options } = {}) {
  const platform = options.platform ?? process.platform;
  let args;
  try { args = parse_arguments(argv); }
  catch {
    return {
      exit_code: 2,
      receipt: {
        schema_version: 1,
        status: 'failed',
        error_code: 'invalid_arguments',
        evidence_class: 'local-only',
      },
    };
  }
  if (!args.run) return { exit_code: 0, receipt: plan_receipt(args.profile, platform) };

  try {
    if (args.profile === 'ordinary' || args.profile === 'windows_qualification') {
      const receipt = await execute_rust_profile(args.profile, { ...options, platform });
      return { exit_code: exit_for_status(receipt.status), receipt };
    }
    const ordinary = await execute_rust_profile('ordinary', { ...options, platform });
    const cleanup_unconfirmed = ordinary.check
      && !['confirmed', 'not_started'].includes(ordinary.check.foreground_exit);
    const windows_qualification = cleanup_unconfirmed
      ? not_started_qualification(platform, 'ordinary_child_cleanup_unconfirmed')
      : await execute_rust_profile('windows_qualification', { ...options, platform });
    const status = aggregate_status([ordinary, windows_qualification]);
    return {
      exit_code: exit_for_status(status),
      receipt: {
        schema_version: 1,
        profile: 'all',
        mode: 'executed',
        status,
        platform,
        evidence_class: 'local-only',
        ordinary,
        windows_qualification,
        acceptance_boundary: platform === 'win32'
          ? 'Combined ordinary and Windows qualification Rust test streams only; not full product acceptance'
          : 'Ordinary Rust tests may pass, but Windows qualification is not applicable and full Rust qualification is incomplete',
      },
    };
  } catch {
    return {
      exit_code: 1,
      receipt: {
        schema_version: 1,
        profile: args.profile,
        mode: 'executed',
        status: 'failed',
        platform,
        evidence_class: 'local-only',
        error_code: 'rust_acceptance_runner_failed',
      },
    };
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const controller = new AbortController();
  const unbind_abort_signals = bind_abort_signals(controller);
  try {
    const result = await main({ signal: controller.signal });
    process.stdout.write(`${JSON.stringify(result.receipt, null, 2)}\n`);
    process.exitCode = result.exit_code;
  } finally {
    unbind_abort_signals();
  }
}
