import { spawn } from 'node:child_process';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { writeFile } from 'node:fs/promises';
import { stop_owned_child } from './runner.mjs';
import { mirror_symlink_ids, ordinary_skip_ids, windows_qualification_ids } from './rust_qualification_catalog.mjs';

const repository_root = fileURLToPath(new URL('../../', import.meta.url));
const maximum_output_bytes = 4 * 1024 * 1024;
const command_timeout_ms = 14 * 60 * 1000;
const cleanup_timeout_ms = 10_000;
const test_line = /^test ([^\s]+) \.\.\. (ok|FAILED|ignored)(?:,[^\r\n]*)?$/gm;
const summary_line = /^test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored; (\d+) measured; (\d+) filtered out; finished in ([\d.]+)(?:s|ms)$/gm;

function append_bounded(chunks, bytes, chunk, output_limit_bytes) {
  const available = Math.max(0, output_limit_bytes - bytes);
  if (available > 0) chunks.push(chunk.subarray(0, available));
  return { bytes: bytes + Math.min(available, chunk.length), truncated: chunk.length > available };
}

function parse_summary(output) {
  const summaries = [...output.matchAll(summary_line)];
  if (summaries.length !== 1) return null;
  const row = summaries[0];
  return {
    result: row[1],
    passed: Number(row[2]),
    failed: Number(row[3]),
    ignored: Number(row[4]),
    measured: Number(row[5]),
    filtered_out: Number(row[6]),
  };
}

export function parse_libtest_output(output) {
  const events = [...output.matchAll(test_line)].map((row) => ({ id: row[1], result: row[2] }));
  return { events, summary: parse_summary(output) };
}

export function parse_test_list(output) {
  const ids = [];
  for (const line of output.split(/\r?\n/)) {
    const match = /^([^\s]+): test$/.exec(line.trim());
    if (match) ids.push(match[1]);
  }
  return ids;
}

function duplicate_values(values) {
  const seen = new Set();
  const duplicates = new Set();
  for (const value of values) {
    if (seen.has(value)) duplicates.add(value);
    seen.add(value);
  }
  return [...duplicates];
}

function exact_id_set_matches(actual, expected) {
  return actual.length === expected.length
    && duplicate_values(actual).length === 0
    && actual.every((id, index) => id === expected[index]);
}

export function validate_qualification_filters(requested_ids) {
  if (!Array.isArray(requested_ids) || duplicate_values(requested_ids).length > 0) return 'qualification_filter_duplicated';
  return exact_id_set_matches(requested_ids, windows_qualification_ids) ? null : 'qualification_manifest_invalid';
}

