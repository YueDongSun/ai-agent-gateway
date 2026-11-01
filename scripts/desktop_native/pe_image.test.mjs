import assert from 'node:assert/strict';
import test from 'node:test';
import { inspect_windows_image } from './pe_image.mjs';

function image(subsystem = 2, optional_magic = 0x20b) {
  const bytes = Buffer.alloc(512);
  bytes.write('MZ', 0, 'ascii');
  bytes.writeUInt32LE(0x80, 0x3c);
  bytes.write('PE\0\0', 0x80, 'ascii');
  bytes.writeUInt16LE(0x8664, 0x84);
  bytes.writeUInt16LE(0x70, 0x94);
  bytes.writeUInt16LE(optional_magic, 0x98);
  bytes.writeUInt16LE(subsystem, 0x98 + 68);
  return bytes;
}

test('reads the real PE subsystem instead of inferring it from a release path', () => {
  assert.deepEqual(inspect_windows_image(image()), { machine: 0x8664, subsystem: 2, windows_gui: true });
  assert.deepEqual(inspect_windows_image(image(3)), { machine: 0x8664, subsystem: 3, windows_gui: false });
  assert.equal(inspect_windows_image(image(2, 0x10b)).windows_gui, true);
});

test('rejects invalid or truncated native images without guessing a subsystem', () => {
  const variants = [Buffer.alloc(0), Buffer.alloc(63), Buffer.alloc(256), image().subarray(0, 200)];
  const wrong_signature = image();
  wrong_signature.write('NO', 0x80, 'ascii');
  variants.push(wrong_signature);
  const invalid_offset = image();
  invalid_offset.writeUInt32LE(0xfffffff0, 0x3c);
  variants.push(invalid_offset);
  const invalid_optional = image();
  invalid_optional.writeUInt16LE(68, 0x94);
  variants.push(invalid_optional, image(2, 0x107));
  for (const bytes of variants) assert.throws(() => inspect_windows_image(bytes), /desktop_native_invalid_pe_image/);
});
