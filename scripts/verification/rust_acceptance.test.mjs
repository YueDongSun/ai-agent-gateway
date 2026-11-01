import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { test } from 'node:test';
import { main, execute_rust_profile, bind_abort_signals } from './rust_acceptance.mjs';
import { windows_qualification_ids } from './rust_qualification_catalog.mjs';

const disabled_privilege_probe = {
  schema_version: 1,
  status: 'passed',
  evidence_class: 'local_prerequisites_only',
  process_token_all_access: true,
  thread_not_impersonating: true,
  primary_process_token: true,
  token_has_restrictions_zero: true,
  is_token_restricted_false: true,
  logon_sid_count: 1,
  enabled_non_deny_logon_sid_count: 1,
  privileges_query_succeeded: true,
  se_backup_available: true,
  se_backup_enabled: false,
  se_restore_available: true,
  se_restore_enabled: false,
  se_security_available: true,
  se_security_enabled: false,
  robocopy_contract: true,
  directory_symlink: true,
  codes: [],
};

function passing_test_receipt(profile, platform) {
  return profile === 'windows_qualification'
    ? {
      schema_version: 1,
      profile,
      evidence_class: 'local-only',
      status: 'passed',
      requested_test_ids: [...windows_qualification_ids],
      executed_test_ids: [...windows_qualification_ids],
      unrun_test_ids: [],
      passed_count: windows_qualification_ids.length,
    }
    : {
      schema_version: 1,
      profile,
      evidence_class: 'local-only',
      status: 'passed',
      excluded_test_ids: platform === 'win32' ? [...windows_qualification_ids] : [],
      counts: { passed: 2, failed: 0, ignored: 0 },
      test_names_discovered: 3,
      test_names_selected: 3,
    };
}

function passing_run(platform, observations = []) {
  return async (check, context) => {
    observations.push({ check, context });
    assert.equal(existsSync(context.env.AAG_CONFIG_DIR), true);
    assert.equal(existsSync(context.env.AAG_HOME_DIR), true);
    assert.equal(context.env.AAG_RUST_TEST_RECEIPT_PATH.startsWith(context.env.AAG_CONFIG_DIR), true);
    const profile = check.id.slice('rust_'.length, -'_tests'.length);
    return {
      status: 'passed',
      exit_code: 0,
      duration_ms: 1,
      foreground_exit: 'confirmed',
      test_receipt: passing_test_receipt(profile, platform),
    };
  };
}

function passing_tool_probe() {
  return async (executable) => ({ status: 'passed', stdout: executable === 'rustc' ? 'rustc 1.88.0 (fixture)' : 'cargo 1.88.0 (fixture)' });
}

test('default CLI request plans all profiles without probing or creating state', async () => {
  let probed = false;
  let ran = false;
  const result = await main({
    argv: [],
    platform: 'win32',
    probe: async () => { probed = true; throw new Error('plan-only must not probe'); },
    run: async () => { ran = true; throw new Error('plan-only must not run'); },
  });
  assert.equal(result.exit_code, 0);
  assert.equal(result.receipt.profile, 'all');
  assert.equal(result.receipt.mode, 'plan_only');
  assert.equal(result.receipt.evidence_class, 'local-only');
  assert.equal(probed, false);
  assert.equal(ran, false);
});

test('unknown CLI arguments are rejected without invoking execution seams', async () => {
  let invoked = false;
  const result = await main({
    argv: ['--profile', 'ordinary', '--unexpected'],
    probe: async () => { invoked = true; },
    run: async () => { invoked = true; },
  });
  assert.equal(result.exit_code, 2);
  assert.equal(result.receipt.error_code, 'invalid_arguments');
  assert.equal(invoked, false);
});

