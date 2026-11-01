import assert from 'node:assert/strict';
import { test } from 'node:test';
import { dirname, isAbsolute, join, resolve } from 'node:path';
import { find_npm_cli } from './npm_discovery.mjs';

const cli_segments = ['node_modules', 'npm', 'bin', 'npm-cli.js'];

function node_adjacent_cli(node_executable) {
  return join(dirname(node_executable), ...cli_segments);
}

function windows_system_cli(program_files = 'C:/Program Files') {
  return join(program_files, 'nodejs', ...cli_segments);
}

function posix_sibling_cli(node_executable) {
  return join(dirname(node_executable), '..', 'lib', ...cli_segments);
}

test('prefers explicit, adjacent, Windows system, then sibling npm entries', () => {
  const node_executable = resolve('npm_discovery_fixture', 'bin', 'node.exe');
  const explicit_cli = resolve('npm_discovery_fixture', 'npm.cjs');
  const adjacent_cli = node_adjacent_cli(node_executable);
  const system_cli = windows_system_cli(resolve('npm_discovery_fixture', 'program_files'));
  const sibling_cli = posix_sibling_cli(node_executable);
  const all_candidates = new Set([explicit_cli, adjacent_cli, system_cli, sibling_cli]);
  const checked = [];
  const options = {
    env: { npm_execpath: explicit_cli, ProgramFiles: resolve('npm_discovery_fixture', 'program_files') },
    node_executable,
    platform: 'win32',
    path_exists: (candidate) => { checked.push(candidate); return all_candidates.has(candidate); },
  };

  assert.equal(find_npm_cli(options), explicit_cli);
  assert.deepEqual(checked, [explicit_cli]);

  checked.length = 0;
  assert.equal(find_npm_cli({ ...options, env: { ProgramFiles: options.env.ProgramFiles } }), adjacent_cli);
  assert.deepEqual(checked, [adjacent_cli]);

  checked.length = 0;
  const system_and_sibling = new Set([system_cli, sibling_cli]);
  assert.equal(find_npm_cli({
    ...options,
    env: { ProgramFiles: options.env.ProgramFiles },
    path_exists: (candidate) => { checked.push(candidate); return system_and_sibling.has(candidate); },
  }), system_cli);
  assert.deepEqual(checked, [adjacent_cli, system_cli]);
});

test('checks the Windows system npm location with Program Files fallback', () => {
  const node_executable = resolve('npm_discovery_fixture', 'node.exe');
  const system_cli = resolve(windows_system_cli());
  const checked = [];
  const found = find_npm_cli({
    env: {},
    node_executable,
    platform: 'win32',
    path_exists: (candidate) => { checked.push(candidate); return candidate === system_cli; },
  });

  assert.equal(found, resolve(system_cli));
  assert.deepEqual(checked, [node_adjacent_cli(node_executable), resolve(system_cli)]);
});

test('accepts explicit script entries and returns absolute normalized paths', () => {
  for (const explicit_cli of [
    './npm_discovery_fixture/npm.cjs',
    './npm_discovery_fixture/npm.mjs',
    './npm_discovery_fixture/npm_entry',
  ]) {
    const expected_cli = resolve(explicit_cli);
    const checked = [];
    const found = find_npm_cli({
      env: { npm_execpath: explicit_cli },
      node_executable: resolve('npm_discovery_fixture', 'node.exe'),
      platform: 'linux',
      path_exists: (candidate) => { checked.push(candidate); return candidate === expected_cli; },
    });

    assert.equal(found, expected_cli);
    assert.equal(isAbsolute(found), true);
    assert.deepEqual(checked, [expected_cli]);
  }
});

test('ignores invalid explicit entries and returns null when no script exists', () => {
  const node_executable = resolve('npm_discovery_fixture', 'node.exe');
  const adjacent_cli = node_adjacent_cli(node_executable);
  for (const npm_execpath of [null, 42, {}, '']) {
    const checked = [];
    assert.equal(find_npm_cli({
      env: { npm_execpath },
      node_executable,
      platform: 'linux',
      path_exists: (candidate) => { checked.push(candidate); return true; },
    }), adjacent_cli);
    assert.deepEqual(checked, [adjacent_cli]);
  }

  const missing_cli = resolve('npm_discovery_fixture', 'missing.mjs');
  const checked = [];
  assert.equal(find_npm_cli({
    env: { npm_execpath: missing_cli, ProgramFiles: 'C:/Program Files' },
    node_executable,
    platform: 'linux',
    path_exists: (candidate) => { checked.push(candidate); return false; },
  }), null);
  assert.deepEqual(checked, [missing_cli, adjacent_cli, posix_sibling_cli(node_executable)]);
});

test('uses the POSIX sibling prefix and never checks the Windows system location', () => {
  const node_executable = resolve('npm_discovery_fixture', 'bin', 'node');
  const sibling_cli = posix_sibling_cli(node_executable);
  const windows_cli = windows_system_cli('C:/Program Files');
  const checked = [];
  const found = find_npm_cli({
    env: { ProgramFiles: 'C:/Program Files' },
    node_executable,
    platform: 'linux',
    path_exists: (candidate) => { checked.push(candidate); return candidate === sibling_cli; },
  });

  assert.equal(found, resolve(sibling_cli));
  assert.deepEqual(checked, [node_adjacent_cli(node_executable), sibling_cli]);
  assert.equal(checked.includes(resolve(windows_cli)), false);
});
