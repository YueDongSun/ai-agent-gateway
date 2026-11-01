import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { EventEmitter } from 'node:events';
import { test } from 'node:test';
import { mirror_symlink_ids, windows_qualification_ids } from './rust_qualification_catalog.mjs';
import { parse_libtest_output, parse_test_list, run_rust_test_check, validate_qualification_filters } from './rust_test_check.mjs';

const ordinary_id = 'services::ordinary_fixture::tests::passes';
const ignored_ordinary_id = 'services::ordinary_fixture::tests::remains_ignored';

function list_output(ids) {
  return `${ids.map((id) => `${id}: test`).join('\n')}\n${ids.length} tests, 0 benchmarks\n`;
}

function test_output(events, {
  passed = events.filter(({ result }) => result === 'ok').length,
  failed = events.filter(({ result }) => result === 'FAILED').length,
  ignored = events.filter(({ result }) => result === 'ignored').length,
  measured = 0,
  filtered_out = 0,
  result = failed === 0 ? 'ok' : 'FAILED',
} = {}) {
  const lines = events.map(({ id, result: outcome }) => `test ${id} ... ${outcome}`);
  lines.push(`test result: ${result}. ${passed} passed; ${failed} failed; ${ignored} ignored; ${measured} measured; ${filtered_out} filtered out; finished in 0.00s`);
  return `${lines.join('\n')}\n`;
}

function all_passed(ids = windows_qualification_ids) {
  return test_output(ids.map((id) => ({ id, result: 'ok' })), { passed: ids.length });
}

function synthetic_spawn(responses, calls = []) {
  return (executable, args, options) => {
    calls.push({ executable, args: [...args], options });
    const response = responses.shift() ?? { stdout: '', exit_code: 0 };
    const source = [
      `process.stdout.write(${JSON.stringify(response.stdout ?? '')});`,
      `process.stderr.write(${JSON.stringify(response.stderr ?? '')});`,
      `process.exit(${Number.isInteger(response.exit_code) ? response.exit_code : 0});`,
    ].join('');
    return spawn(process.execPath, ['-e', source], options);
  };
}

function ordinary_windows_responses() {
  const discovered = [...windows_qualification_ids, ordinary_id, ignored_ordinary_id];
  const events = [
    { id: ordinary_id, result: 'ok' },
    { id: ignored_ordinary_id, result: 'ignored' },
  ];
  return [
    { stdout: list_output(discovered), exit_code: 0 },
    { stdout: test_output(events, { passed: 1, ignored: 1, filtered_out: windows_qualification_ids.length }), exit_code: 0 },
  ];
}

test('qualified filters are unique and exactly the manifest ids', () => {
  assert.equal(validate_qualification_filters([...windows_qualification_ids]), null);
  assert.equal(validate_qualification_filters([...windows_qualification_ids, windows_qualification_ids[0]]), 'qualification_filter_duplicated');
  assert.equal(validate_qualification_filters(windows_qualification_ids.slice(1)), 'qualification_manifest_invalid');
});

test('qualified profile requests each exact test once and rejects ignored execution', async () => {
  const calls = [];
  const receipt = await run_rust_test_check('windows_qualification', {
    platform: 'win32',
    spawn_process: synthetic_spawn([{ stdout: all_passed(), exit_code: 0 }], calls),
    forward_output: () => {},
  });
  assert.equal(receipt.status, 'passed');
  assert.equal(receipt.passed_count, 26);
  assert.deepEqual(receipt.executed_test_ids, [...windows_qualification_ids]);
  assert.deepEqual(receipt.unrun_test_ids, []);
  assert.equal(calls.length, 1);
  const args = calls[0].args;
  const separator = args.indexOf('--');
  assert.deepEqual(args.slice(0, separator), ['test', '--locked', '--offline', '--lib', '--color', 'never']);
  assert.deepEqual(args.slice(separator + 1), ['--test-threads=1', '--exact', ...windows_qualification_ids]);
  assert.equal(args.includes('--ignored'), false);
  assert.equal(calls[0].options.shell, false);
  assert.equal(calls[0].options.detached, process.platform !== 'win32');
  assert.match(calls[0].options.cwd, /src-tauri$/);
});