test('blocked Windows preflight creates no AAG state and leaves every qualification ID unrun', async () => {
  let run_calls = 0;
  let tool_probe_calls = 0;
  const receipt = await execute_rust_profile('windows_qualification', {
    platform: 'win32',
    windows_probe: async () => ({ status: 'blocked', codes: ['robocopy_contract_unproven'] }),
    probe: async () => { tool_probe_calls++; return { status: 'passed', stdout: 'cargo 1.88.0' }; },
    run: async () => { run_calls++; throw new Error('blocked preflight must not start Cargo'); },
  });
  assert.equal(receipt.status, 'blocked');
  assert.equal(receipt.runtime_state, 'not_created');
  assert.equal(receipt.tests_executed, 0);
  assert.deepEqual(receipt.executed_test_ids, []);
  assert.deepEqual(receipt.unrun_test_ids, [...windows_qualification_ids]);
  assert.equal(run_calls, 0);
  assert.equal(tool_probe_calls, 0);
});

test('explicit non-Windows qualification is nonzero and not applicable', async () => {
  let windows_probe_calls = 0;
  let run_calls = 0;
  const result = await main({
    argv: ['--profile', 'windows_qualification', '--run'],
    platform: 'linux',
    windows_probe: async () => { windows_probe_calls++; throw new Error('must not probe'); },
    run: async () => { run_calls++; throw new Error('must not run'); },
  });
  assert.equal(result.exit_code, 1);
  assert.equal(result.receipt.status, 'not_applicable');
  assert.equal(result.receipt.runtime_state, 'not_created');
  assert.deepEqual(result.receipt.unrun_test_ids, [...windows_qualification_ids]);
  assert.equal(windows_probe_calls, 0);
  assert.equal(run_calls, 0);
});

test('all runs ordinary tests even when Windows qualification is blocked', async () => {
  const observations = [];
  const original = new Map(['AAG_TEST_CCSWITCH_DB', 'AAG_MODEL_DISCOVERY_PROBE', 'AAG_LIVE_CANARY_MODEL']
    .map((name) => [name, process.env[name]]));
  try {
    process.env.AAG_TEST_CCSWITCH_DB = 'fixture_live_database';
    process.env.AAG_MODEL_DISCOVERY_PROBE = 'fixture_live_probe';
    process.env.AAG_LIVE_CANARY_MODEL = 'fixture_live_model';
    const result = await main({
      argv: ['--profile', 'all', '--run'],
      platform: 'win32',
      probe: passing_tool_probe(),
      windows_probe: async () => ({ status: 'blocked', codes: ['directory_symlink_unavailable'] }),
      run: passing_run('win32', observations),
    });
    assert.equal(result.exit_code, 1);
    assert.equal(result.receipt.status, 'blocked');
    assert.equal(result.receipt.ordinary.status, 'passed');
    assert.equal(result.receipt.windows_qualification.status, 'blocked');
    assert.equal(result.receipt.windows_qualification.tests_executed, 0);
    assert.deepEqual(result.receipt.windows_qualification.unrun_test_ids, [...windows_qualification_ids]);
    assert.equal(observations.length, 1);
    const env = observations[0].context.env;
    assert.equal(env.AAG_TEST_CCSWITCH_DB, undefined);
    assert.equal(env.AAG_MODEL_DISCOVERY_PROBE, undefined);
    assert.equal(env.AAG_LIVE_CANARY_MODEL, undefined);
    assert.notEqual(env.AAG_CONFIG_DIR, process.env.AAG_CONFIG_DIR);
    assert.notEqual(env.AAG_HOME_DIR, process.env.AAG_HOME_DIR);
    assert.equal(JSON.stringify(result.receipt).includes(env.AAG_CONFIG_DIR), false);
  } finally {
    for (const [name, value] of original) {
      if (value === undefined) delete process.env[name];
      else process.env[name] = value;
    }
  }
});

test('disabled but assigned privileges pass prerequisite classification without enabling them', async () => {
  const observations = [];
  const receipt = await execute_rust_profile('windows_qualification', {
    platform: 'win32',
    windows_probe: async () => disabled_privilege_probe,
    probe: passing_tool_probe(),
    run: passing_run('win32', observations),
  });
  assert.equal(receipt.status, 'passed');
  assert.equal(receipt.tests_executed, 26);
  assert.deepEqual(receipt.unrun_test_ids, []);
  assert.equal(receipt.preflight.probe.se_backup_available, true);
  assert.equal(receipt.preflight.probe.se_backup_enabled, false);
  assert.equal(observations.length, 1);
  assert.match(receipt.acceptance_boundary, /subset only/);
});

