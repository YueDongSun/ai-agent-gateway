import { readFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const png_signature = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
const required_ico_sizes = [16, 24, 32, 48, 64, 256];
const required_icns_types = ['ic07', 'ic08', 'ic10'];
const required_png_files = {
  '32x32.png': 32,
  '128x128.png': 128,
  '128x128@2x.png': 256,
  'icon.png': 512,
  'tray_icon.png': 32,
};

function read_u32_be(bytes, offset) {
  return ((bytes[offset] << 24) >>> 0) + (bytes[offset + 1] << 16) + (bytes[offset + 2] << 8) + bytes[offset + 3];
}

function read_u32_le(bytes, offset) {
  return bytes[offset] + (bytes[offset + 1] << 8) + (bytes[offset + 2] << 16) + ((bytes[offset + 3] << 24) >>> 0);
}

function read_u16_le(bytes, offset) {
  return bytes[offset] + (bytes[offset + 1] << 8);
}

function ascii(bytes, offset, length) {
  return Buffer.from(bytes.subarray(offset, offset + length)).toString('ascii');
}

export function validate_png(bytes, expected_size, file_name = 'PNG') {
  const errors = [];
  if (bytes.length < 33 || !png_signature.every((value, index) => bytes[index] === value)) {
    return [`${file_name}: invalid PNG signature`];
  }
  if (read_u32_be(bytes, 8) !== 13 || ascii(bytes, 12, 4) !== 'IHDR') {
    return [`${file_name}: missing IHDR chunk`];
  }
  const width = read_u32_be(bytes, 16);
  const height = read_u32_be(bytes, 20);
  const bit_depth = bytes[24];
  const color_type = bytes[25];
  if (width !== expected_size || height !== expected_size) {
    errors.push(`${file_name}: expected ${expected_size}x${expected_size} RGBA PNG, found ${width}x${height}`);
  }
  if (bit_depth !== 8 || color_type !== 6) {
    errors.push(`${file_name}: expected 8-bit RGBA PNG, found bit depth ${bit_depth}, color type ${color_type}`);
  }
  return errors;
}

export function validate_ico(bytes, file_name = 'icon.ico') {
  const errors = [];
  if (bytes.length < 6 || read_u16_le(bytes, 0) !== 0 || read_u16_le(bytes, 2) !== 1) {
    return [`${file_name}: invalid ICO header`];
  }
  const entry_count = read_u16_le(bytes, 4);
  if (entry_count === 0 || bytes.length < 6 + entry_count * 16) {
    return [`${file_name}: invalid ICO directory length`];
  }
  const dimensions = new Set();
  for (let index = 0; index < entry_count; index += 1) {
    const offset = 6 + index * 16;
    const width = bytes[offset] || 256;
    const height = bytes[offset + 1] || 256;
    const data_length = read_u32_le(bytes, offset + 8);
    const data_offset = read_u32_le(bytes, offset + 12);
    if (width !== height) {
      errors.push(`${file_name}: directory entry ${index} is not square (${width}x${height})`);
    }
    if (data_length === 0 || data_offset < 6 + entry_count * 16 || data_offset + data_length > bytes.length) {
      errors.push(`${file_name}: directory entry ${index} has out-of-bounds image data`);
    }
    dimensions.add(width);
  }
  for (const required_size of required_ico_sizes) {
    if (!dimensions.has(required_size)) {
      errors.push(`${file_name}: missing ${required_size}x${required_size} image layer`);
    }
  }
  return errors;
}

export function validate_icns(bytes, file_name = 'icon.icns') {
  const errors = [];
  if (bytes.length < 8 || ascii(bytes, 0, 4) !== 'icns') {
    return [`${file_name}: invalid ICNS header`];
  }
  const declared_length = read_u32_be(bytes, 4);
  if (declared_length !== bytes.length) {
    return [`${file_name}: declared length ${declared_length} does not match file length ${bytes.length}`];
  }
  const layer_types = new Set();
  let offset = 8;
  while (offset < bytes.length) {
    if (offset + 8 > bytes.length) {
      errors.push(`${file_name}: truncated ICNS chunk header at offset ${offset}`);
      break;
    }
    const chunk_type = ascii(bytes, offset, 4);
    const chunk_length = read_u32_be(bytes, offset + 4);
    if (chunk_length < 8 || offset + chunk_length > bytes.length) {
      errors.push(`${file_name}: invalid ${chunk_type} chunk length ${chunk_length} at offset ${offset}`);
      break;
    }
    if (chunk_length === 8) {
      errors.push(`${file_name}: empty ${chunk_type} image layer`);
    }
    layer_types.add(chunk_type);
    offset += chunk_length;
  }
  const has_classic_32_layer = layer_types.has('il32') && layer_types.has('l8mk');
  if (!layer_types.has('icp5') && !has_classic_32_layer) {
    errors.push(`${file_name}: missing 32x32 image layer (icp5 or il32 plus l8mk)`);
  }
  for (const required_type of required_icns_types) {
    if (!layer_types.has(required_type)) {
      errors.push(`${file_name}: missing ${required_type} image layer`);
    }
  }
  return errors;
}

export function validate_desktop_versions({ tauri_config, ui_package, cargo_toml }) {
  const errors = [];
  const desktop_version = tauri_config?.version;
  if (typeof desktop_version !== 'string' || desktop_version.length === 0) {
    return ['src-tauri/tauri.conf.json: missing desktop version'];
  }
  if (ui_package && ui_package.version !== desktop_version) {
    errors.push(`ui/package.json: version ${ui_package.version} does not match desktop version ${desktop_version}`);
  }
  if (typeof cargo_toml === 'string') {
    const package_section = cargo_toml.split(/\r?\n(?=\[)/).find((section) => section.startsWith('[package]')) ?? '';
    const cargo_version = package_section.match(/^version\s*=\s*"([^"]+)"\s*$/m)?.[1];
    if (cargo_version !== desktop_version) {
      errors.push(`src-tauri/Cargo.toml: version ${cargo_version ?? 'missing'} does not match desktop version ${desktop_version}`);
    }
  }
  return errors;
}

export function validate_native_frontend_dist(tauri_config) {
  if (tauri_config.build?.frontendDist !== '../ui/dist') {
    return [`src-tauri/tauri.conf.json: desktop frontendDist must be ../ui/dist, found ${tauri_config.build?.frontendDist ?? 'missing'}`];
  }
  return [];
}

async function read_text_file(repository_root, relative_path) {
  try {
    return { content: await readFile(path.join(repository_root, relative_path), 'utf8'), errors: [] };
  } catch (error) {
    const code = typeof error?.code === 'string' ? error.code : 'unknown';
    return { content: null, errors: [`${relative_path}: unable to read (${code})`] };
  }
}

async function read_json_manifest(repository_root, relative_path) {
  const { content, errors } = await read_text_file(repository_root, relative_path);
  if (errors.length > 0) return { value: null, errors };

  let value;
  try {
    value = JSON.parse(content);
  } catch {
    return { value: null, errors: [`${relative_path}: unable to parse (invalid_json)`] };
  }
  if (value === null || typeof value !== 'object' || Array.isArray(value)) {
    return { value: null, errors: [`${relative_path}: invalid manifest (expected object)`] };
  }
  return { value, errors: [] };
}

export async function run_release_preflight(repository_root) {
  const [tauri_config_result, ui_package_result, cargo_result] = await Promise.all([
    read_json_manifest(repository_root, 'src-tauri/tauri.conf.json'),
    read_json_manifest(repository_root, 'ui/package.json'),
    read_text_file(repository_root, 'src-tauri/Cargo.toml'),
  ]);
  const errors = [
    ...tauri_config_result.errors,
    ...ui_package_result.errors,
    ...cargo_result.errors,
  ];
  if (tauri_config_result.value) {
    errors.push(...validate_desktop_versions({
      tauri_config: tauri_config_result.value,
      ui_package: ui_package_result.value,
      cargo_toml: cargo_result.content,
    }));
    errors.push(...validate_native_frontend_dist(tauri_config_result.value));
  }
  const icons_directory = path.join(repository_root, 'src-tauri', 'icons');
  for (const [file_name, expected_size] of Object.entries(required_png_files)) {
    try {
      errors.push(...validate_png(await readFile(path.join(icons_directory, file_name)), expected_size, `src-tauri/icons/${file_name}`));
    } catch (error) {
      errors.push(`src-tauri/icons/${file_name}: unable to read (${error.code ?? error.message})`);
    }
  }
  for (const [file_name, validate_icon] of [['icon.ico', validate_ico], ['icon.icns', validate_icns]]) {
    try {
      errors.push(...validate_icon(await readFile(path.join(icons_directory, file_name)), `src-tauri/icons/${file_name}`));
    } catch (error) {
      errors.push(`src-tauri/icons/${file_name}: unable to read (${error.code ?? error.message})`);
    }
  }
  return errors;
}

const script_path = fileURLToPath(import.meta.url);
if (process.argv[1] && path.resolve(process.argv[1]) === script_path) {
  const errors = await run_release_preflight(path.resolve(path.dirname(script_path), '..'));
  if (errors.length > 0) {
    console.error('Release preflight failed:');
    for (const error of errors) console.error(`- ${error}`);
    process.exitCode = 1;
  } else {
    console.log('Release preflight passed.');
  }
}