test('qualified profile rejects zero, missing, duplicate, extra, ignored, and summary-less output', async (context) => {
  const duplicate_events = windows_qualification_ids.slice(0, -1).map((id) => ({ id, result: 'ok' }));
  duplicate_events.push({ id: windows_qualification_ids[0], result: 'ok' });
  const ignored_events = windows_qualification_ids.map((id, index) => ({ id, result: index === 0 ? 'ignored' : 'ok' }));
  const extra_events = [...windows_qualification_ids.map((id) => ({ id, result: 'ok' })), { id: 'tests::extra', result: 'ok' }];
  const cases = [
    ['zero tests', test_output([], { passed: 0 }), 'qualified_tests_missing'],
    ['missing test', test_output(windows_qualification_ids.slice(0, -1).map((id) => ({ id, result: 'ok' }))), 'qualified_tests_missing'],
    ['duplicate filter result', test_output(duplicate_events, { passed: 26 }), 'qualified_tests_missing'],
    ['ignored test', test_output(ignored_events, { passed: 25, ignored: 1 }), 'qualified_tests_ignored'],
    ['extra test', test_output(extra_events, { passed: 27 }), 'qualified_tests_extra'],
    ['missing summary', windows_qualification_ids.map((id) => `test ${id} ... ok`).join('\n'), 'libtest_summary_missing'],
  ];
  for (const [name, stdout, error_code] of cases) {
    await context.test(name, async () => {
      const receipt = await run_rust_test_check('windows_qualification', {
        platform: 'win32',
        spawn_process: synthetic_spawn([{ stdout, exit_code: 0 }]),
        forward_output: () => {},
      });
      assert.equal(receipt.status, 'failed');
      assert.equal(receipt.error_code, error_code);
    });
  }
});

test('qualified Cargo exit failures remain failures even with passing test lines', async () => {
  const receipt = await run_rust_test_check('windows_qualification', {
    platform: 'win32',
    spawn_process: synthetic_spawn([{ stdout: all_passed(), exit_code: 101 }]),
    forward_output: () => {},
  });
  assert.equal(receipt.status, 'failed');
  assert.equal(receipt.exit_code, 101);
});

test('ordinary Windows run uses only manifest skips and discovers nonzero retained tests', async () => {
  const calls = [];
  const receipt = await run_rust_test_check('ordinary', {
    platform: 'win32',
    spawn_process: synthetic_spawn(ordinary_windows_responses(), calls),
    forward_output: () => {},
  });
  assert.equal(receipt.status, 'passed');
  assert.equal(receipt.test_names_discovered, 28);
  assert.equal(receipt.test_names_selected, 2);
  assert.equal(receipt.counts.passed, 1);
  assert.equal(receipt.counts.ignored, 1);
  assert.deepEqual(receipt.excluded_test_ids, [...windows_qualification_ids]);
  assert.equal(calls.length, 2);
  const args = calls[1].args;
  const separator = args.indexOf('--');
  const filters = args.slice(separator + 1);
  assert.deepEqual(filters, ['--test-threads=1', ...windows_qualification_ids.flatMap((id) => ['--skip', id])]);
  assert.equal(args.includes('--ignored'), false);
  assert.equal(args.includes('--exact'), false);
  assert.equal(calls.every(({ options }) => options.shell === false), true);
});

test('Unix ordinary run retains the four symlink tests and excludes no manifest ids', async () => {
  const discovered = [...mirror_symlink_ids, ordinary_id];
  const events = discovered.map((id) => ({ id, result: 'ok' }));
  const calls = [];
  const receipt = await run_rust_test_check('ordinary', {
    platform: 'linux',
    spawn_process: synthetic_spawn([
      { stdout: list_output(discovered), exit_code: 0 },
      { stdout: test_output(events, { passed: discovered.length }), exit_code: 0 },
    ], calls),
    forward_output: () => {},
  });
  assert.equal(receipt.status, 'passed');
  assert.equal(receipt.excluded_test_ids.length, 0);
  assert.equal(receipt.test_names_selected, discovered.length);
  assert.equal(calls[1].args.includes('--skip'), false);
  assert.ok(mirror_symlink_ids.every((id) => calls[1].args.includes(id) === false));
});

