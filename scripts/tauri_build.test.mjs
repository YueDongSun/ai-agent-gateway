import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import test from 'node:test';
import { fileURLToPath, pathToFileURL } from 'node:url';

const repository_root = fileURLToPath(new URL('../', import.meta.url));
const package_json = JSON.parse(readFileSync(join(repository_root, 'package.json'), 'utf8'));
const fixture_timeout_ms = 30_000;
const cancellation_fixture_limits = { child_lifetime_ms: 3_000, polling_interval_ms: 10, readiness_timeout_ms: 5_000 };
const fixture_output_marker = 'aag_build_fixture:';
const npm_entry = [
  process.env.npm_execpath,
  join(dirname(process.execPath), 'node_modules/npm/bin/npm-cli.js'),
  resolve(dirname(process.execPath), '../lib/node_modules/npm/bin/npm-cli.js'),
  process.env.APPDATA && join(process.env.APPDATA, 'npm/node_modules/npm/bin/npm-cli.js'),
].find((candidate) => candidate && existsSync(candidate));

function create_build_fixture(test_context, exit_code = 0) {
  const fixture_root = mkdtempSync(join(tmpdir(), 'aag_build_fixture_'));
  test_context.after(() => {
    rmSync(fixture_root, { recursive: true, force: true });
    assert.equal(existsSync(fixture_root), false);
  });
  const cli_path = join(fixture_root, 'node_modules/@tauri-apps/cli/tauri.js');
  const bin_directory = join(fixture_root, 'node_modules/.bin');
  mkdirSync(dirname(cli_path), { recursive: true });
  mkdirSync(bin_directory, { recursive: true });
  mkdirSync(join(fixture_root, 'scripts'));
  mkdirSync(join(fixture_root, 'config'));
  mkdirSync(join(fixture_root, 'home'));
  writeFileSync(join(fixture_root, 'package.json'), JSON.stringify({
    private: true,
    type: 'module',
    scripts: {
      'desktop:build': package_json.scripts['desktop:build'],
      'tauri:build': package_json.scripts['tauri:build'],
    },
  }));
  writeFileSync(cli_path, `process.stdout.write(${JSON.stringify(fixture_output_marker)} + JSON.stringify({ arguments: process.argv.slice(2), cwd: process.cwd() }) + '\\n'); process.exitCode = ${exit_code};\n`);
  for (const file_name of ['tauri_build.mjs', 'tauri_build_config.mjs']) {
    const source_path = join(repository_root, 'scripts', file_name);
    if (existsSync(source_path)) copyFileSync(source_path, join(fixture_root, 'scripts', file_name));
  }
  if (process.platform === 'win32') {
    writeFileSync(join(bin_directory, 'tauri.cmd'), `@"${process.execPath}" "${cli_path}" %*\r\n`);
  } else {
    const quote_path = (value) => `'${value.replaceAll("'", "'\"'\"'")}'`;
    writeFileSync(join(bin_directory, 'tauri'), `#!/bin/sh\nexec ${quote_path(process.execPath)} ${quote_path(cli_path)} "$@"\n`, { mode: 0o700 });
  }
  const env = {
    ...process.env,
    AAG_CONFIG_DIR: join(fixture_root, 'config'),
    AAG_HOME_DIR: join(fixture_root, 'home'),
    npm_config_update_notifier: 'false',
  };
  delete env.AAG_TEST_CCSWITCH_DB;
  return { root: fixture_root, env };
}

function run_build_fixture(fixture, cli_arguments, { script_name = 'desktop:build', working_directory = fixture.root, runtime_arguments = [] } = {}) {
  if (script_name) assert.ok(npm_entry, 'The npm CLI is required to test the public build scripts');
  const entry_arguments = script_name
    ? [npm_entry, 'run', script_name, '--']
    : [join(fixture.root, 'scripts/tauri_build.mjs')];
  const result = spawnSync(process.execPath, [...runtime_arguments, ...entry_arguments, ...cli_arguments], {
    cwd: working_directory,
    env: fixture.env,
    encoding: 'utf8',
    timeout: fixture_timeout_ms,
    maxBuffer: 64 * 1024,
  });
  assert.ifError(result.error);
  const receipt_line = result.stdout.split(/\r?\n/).find((line) => line.startsWith(fixture_output_marker));
  assert.ok(receipt_line || result.status !== 0, 'The successful build fixture must emit its receipt');
  return { ...result, receipt: receipt_line ? JSON.parse(receipt_line.slice(fixture_output_marker.length)) : null };
}