async function wait_bounded_exit(exit, timeout_ms) {
  let timer;
  try {
    return await Promise.race([
      exit,
      new Promise((resolve_wait) => { timer = setTimeout(() => resolve_wait(null), timeout_ms); }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

async function run_process(executable, args, {
  cwd, env = process.env, signal, spawn_process = spawn,
  timeout_ms = command_timeout_ms, output_limit_bytes = maximum_output_bytes,
} = {}) {
  if (signal?.aborted) return { status: 'cancelled', exit_code: null, output: '', output_truncated: false };
  let child;
  try {
    child = spawn_process(executable, args, {
      cwd, env, shell: false, windowsHide: true, detached: process.platform !== 'win32',
      stdio: ['ignore', 'pipe', 'pipe'],
    });
  } catch {
    return { status: 'unavailable', exit_code: null, output: '', output_truncated: false };
  }

  const chunks = [];
  let byte_count = 0;
  let output_truncated = false;
  const collect = (chunk) => {
    const captured = append_bounded(chunks, byte_count, chunk, output_limit_bytes);
    byte_count = captured.bytes;
    output_truncated ||= captured.truncated;
  };
  child.stdout?.on('data', collect);
  child.stderr?.on('data', collect);

  const completed = new Promise((resolve_completion) => {
    let settled = false;
    const settle = (result) => {
      if (settled) return;
      settled = true;
      resolve_completion(result);
    };
    child.once('error', () => settle({ status: 'unavailable', exit_code: null }));
    // Close, not exit: libtest output is complete only after both pipes close.
    child.once('close', (code, child_signal) => settle({
      status: code === 0 && child_signal === null ? 'passed' : 'failed',
      exit_code: code,
      signal: child_signal,
    }));
  });

  let timer;
  let abort_handler;
  const stop_reason = new Promise((resolve_stop) => {
    timer = setTimeout(() => resolve_stop('timed_out'), timeout_ms);
    abort_handler = () => resolve_stop('cancelled');
    signal?.addEventListener('abort', abort_handler, { once: true });
    if (signal?.aborted) abort_handler();
  });
  const first = await Promise.race([completed, stop_reason]);
  clearTimeout(timer);
  signal?.removeEventListener('abort', abort_handler);

  let result = first;
  if (typeof first === 'string') {
    let cleanup_failed = false;
    try { await stop_owned_child(child); }
    catch { cleanup_failed = true; }
    const stopped = await wait_bounded_exit(completed, cleanup_timeout_ms);
    result = stopped && !cleanup_failed
      ? { ...stopped, status: first }
      : { status: 'cleanup_failed', exit_code: null, signal: null };
    if (result.status === 'cleanup_failed') {
      child.unref();
      child.stdout?.destroy();
      child.stderr?.destroy();
    }
  }

  const output = Buffer.concat(chunks).toString('utf8');
  return { ...result, output, output_truncated };
}

function cargo_args(list_args = []) {
  return ['test', '--locked', '--offline', '--lib', '--color', 'never', '--', '--test-threads=1', ...list_args];
}

function safe_command_result(result, forward = (text) => process.stderr.write(text)) {
  if (result.output) forward(result.output);
  return {
    status: result.status,
    exit_code: result.exit_code,
    output: result.output,
    output_truncated: result.output_truncated,
  };
}

function parse_test_receipt(profile, fields) {
  return {
    schema_version: 1,
    profile,
    evidence_class: 'local-only',
    ...fields,
  };
}

async function run_cargo(root, env, args, options) {
  const result = await run_process(options.cargo_executable ?? 'cargo', args, {
    cwd: join(root, 'src-tauri'),
    env,
    signal: options.signal,
    spawn_process: options.spawn_process,
    timeout_ms: options.timeout_ms,
    output_limit_bytes: options.output_limit_bytes,
  });
  safe_command_result(result, options.forward_output);
  return result;
}

// libtest --skip is a substring filter and ignores --exact, so the dropped
// set cannot be proven from argv alone. Prove against discovery that the
// substring drop set equals the manifest exactly before trusting --skip.
function libtest_skip_set(ids, skips) {
  return ids.filter((id) => skips.some((skip) => id.includes(skip)));
}

function list_validation(ids, skips, platform) {
  if (ids.length === 0 || duplicate_values(ids).length > 0) return 'test_name_discovery_invalid';
  const dropped = libtest_skip_set(ids, skips);
  if (dropped.length !== skips.length || !skips.every((skip) => dropped.includes(skip))) {
    return 'qualification_manifest_mismatch';
  }
  if (platform !== 'win32' && mirror_symlink_ids.some((id) => ids.filter((found) => found === id).length !== 1)) {
    return 'unix_mirror_test_discovery_mismatch';
  }
  return null;
}

function ordinary_result_validation({ parsed, discovered_ids, excluded_ids }) {
  if (!parsed.summary) return { error_code: 'libtest_summary_missing' };
  const events = parsed.events;
  const event_ids = events.map(({ id }) => id);
  const duplicates = duplicate_values(event_ids);
  const discovered = new Set(discovered_ids);
  const excluded = new Set(excluded_ids);
  const extra = event_ids.filter((id) => !discovered.has(id));
  const excluded_events = event_ids.filter((id) => excluded.has(id));
  const counts = {
    passed: events.filter(({ result }) => result === 'ok').length,
    failed: events.filter(({ result }) => result === 'FAILED').length,
    ignored: events.filter(({ result }) => result === 'ignored').length,
  };
  const expected_selection_count = discovered_ids.length - excluded_ids.length;
  const reported_selection_count = parsed.summary.passed + parsed.summary.failed
    + parsed.summary.ignored + parsed.summary.measured + parsed.summary.filtered_out;
  if (duplicates.length || extra.length || excluded_events.length) return { error_code: 'ordinary_test_selection_mismatch', counts };
  if (parsed.summary.result !== 'ok' || parsed.summary.failed !== 0 || counts.failed !== 0) {
    return { error_code: 'cargo_test_failed', counts };
  }
  if (parsed.summary.passed === 0 || counts.passed === 0) return { error_code: 'ordinary_zero_tests_executed', counts };
  if (counts.passed !== parsed.summary.passed || counts.ignored !== parsed.summary.ignored
      || parsed.summary.filtered_out !== excluded_ids.length
      || counts.passed + counts.failed + counts.ignored + parsed.summary.measured !== expected_selection_count
      || reported_selection_count !== discovered_ids.length || expected_selection_count <= 0) {
    return { error_code: 'ordinary_test_count_mismatch', counts };
  }
  return { counts, test_names_discovered: discovered_ids.length, test_names_selected: expected_selection_count };
}

function qualified_result_validation(parsed, requested_ids) {
  const events = parsed.events;
  const grouped = new Map();
  for (const event of events) grouped.set(event.id, [...(grouped.get(event.id) ?? []), event]);
  const executed_test_ids = events.map(({ id }) => id).filter((id) => requested_ids.includes(id));
  const missing_test_ids = requested_ids.filter((id) => (grouped.get(id) ?? []).length === 0);
  const duplicate_test_ids = requested_ids.filter((id) => (grouped.get(id) ?? []).length > 1);
  const extra_test_ids = events.filter(({ id }) => !requested_ids.includes(id)).map(({ id }) => id);
  const failed_test_ids = events.filter(({ result }) => result === 'FAILED').map(({ id }) => id);
  const ignored_test_ids = events.filter(({ result }) => result === 'ignored').map(({ id }) => id);
  const parsed_ok = parsed.summary
    && parsed.summary.result === 'ok'
    && parsed.summary.passed === requested_ids.length
    && parsed.summary.failed === 0
    && parsed.summary.ignored === 0
    && parsed.summary.measured === 0
    && events.length === requested_ids.length
    && events.every(({ result }) => result === 'ok');
  const errors = [];
  if (missing_test_ids.length) errors.push('qualified_tests_missing');
  if (duplicate_test_ids.length) errors.push('qualified_tests_duplicated');
  if (extra_test_ids.length) errors.push('qualified_tests_extra');
  if (ignored_test_ids.length) errors.push('qualified_tests_ignored');
  if (failed_test_ids.length || (parsed.summary && parsed.summary.failed > 0)) errors.push('qualified_tests_failed');
  if (!parsed.summary) errors.push('libtest_summary_missing');
  else if (!parsed_ok && errors.length === 0) errors.push('qualified_test_count_mismatch');
  return {
    errors,
    executed_test_ids,
    missing_test_ids,
    duplicate_test_ids,
    extra_test_ids,
    failed_test_ids,
    ignored_test_ids,
    passed_count: events.filter(({ result }) => result === 'ok').length,
    summary: parsed.summary,
  };
}

async function write_receipt_file(receipt, env) {
  const receipt_path = env.AAG_RUST_TEST_RECEIPT_PATH;
  if (!receipt_path) return null;
  const config_dir = env.AAG_CONFIG_DIR;
  if (!config_dir || dirname(resolve(receipt_path)) !== resolve(config_dir)) return 'receipt_path_invalid';
  try {
    await writeFile(receipt_path, `${JSON.stringify(receipt)}\n`, { encoding: 'utf8', mode: 0o600, flag: 'wx' });
    return null;
  } catch {
    return 'receipt_write_failed';
  }
}

export async function run_rust_test_check(profile, {
  root = repository_root,
  platform = process.platform,
  env = process.env,
  signal,
  cargo_executable = 'cargo',
  spawn_process = spawn,
  forward_output = (text) => process.stderr.write(text),
  timeout_ms = command_timeout_ms,
  output_limit_bytes = maximum_output_bytes,
} = {}) {
  if (!['ordinary', 'windows_qualification'].includes(profile)) {
    return parse_test_receipt(profile, { status: 'failed', error_code: 'invalid_profile' });
  }
  if (profile === 'windows_qualification' && platform !== 'win32') {
    return parse_test_receipt(profile, {
      status: 'not_applicable',
      requested_test_ids: [...windows_qualification_ids],
      executed_test_ids: [],
      unrun_test_ids: [...windows_qualification_ids],
      error_code: 'windows_only_profile',
    });
  }

  const excluded_ids = profile === 'ordinary' ? ordinary_skip_ids(platform) : [];
  const requested_ids = profile === 'windows_qualification' ? [...windows_qualification_ids] : [];
  const base = {
    requested_test_ids: requested_ids,
    excluded_test_ids: excluded_ids,
    executed_test_ids: [],
    unrun_test_ids: requested_ids,
  };
  const options = { cargo_executable, spawn_process, forward_output, signal, timeout_ms, output_limit_bytes };

  if (profile === 'ordinary') {
    const listing = await run_cargo(root, env, cargo_args(['--list']), options);
    if (listing.status !== 'passed' || listing.output_truncated) {
      return parse_test_receipt(profile, {
        ...base,
        status: listing.status === 'cancelled' || listing.status === 'timed_out' ? listing.status : 'failed',
        error_code: listing.output_truncated ? 'cargo_list_output_limit' : 'cargo_list_failed',
        exit_code: listing.exit_code,
        cargo_signal: listing.signal ?? null,
      });
    }
    const discovered_ids = parse_test_list(listing.output);
    const validation_error = list_validation(discovered_ids, excluded_ids, platform);
    if (validation_error) return parse_test_receipt(profile, { ...base, status: 'failed', error_code: validation_error });
    const skip_args = excluded_ids.flatMap((id) => ['--skip', id]);
    const result = await run_cargo(root, env, cargo_args(skip_args), options);
    const parsed = parse_libtest_output(result.output);
    const validation = ordinary_result_validation({ parsed, discovered_ids, excluded_ids });
    const failed = result.status !== 'passed' || result.output_truncated || Boolean(validation.error_code);
    return parse_test_receipt(profile, {
      ...base,
      status: failed ? result.status === 'cancelled' || result.status === 'timed_out' || result.status === 'cleanup_failed' ? result.status : 'failed' : 'passed',
      ...(failed ? { error_code: result.output_truncated ? 'cargo_test_output_limit' : validation.error_code ?? 'cargo_test_failed' } : {}),
      exit_code: result.exit_code,
      cargo_signal: result.signal ?? null,
      ...validation,
      test_names_discovered: discovered_ids.length,
      excluded_test_ids: excluded_ids,
    });
  }

  const filter_error = validate_qualification_filters(requested_ids);
  if (filter_error) {
    return parse_test_receipt(profile, { ...base, status: 'failed', error_code: filter_error });
  }
  const result = await run_cargo(root, env, cargo_args(['--exact', ...requested_ids]), options);
  const parsed = parse_libtest_output(result.output);
  const validation = qualified_result_validation(parsed, requested_ids);
  const failed = result.status !== 'passed' || result.output_truncated || validation.errors.length > 0;
  const executed = new Set(validation.executed_test_ids);
  const receipt = parse_test_receipt(profile, {
    ...base,
    status: failed ? result.status === 'cancelled' || result.status === 'timed_out' || result.status === 'cleanup_failed' ? result.status : 'failed' : 'passed',
    ...(failed ? { error_code: result.output_truncated ? 'cargo_test_output_limit' : validation.errors[0] ?? 'cargo_test_failed' } : {}),
    exit_code: result.exit_code,
    cargo_signal: result.signal ?? null,
    executed_test_ids: validation.executed_test_ids,
    unrun_test_ids: requested_ids.filter((id) => !executed.has(id)),
    missing_test_ids: validation.missing_test_ids,
    duplicate_test_ids: validation.duplicate_test_ids,
    extra_test_ids: validation.extra_test_ids,
    failed_test_ids: validation.failed_test_ids,
    ignored_test_ids: validation.ignored_test_ids,
    passed_count: validation.passed_count,
  });
  return receipt;
}

async function persist_and_print(receipt) {
  const write_error = await write_receipt_file(receipt, process.env);
  const final_receipt = write_error
    ? { ...receipt, status: 'failed', error_code: write_error }
    : receipt;
  process.stderr.write(`AAG_RUST_TEST_RECEIPT ${JSON.stringify(final_receipt)}\n`);
  process.exitCode = final_receipt.status === 'passed'
    ? 0
    : Number.isInteger(final_receipt.exit_code) && final_receipt.exit_code > 0
      ? Math.min(final_receipt.exit_code, 255)
      : 1;
}

async function main() {
  const [profile, ...extra] = process.argv.slice(2);
  if (!['ordinary', 'windows_qualification'].includes(profile) || extra.length > 0) {
    process.stderr.write('AAG_RUST_TEST_RECEIPT {"schema_version":1,"status":"failed","error_code":"invalid_arguments","evidence_class":"local-only"}\n');
    process.exitCode = 2;
    return;
  }
  await persist_and_print(await run_rust_test_check(profile));
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  await main();
}