test('ordinary zero-test and manifest discovery failures do not pass', async (context) => {
  await context.test('zero tests', async () => {
    const discovered = [...mirror_symlink_ids, ordinary_id];
    const receipt = await run_rust_test_check('ordinary', {
      platform: 'linux',
      spawn_process: synthetic_spawn([
        { stdout: list_output(discovered), exit_code: 0 },
        { stdout: test_output([], { passed: 0 }), exit_code: 0 },
      ]),
      forward_output: () => {},
    });
    assert.equal(receipt.status, 'failed');
    assert.equal(receipt.error_code, 'ordinary_zero_tests_executed');
  });
  await context.test('missing Unix mirror tests', async () => {
    let cargo_calls = 0;
    const receipt = await run_rust_test_check('ordinary', {
      platform: 'linux',
      spawn_process: synthetic_spawn([{ stdout: list_output([ordinary_id]), exit_code: 0 }], { get length() { return cargo_calls; }, push() { cargo_calls++; } }),
      forward_output: () => {},
    });
    assert.equal(receipt.status, 'failed');
    assert.equal(receipt.error_code, 'unix_mirror_test_discovery_mismatch');
    assert.equal(cargo_calls, 1);
  });
  await context.test('Windows exclusion missing', async () => {
    const calls = [];
    const receipt = await run_rust_test_check('ordinary', {
      platform: 'win32',
      spawn_process: synthetic_spawn([{ stdout: list_output([ordinary_id]), exit_code: 0 }], calls),
      forward_output: () => {},
    });
    assert.equal(receipt.status, 'failed');
    assert.equal(receipt.error_code, 'qualification_manifest_mismatch');
    assert.equal(calls.length, 1);
  });
});

test('ordinary Cargo failures preserve their actual exit code', async () => {
  const calls = [];
  const discovered = [...mirror_symlink_ids, ordinary_id];
  const receipt = await run_rust_test_check('ordinary', {
    platform: 'linux',
    spawn_process: synthetic_spawn([
      { stdout: list_output(discovered), exit_code: 0 },
      { stdout: test_output([{ id: ordinary_id, result: 'FAILED' }], { passed: 0, failed: 1, result: 'FAILED' }), exit_code: 101 },
    ], calls),
    forward_output: () => {},
  });
  assert.equal(receipt.status, 'failed');
  assert.equal(receipt.exit_code, 101);
});

test('pre-cancelled and interrupted helper runs never report success', async () => {
  const controller = new AbortController();
  controller.abort();
  const not_started = await run_rust_test_check('windows_qualification', {
    platform: 'win32', signal: controller.signal,
    spawn_process: () => { throw new Error('must not spawn'); },
    forward_output: () => {},
  });
  assert.equal(not_started.status, 'cancelled');
  assert.equal(not_started.executed_test_ids.length, 0);

  const active_controller = new AbortController();
  const timer = setTimeout(() => active_controller.abort(), 50);
  try {
    const interrupted = await run_rust_test_check('windows_qualification', {
      platform: 'win32', signal: active_controller.signal,
      spawn_process: (executable, args, options) => spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000)'], options),
      forward_output: () => {}, timeout_ms: 1000,
    });
    assert.equal(interrupted.status, 'cancelled');
  } finally {
    clearTimeout(timer);
  }
});

test('non-Windows qualification is not applicable and starts no Cargo child', async () => {
  let spawned = false;
  const receipt = await run_rust_test_check('windows_qualification', {
    platform: 'linux', spawn_process: () => { spawned = true; throw new Error('must not spawn'); },
  });
  assert.equal(receipt.status, 'not_applicable');
  assert.deepEqual(receipt.unrun_test_ids, [...windows_qualification_ids]);
  assert.equal(spawned, false);
});