for (const script_name of ['desktop:build', 'tauri:build']) {
  test(`${script_name} forwards Tauri options before the locked Cargo boundary`, (test_context) => {
    const fixture = create_build_fixture(test_context);
    const tauri_arguments = ['--target', 'x86_64-apple-darwin', '--bundles', 'nsis', '--ci'];
    const result = run_build_fixture(fixture, tauri_arguments, { script_name });
    assert.equal(result.status, 0);
    const cli_arguments = result.receipt.arguments;
    const separator_index = cli_arguments.indexOf('--');
    assert.ok(separator_index > cli_arguments.indexOf('--target'), 'Tauri --target must precede the Cargo separator');
    assert.ok(separator_index > cli_arguments.indexOf('--bundles'), 'Tauri --bundles must precede the Cargo separator');
    assert.ok(separator_index > cli_arguments.indexOf('--ci'), 'Tauri --ci must precede the Cargo separator');
    assert.deepEqual(cli_arguments.slice(3, separator_index), tauri_arguments);
    assert.deepEqual(cli_arguments.slice(separator_index + 1), ['--locked']);
  });

  test(`${script_name} retains the local configuration and locked default`, (test_context) => {
    const fixture = create_build_fixture(test_context);
    const result = run_build_fixture(fixture, [], { script_name });
    assert.equal(result.status, 0);
    assert.deepEqual(result.receipt.arguments, ['build', '--config', join(fixture.root, 'src-tauri/tauri.conf.json'), '--', '--locked']);
    assert.equal(resolve(result.receipt.cwd), resolve(fixture.root));
  });

  test(`${script_name} keeps explicit Cargo arguments behind the runner boundary`, (test_context) => {
    const fixture = create_build_fixture(test_context);
    const result = run_build_fixture(fixture, ['--target', 'aarch64-apple-darwin', '--', '--offline', '--jobs', '2'], { script_name });
    assert.equal(result.status, 0);
    assert.deepEqual(result.receipt.arguments.slice(3), ['--target', 'aarch64-apple-darwin', '--', '--offline', '--jobs', '2', '--locked']);
  });

  test(`${script_name} accepts runner-only arguments without duplicating --locked`, (test_context) => {
    const fixture = create_build_fixture(test_context);
    const result = run_build_fixture(fixture, ['--', '--offline', '--locked'], { script_name });
    assert.equal(result.status, 0);
    assert.deepEqual(result.receipt.arguments.slice(3), ['--', '--offline', '--locked']);
  });

  test(`${script_name} retains locking before Cargo's own option terminator`, (test_context) => {
    const fixture = create_build_fixture(test_context);
    const result = run_build_fixture(fixture, ['--', '--offline', '--'], { script_name });
    assert.equal(result.status, 0);
    assert.deepEqual(result.receipt.arguments.slice(3), ['--', '--offline', '--locked', '--']);
  });

  test(`${script_name} propagates a failing CLI exit code`, (test_context) => {
    const fixture = create_build_fixture(test_context, 23);
    const result = run_build_fixture(fixture, ['--ci'], { script_name });
    assert.ok(result.receipt);
    assert.equal(result.status, 23);
  });
}

test('the build entrypoint anchors its root and passes literal arguments without a shell', (test_context) => {
  const fixture = create_build_fixture(test_context);
  const working_directory = join(fixture.root, 'unrelated_directory');
  const external_config = 'fixture directory/literal&value%with$characters.json';
  mkdirSync(working_directory);
  const result = run_build_fixture(fixture, ['--config', external_config], { script_name: null, working_directory });
  assert.equal(result.status, 0);
  assert.equal(resolve(result.receipt.cwd), resolve(fixture.root));
  assert.deepEqual(result.receipt.arguments.slice(3), ['--config', external_config, '--', '--locked']);
});

