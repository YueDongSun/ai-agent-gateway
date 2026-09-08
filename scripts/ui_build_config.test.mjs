import assert from 'node:assert/strict';
import test from 'node:test';
import { build_settings } from '../ui/vite_build_config.mjs';

test('release builds preserve the native output boundary', () => {
  assert.deepEqual(build_settings('build', 'production'), {
    build_channel: 'release',
    is_demo_build: false,
    out_dir: 'dist',
  });
});

test('demo builds use an isolated browser output boundary', () => {
  assert.deepEqual(build_settings('build', 'demo'), {
    build_channel: 'demo',
    is_demo_build: true,
    out_dir: 'dist_demo',
  });
});

test('development server declares a distinct channel', () => {
  assert.deepEqual(build_settings('serve', 'development'), {
    build_channel: 'development',
    is_demo_build: false,
    out_dir: 'dist',
  });
});
