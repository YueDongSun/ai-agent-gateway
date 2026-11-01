import assert from 'node:assert/strict';
import { existsSync } from 'node:fs';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { run_release_preflight } from './release_preflight.mjs';

const repository_root = fileURLToPath(new URL('../', import.meta.url));

async function with_fixture(run) {
  const fixture_root = await mkdtemp(path.join(tmpdir(), 'aag_release_preflight_'));
  try {
    await mkdir(path.join(fixture_root, 'src-tauri', 'icons'), { recursive: true });
    await mkdir(path.join(fixture_root, 'ui'), { recursive: true });
    await writeFile(path.join(fixture_root, 'src-tauri', 'tauri.conf.json'), JSON.stringify({
      version: '1.2.3',
      build: { frontendDist: '../ui/dist' },
    }));
    await writeFile(path.join(fixture_root, 'ui', 'package.json'), JSON.stringify({ version: '1.2.3' }));
    await writeFile(path.join(fixture_root, 'src-tauri', 'Cargo.toml'), '[package]\nversion = "1.2.3"\n');
    await run(fixture_root);
  } finally {
    await rm(fixture_root, { recursive: true, force: true });
    assert.equal(existsSync(fixture_root), false);
  }
}

test('missing Tauri manifest is returned as an error and icon checks continue', async () => {
  await with_fixture(async (fixture_root) => {
    await rm(path.join(fixture_root, 'src-tauri', 'tauri.conf.json'));
    const errors = await run_release_preflight(fixture_root);
    assert.ok(errors.includes('src-tauri/tauri.conf.json: unable to read (ENOENT)'));
    assert.ok(errors.includes('src-tauri/icons/icon.png: unable to read (ENOENT)'));
  });
});

test('invalid UI JSON returns a content-free parse diagnostic', async () => {
  await with_fixture(async (fixture_root) => {
    const invalid_json = '{"version":';
    await writeFile(path.join(fixture_root, 'ui', 'package.json'), invalid_json);
    const errors = await run_release_preflight(fixture_root);
    assert.ok(errors.includes('ui/package.json: unable to parse (invalid_json)'));
    assert.equal(errors.join('\n').includes(invalid_json), false);
    assert.ok(errors.includes('src-tauri/icons/icon.png: unable to read (ENOENT)'));
  });
});

test('missing Cargo manifest is reported without parsing TOML', async () => {
  await with_fixture(async (fixture_root) => {
    await rm(path.join(fixture_root, 'src-tauri', 'Cargo.toml'));
    await writeFile(path.join(fixture_root, 'ui', 'package.json'), JSON.stringify({ version: '9.9.9' }));
    const errors = await run_release_preflight(fixture_root);
    assert.ok(errors.includes('src-tauri/Cargo.toml: unable to read (ENOENT)'));
    assert.ok(errors.includes('ui/package.json: version 9.9.9 does not match desktop version 1.2.3'));
    assert.equal(errors.some((error) => error.startsWith('src-tauri/Cargo.toml: version ')), false);
    assert.ok(errors.includes('src-tauri/icons/icon.png: unable to read (ENOENT)'));
  });
});

test('non-object JSON manifest roots are reported as invalid objects', async () => {
  await with_fixture(async (fixture_root) => {
    for (const value of [null, [], true, 42, 'manifest']) {
      await writeFile(path.join(fixture_root, 'src-tauri', 'tauri.conf.json'), JSON.stringify(value));
      await writeFile(path.join(fixture_root, 'ui', 'package.json'), JSON.stringify(value));
      const errors = await run_release_preflight(fixture_root);
      assert.ok(errors.includes('src-tauri/tauri.conf.json: invalid manifest (expected object)'));
      assert.ok(errors.includes('ui/package.json: invalid manifest (expected object)'));
      assert.ok(errors.includes('src-tauri/icons/icon.png: unable to read (ENOENT)'));
    }
  });
});

test('valid checkout retains the passing release preflight result', async () => {
  assert.deepEqual(await run_release_preflight(repository_root), []);
});
