import { describe, expect, it } from 'vitest';
import { validate_desktop_versions, validate_ico, validate_icns, validate_native_frontend_dist, validate_png } from '../scripts/release_preflight.mjs';

function png_fixture(width: number, height: number, color_type = 6): Uint8Array {
  const bytes = new Uint8Array(33);
  bytes.set([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13, 0x49, 0x48, 0x44, 0x52]);
  new DataView(bytes.buffer).setUint32(16, width);
  new DataView(bytes.buffer).setUint32(20, height);
  bytes[24] = 8;
  bytes[25] = color_type;
  return bytes;
}

function ico_fixture(sizes: number[]): Uint8Array {
  const directory_length = 6 + sizes.length * 16;
  const bytes = new Uint8Array(directory_length + sizes.length);
  const view = new DataView(bytes.buffer);
  view.setUint16(2, 1, true);
  view.setUint16(4, sizes.length, true);
  sizes.forEach((size, index) => {
    const offset = 6 + index * 16;
    bytes[offset] = size === 256 ? 0 : size;
    bytes[offset + 1] = size === 256 ? 0 : size;
    view.setUint32(offset + 8, 1, true);
    view.setUint32(offset + 12, directory_length + index, true);
  });
  return bytes;
}

function icns_fixture(types: string[]): Uint8Array {
  const bytes = new Uint8Array(8 + types.length * 9);
  bytes.set(new TextEncoder().encode('icns'));
  new DataView(bytes.buffer).setUint32(4, bytes.length);
  types.forEach((type, index) => {
    const offset = 8 + index * 9;
    bytes.set(new TextEncoder().encode(type), offset);
    new DataView(bytes.buffer).setUint32(offset + 4, 9);
    bytes[offset + 8] = 1;
  });
  return bytes;
}

describe('release preflight formats', () => {
  it('accepts a square RGBA PNG at its expected size', () => {
    expect(validate_png(png_fixture(32, 32), 32)).toEqual([]);
  });

  it('rejects a PNG with an incorrect color type or dimensions', () => {
    expect(validate_png(png_fixture(64, 32, 2), 32)).toEqual([
      'PNG: expected 32x32 RGBA PNG, found 64x32',
      'PNG: expected 8-bit RGBA PNG, found bit depth 8, color type 2',
    ]);
  });

  it('requires the complete ICO layer set and in-bounds data', () => {
    expect(validate_ico(ico_fixture([16, 24, 32, 48, 64, 256]))).toEqual([]);
    expect(validate_ico(ico_fixture([16]))).toContain('icon.ico: missing 256x256 image layer');
  });

  it('requires a valid ICNS container with a 1024 layer', () => {
    expect(validate_icns(icns_fixture(['icp5', 'ic07', 'ic08', 'ic10']))).toEqual([]);
    expect(validate_icns(icns_fixture(['icp5', 'ic07', 'ic08']))).toContain('icon.icns: missing ic10 image layer');
  });

  it('accepts the classic 32 pixel image and alpha-mask pair', () => {
    expect(validate_icns(icns_fixture(['il32', 'l8mk', 'ic07', 'ic08', 'ic10']))).toEqual([]);
  });

  it('rejects a classic 32 pixel image without its alpha mask', () => {
    expect(validate_icns(icns_fixture(['il32', 'ic07', 'ic08', 'ic10']))).toContain(
      'icon.icns: missing 32x32 image layer (icp5 or il32 plus l8mk)',
    );
  });
});

describe('desktop version authority', () => {
  it('uses the Tauri version as the desktop release authority', () => {
    expect(validate_desktop_versions({
      tauri_config: { version: '0.2.0' },
      ui_package: { version: '0.2.0' },
      cargo_toml: '[package]\nname = "aag"\nversion = "0.2.0"\n',
    })).toEqual([]);
  });

  it('reports mismatched desktop consumers without treating the legacy CLI as one', () => {
    expect(validate_desktop_versions({
      tauri_config: { version: '0.2.0' },
      ui_package: { version: '0.2.1' },
      cargo_toml: '[package]\nversion = "0.1.0"\n',
    })).toEqual([
      'ui/package.json: version 0.2.1 does not match desktop version 0.2.0',
      'src-tauri/Cargo.toml: version 0.1.0 does not match desktop version 0.2.0',
    ]);
  });
});

describe('native frontend output contract', () => {
  it('accepts only the native frontend output directory', () => {
    expect(validate_native_frontend_dist({ build: { frontendDist: '../ui/dist' } })).toEqual([]);
  });

  it('rejects the browser demonstration output and development URL', () => {
    expect(validate_native_frontend_dist({ build: { frontendDist: '../ui/dist_demo' } })).toEqual([
      'src-tauri/tauri.conf.json: desktop frontendDist must be ../ui/dist, found ../ui/dist_demo',
    ]);
    expect(validate_native_frontend_dist({ build: { frontendDist: 'http://localhost:1421' } })).toEqual([
      'src-tauri/tauri.conf.json: desktop frontendDist must be ../ui/dist, found http://localhost:1421',
    ]);
  });
});
