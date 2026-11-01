import assert from 'node:assert/strict';
import test from 'node:test';
import { resolve } from 'node:path';
import { create_tauri_build_arguments } from './tauri_build_config.mjs';

const project_root = resolve('fixture_project');
const local_cli_entry = resolve(project_root, 'node_modules/@tauri-apps/cli/tauri.js');
const config_path = resolve(project_root, 'src-tauri/tauri.conf.json');
const fixed_arguments = [local_cli_entry, 'build', '--config', config_path];

test('creates locked argv with absolute local paths by default', () => {
  const arguments_list = create_tauri_build_arguments(project_root);

  assert.deepEqual(arguments_list, [...fixed_arguments, '--', '--locked']);
  assert.equal(arguments_list[0], local_cli_entry);
  assert.equal(arguments_list[3], config_path);
  assert.ok(resolve(arguments_list[0]) === arguments_list[0]);
  assert.ok(resolve(arguments_list[3]) === arguments_list[3]);
});

test('keeps target, bundles, and CI options before the runner boundary', () => {
  const cli_arguments = ['--target', 'x86_64-pc-windows-msvc', '--bundles', 'nsis', '--ci'];

  assert.deepEqual(create_tauri_build_arguments(project_root, cli_arguments), [
    ...fixed_arguments,
    ...cli_arguments,
    '--',
    '--locked',
  ]);
});

test('preserves explicit Cargo arguments after the runner boundary', () => {
  const cli_arguments = ['--target', 'x86_64-pc-windows-msvc', '--', '--offline', '-j', '2'];

  assert.deepEqual(create_tauri_build_arguments(project_root, cli_arguments), [
    ...fixed_arguments,
    '--target',
    'x86_64-pc-windows-msvc',
    '--',
    '--offline',
    '-j',
    '2',
    '--locked',
  ]);
});

test('treats a leading separator as runner-only arguments', () => {
  const cli_arguments = ['--', '--offline'];

  assert.deepEqual(create_tauri_build_arguments(project_root, cli_arguments), [
    ...fixed_arguments,
    '--',
    '--offline',
    '--locked',
  ]);
});

test('does not duplicate a runner --locked argument', () => {
  const cli_arguments = ['--', '--locked', '--offline'];

  assert.deepEqual(create_tauri_build_arguments(project_root, cli_arguments), [
    ...fixed_arguments,
    '--',
    '--locked',
    '--offline',
  ]);
});

test('preserves ordering, literal values, spaces, metacharacters, and later separators', () => {
  const cli_arguments = [
    '--label',
    'value with spaces',
    '$(touch nowhere) & |',
    '--',
    '--message',
    'cargo value with spaces',
    '--',
    '--color=always',
  ];

  assert.deepEqual(create_tauri_build_arguments(project_root, cli_arguments), [
    ...fixed_arguments,
    '--label',
    'value with spaces',
    '$(touch nowhere) & |',
    '--',
    '--message',
    'cargo value with spaces',
    '--locked',
    '--',
    '--color=always',
  ]);
});

test('keeps locking before a later runner option terminator', () => {
  assert.deepEqual(create_tauri_build_arguments(project_root, ['--', '--offline', '--']), [
    ...fixed_arguments,
    '--',
    '--offline',
    '--locked',
    '--',
  ]);
});

test('does not count positional --locked data as a Cargo locking option', () => {
  assert.deepEqual(create_tauri_build_arguments(project_root, ['--', '--', '--locked']), [
    ...fixed_arguments,
    '--',
    '--locked',
    '--',
    '--locked',
  ]);
});

test('leaves input arrays untouched and returns a fresh result array', () => {
  const cli_arguments = Object.freeze(['--', '--offline']);
  const original_arguments = [...cli_arguments];
  const first_result = create_tauri_build_arguments(project_root, cli_arguments);
  const second_result = create_tauri_build_arguments(project_root, cli_arguments);

  assert.deepEqual(cli_arguments, original_arguments);
  assert.deepEqual(first_result, second_result);
  assert.notStrictEqual(first_result, second_result);
});

test('rejects invalid root and argument inputs with stable content-free errors', () => {
  assert.throws(
    () => create_tauri_build_arguments(''),
    { name: 'TypeError', message: 'root_directory must be a non-empty path string' },
  );
  assert.throws(
    () => create_tauri_build_arguments(null),
    { name: 'TypeError', message: 'root_directory must be a non-empty path string' },
  );
  assert.throws(
    () => create_tauri_build_arguments(`${project_root}\0invalid`),
    { name: 'TypeError', message: 'root_directory must be a non-empty path string' },
  );
  assert.throws(
    () => create_tauri_build_arguments(project_root, null),
    { name: 'TypeError', message: 'cli_arguments must be an array of strings' },
  );
  assert.throws(
    () => create_tauri_build_arguments(project_root, ['--ci', 3]),
    { name: 'TypeError', message: 'cli_arguments must be an array of strings' },
  );
  assert.throws(
    () => create_tauri_build_arguments(project_root, ['--ci', '\0']),
    { name: 'TypeError', message: 'cli_arguments must be an array of strings' },
  );
});
