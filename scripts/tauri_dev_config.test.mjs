import assert from 'node:assert/strict';
import test from 'node:test';
import {
  create_tauri_dev_config,
  development_url,
  forwarded_arguments,
  local_tauri_entry,
} from './tauri_dev_config.mjs';

test('create_tauri_dev_config replaces only the development launcher boundary', () => {
  const base_config = {
    build: {
      beforeDevCommand: 'npm run dev --prefix ui',
      beforeBuildCommand: 'npm run build --prefix ui',
      devUrl: 'http://localhost:1421',
    },
  };

  const config = create_tauri_dev_config(base_config, 'http://127.0.0.1:1423');

  assert.equal(config.build.devUrl, 'http://127.0.0.1:1423');
  assert.equal(config.build.beforeDevCommand, '');
  assert.equal(config.build.beforeBuildCommand, 'npm run build --prefix ui');
  assert.equal(base_config.build.beforeDevCommand, 'npm run dev --prefix ui');
});

test('local_tauri_entry targets the installed CLI without a shell shim', () => {
  assert.match(local_tauri_entry('I:/cc_copilot_kilo'), /node_modules[\\/]@tauri-apps[\\/]cli[\\/]tauri\.js$/);
});

test('forwarded_arguments supports direct and npm-separated script arguments', () => {
  assert.deepEqual(forwarded_arguments(['node', 'tauri-dev.mjs', '--no-watch']), ['--no-watch']);
  assert.deepEqual(forwarded_arguments(['node', 'tauri-dev.mjs', '--', '--no-watch']), ['--no-watch']);
});

test('development_url brackets IPv6 hosts', () => {
  assert.equal(development_url('::1', 1421), 'http://[::1]:1421');
  assert.equal(development_url('[::1]', 1421), 'http://[::1]:1421');
});