test('ordinary --skip proof rejects similarly named tests so exclusion stays exact', async (context) => {
  await context.test('suffix-extended test blocks the profile before any test run', async () => {
    const calls = [];
    const discovered = [...windows_qualification_ids, `${windows_qualification_ids[0]}_fixture_extension`, ordinary_id];
    const receipt = await run_rust_test_check('ordinary', {
      platform: 'win32',
      spawn_process: synthetic_spawn([{ stdout: list_output(discovered), exit_code: 0 }], calls),
      forward_output: () => {},
    });
    assert.equal(receipt.status, 'failed');
    assert.equal(receipt.error_code, 'qualification_manifest_mismatch');
    assert.equal(calls.length, 1, 'no cargo test invocation may start after an unproven exclusion');
  });
  await context.test('prefix-wrapped test blocks the profile', async () => {
    const calls = [];
    const discovered = [...windows_qualification_ids, `z${windows_qualification_ids[0]}`, ordinary_id];
    const receipt = await run_rust_test_check('ordinary', {
      platform: 'win32',
      spawn_process: synthetic_spawn([{ stdout: list_output(discovered), exit_code: 0 }], calls),
      forward_output: () => {},
    });
    assert.equal(receipt.status, 'failed');
    assert.equal(receipt.error_code, 'qualification_manifest_mismatch');
    assert.equal(calls.length, 1);
  });
  await context.test('distinct same-module tests keep the partition exact and pass', async () => {
    const distinct = [
      ...windows_qualification_ids,
      ordinary_id,
      'services::mirror_sync::tests::other_top_level_link_checks',
    ];
    const events = [
      { id: ordinary_id, result: 'ok' },
      { id: 'services::mirror_sync::tests::other_top_level_link_checks', result: 'ok' },
    ];
    const receipt = await run_rust_test_check('ordinary', {
      platform: 'win32',
      spawn_process: synthetic_spawn([
        { stdout: list_output(distinct), exit_code: 0 },
        { stdout: test_output(events, { passed: 2, ignored: 0, filtered_out: windows_qualification_ids.length }), exit_code: 0 },
      ]),
      forward_output: () => {},
    });
    assert.equal(receipt.status, 'passed');
    assert.equal(receipt.test_names_selected, 2);
    assert.equal(receipt.counts.passed, 2);
  });
});

test('libtest output is complete only after child pipes close', async () => {
  const ids = [...mirror_symlink_ids, ordinary_id];
  const responses = [list_output(ids), test_output(ids.map((id) => ({ id, result: 'ok' })), { passed: ids.length })];
  const spawn_process = () => {
    const child = new EventEmitter();
    child.stdout = new EventEmitter();
    child.stderr = new EventEmitter();
    child.unref = () => {};
    const output = responses.shift();
    setImmediate(() => {
      child.emit('exit', 0, null);
      setTimeout(() => {
        child.stdout.emit('data', Buffer.from(output));
        child.emit('close', 0, null);
      }, 10);
    });
    return child;
  };
  const receipt = await run_rust_test_check('ordinary', {
    platform: 'linux', spawn_process, forward_output: () => {},
  });
  assert.equal(receipt.status, 'passed');
  assert.equal(receipt.counts.passed, ids.length);
});

test('libtest parsing counts exact events and list discovery ignores benchmark summary rows', () => {
  const output = test_output([{ id: ordinary_id, result: 'ok' }], { passed: 1 })
    + `test ${ignored_ordinary_id} ... ignored, explicit authorization required\n`;
  assert.deepEqual(parse_libtest_output(output), {
    events: [{ id: ordinary_id, result: 'ok' }, { id: ignored_ordinary_id, result: 'ignored' }],
    summary: { result: 'ok', passed: 1, failed: 0, ignored: 0, measured: 0, filtered_out: 0 },
  });
  assert.deepEqual(parse_test_list(`${list_output([ordinary_id])}1 benchmark, 0 tests`), [ordinary_id]);
});