for (const [signal, expected_exit_code] of [['SIGINT', 130], ['SIGTERM', 143]]) {
  test(`the build launcher handles fixture-delivered ${signal} and waits for its child`, (test_context) => {
    const fixture = create_build_fixture(test_context);
    const ready_path = join(fixture.root, 'child_ready.json');
    const pending_path = join(fixture.root, 'child_ready_pending.json');
    const safety_exit_path = join(fixture.root, 'child_safety_exit.json');
    const preload_path = join(fixture.root, 'cancel_launcher.mjs');
    writeFileSync(join(fixture.root, 'node_modules/@tauri-apps/cli/tauri.js'), `
      import { renameSync, writeFileSync } from 'node:fs';
      process.on('SIGINT', () => process.exit(0));
      process.on('SIGTERM', () => process.exit(0));
      process.stdout.write(${JSON.stringify(fixture_output_marker)} + JSON.stringify({ arguments: process.argv.slice(2), cwd: process.cwd() }) + '\\n');
      writeFileSync(${JSON.stringify(pending_path)}, JSON.stringify({ pid: process.pid }));
      renameSync(${JSON.stringify(pending_path)}, ${JSON.stringify(ready_path)});
      setTimeout(() => writeFileSync(${JSON.stringify(safety_exit_path)}, 'expired'), ${cancellation_fixture_limits.child_lifetime_ms});
    `);
    writeFileSync(preload_path, `
      import { existsSync } from 'node:fs';
      const polling_timer = setInterval(() => {
        if (!existsSync(${JSON.stringify(ready_path)})) return;
        clearInterval(polling_timer);
        clearTimeout(readiness_timer);
        process.emit(${JSON.stringify(signal)});
      }, ${cancellation_fixture_limits.polling_interval_ms});
      const readiness_timer = setTimeout(() => {
        clearInterval(polling_timer);
        process.exitCode = 1;
      }, ${cancellation_fixture_limits.readiness_timeout_ms});
    `);
    const result = run_build_fixture(fixture, [], { script_name: null, runtime_arguments: ['--import', pathToFileURL(preload_path).href] });
    assert.ok(result.receipt, 'The cancellation fixture CLI must start before the cancellation check');
    assert.equal(result.status, expected_exit_code, 'Cancellation must not report a successful build');
    assert.equal(existsSync(safety_exit_path), false, 'Cancellation must stop the child before its safety deadline');
    const child_pid = JSON.parse(readFileSync(ready_path, 'utf8')).pid;
    assert.throws(() => process.kill(child_pid, 0), { code: 'ESRCH' });
  });
}

test('cancellation during child creation is retained until the child can be stopped', (test_context) => {
  const fixture = create_build_fixture(test_context);
  const child_path = join(fixture.root, 'spawned_child.json');
  const safety_exit_path = join(fixture.root, 'child_safety_exit.json');
  const preload_path = join(fixture.root, 'cancel_on_spawn.mjs');
  writeFileSync(join(fixture.root, 'node_modules/@tauri-apps/cli/tauri.js'), `
    import { writeFileSync } from 'node:fs';
    process.stdout.write(${JSON.stringify(fixture_output_marker)} + JSON.stringify({ arguments: process.argv.slice(2), cwd: process.cwd() }) + '\\n');
    setTimeout(() => writeFileSync(${JSON.stringify(safety_exit_path)}, 'expired'), ${cancellation_fixture_limits.child_lifetime_ms});
  `);
  writeFileSync(preload_path, `
    import child_process from 'node:child_process';
    import { writeFileSync } from 'node:fs';
    import { syncBuiltinESMExports } from 'node:module';
    const original_spawn = child_process.spawn;
    child_process.spawn = (...spawn_arguments) => {
      const child = original_spawn(...spawn_arguments);
      writeFileSync(${JSON.stringify(child_path)}, JSON.stringify({ pid: child.pid }));
      process.emit('SIGTERM');
      return child;
    };
    syncBuiltinESMExports();
  `);
  const result = run_build_fixture(fixture, [], { script_name: null, runtime_arguments: ['--import', pathToFileURL(preload_path).href] });
  assert.equal(result.status, 143, 'Startup cancellation must be retained before spawn returns');
  assert.equal(existsSync(safety_exit_path), false);
  const child_pid = JSON.parse(readFileSync(child_path, 'utf8')).pid;
  assert.throws(() => process.kill(child_pid, 0), { code: 'ESRCH' });
});

test('a missing installed CLI fails rather than reporting a successful build', (test_context) => {
  const fixture = create_build_fixture(test_context);
  rmSync(join(fixture.root, 'node_modules/@tauri-apps/cli/tauri.js'));
  const result = run_build_fixture(fixture, []);
  assert.equal(result.status, 1);
  assert.equal(result.receipt, null);
});