test('an enabled-but-unassigned privilege claim cannot qualify the preflight', async () => {
  const unassigned_probe = {
    ...disabled_privilege_probe,
    se_backup_available: false,
    se_backup_enabled: true,
  };
  let run_calls = 0;
  const receipt = await execute_rust_profile('windows_qualification', {
    platform: 'win32',
    windows_probe: async () => unassigned_probe,
    probe: passing_tool_probe(),
    run: async () => { run_calls++; throw new Error('unassigned privilege must block before Cargo'); },
  });
  assert.equal(receipt.status, 'blocked');
  assert.equal(receipt.runtime_state, 'not_created');
  assert.equal(receipt.tests_executed, 0);
  assert.equal(run_calls, 0);
});

test('Rust test helper failure sets a nonzero CLI result', async () => {
  const result = await main({
    argv: ['--profile', 'ordinary', '--run'],
    platform: 'linux',
    probe: passing_tool_probe(),
    run: async () => ({ status: 'failed', exit_code: 101, duration_ms: 1, foreground_exit: 'confirmed' }),
  });
  assert.equal(result.exit_code, 1);
  assert.equal(result.receipt.status, 'failed');
});

test('pre-cancelled run creates no state and executes no test helper', async () => {
  const controller = new AbortController();
  controller.abort();
  let run_calls = 0;
  const result = await main({
    argv: ['--profile', 'windows_qualification', '--run'],
    platform: 'win32',
    signal: controller.signal,
    windows_probe: async () => { throw new Error('cancelled run must not probe'); },
    run: async () => { run_calls++; throw new Error('cancelled run must not execute'); },
  });
  assert.equal(result.exit_code, 1);
  assert.equal(result.receipt.status, 'cancelled');
  assert.equal(result.receipt.runtime_state, 'not_created');
  assert.equal(result.receipt.tests_executed, 0);
  assert.deepEqual(result.receipt.unrun_test_ids, [...windows_qualification_ids]);
  assert.equal(run_calls, 0);
});

test('CLI signal binding aborts the controller on SIGINT and SIGTERM and unbinds', async () => {
  const { EventEmitter } = await import('node:events');
  for (const signal of ['SIGINT', 'SIGTERM']) {
    const controller = new AbortController();
    const emitter = new EventEmitter();
    const unbind = bind_abort_signals(controller, emitter);
    assert.equal(controller.signal.aborted, false);
    emitter.emit(signal);
    assert.equal(controller.signal.aborted, true);
    unbind();
    assert.equal(emitter.listenerCount(signal), 0);
  }
  const controller = new AbortController();
  const emitter = new EventEmitter();
  const unbind = bind_abort_signals(controller, emitter);
  unbind();
  emitter.emit('SIGINT');
  assert.equal(controller.signal.aborted, false, 'unbound listeners must not abort');
});

test('read-only PowerShell preflight has no privilege adjustment or OS configuration calls', () => {
  const script = readFileSync(new URL('./windows_qualification_probe.ps1', import.meta.url), 'utf8');
  assert.match(script, /TokenInformation\(token, 21\)/);
  assert.match(script, /TokenAllAccess = 0x000F01FF/);
  assert.match(script, /SymbolicLinkFlagAllowUnprivilegedCreate = 0x2/);
  assert.match(script, /IntPtr buffer = Marshal\.AllocHGlobal/);
  assert.match(script, /Marshal\.FreeHGlobal\(buffer\)/);
  assert.doesNotMatch(script, /Marshal\.ReadInt32\(sid, 12\)/, 'logon SID check must not fix the second subauthority');
  assert.doesNotMatch(script, /\b(?:AdjustTokenPrivileges|SetTokenInformation|RegSetValueExW|RegCreateKeyExW|CreateServiceW|StartServiceW|Lsa[A-Z]\w*)\s*\(/);
});
