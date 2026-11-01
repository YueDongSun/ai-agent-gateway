import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const entry = fileURLToPath(new URL('./native_smoke.mjs', import.meta.url));

function run(args) {
  const result = spawnSync(process.execPath, [entry, ...args], { encoding: 'utf8', timeout: 15_000, windowsHide: true });
  assert.ifError(result.error);
  assert.equal(result.status, 1);
  assert.match(result.stdout, /DESKTOP_NATIVE_STATUS=blocked/);
  assert.doesNotMatch(result.stdout, /PASS native process/);
  return result;
}

test('native acceptance refuses a console-subsystem artifact before launching it', (context) => {
  const directory = mkdtempSync(join(tmpdir(), 'aag_desktop_image_fixture_'));
  context.after(() => rmSync(directory, { recursive: true, force: true }));
  const image = Buffer.alloc(512);
  image.write('MZ', 0, 'ascii');
  image.writeUInt32LE(0x80, 0x3c);
  image.write('PE\0\0', 0x80, 'ascii');
  image.writeUInt16LE(0x70, 0x94);
  image.writeUInt16LE(0x20b, 0x98);
  image.writeUInt16LE(3, 0x98 + 68);
  const binary = join(directory, 'ai-agent-gateway.exe');
  writeFileSync(binary, image);
  const result = run([binary]);
  assert.match(result.stderr, process.platform === 'win32'
    ? /desktop_native_check_failed:executable is compiled for the Windows GUI subsystem/
    : /desktop_native_requires_windows/);
  assert.doesNotMatch(result.stderr, /aag_desktop_image_fixture_/);
});

test('native acceptance rejects unknown arguments without creating runtime state', () => {
  const result = run(['not_a_binary', '--unexpected']);
  assert.match(result.stderr, process.platform === 'win32'
    ? /desktop_native_unexpected_arguments/
    : /desktop_native_requires_windows/);
  assert.match(result.stdout, /STATE=removed/);
});
