import { execFileSync } from 'node:child_process';
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, renameSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { basename, dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { validate_icns, validate_ico, validate_png } from './release_preflight.mjs';

const repository_root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const icon_directory = join(repository_root, 'src-tauri', 'icons');
const temporary_prefix = 'aag_icons_';
const generation_timeout_ms = 60_000;
const png_sizes = {
  '32x32.png': 32,
  '128x128.png': 128,
  '128x128@2x.png': 256,
};

function generate_asset(source_path, output_directory, png_size) {
  const arguments_list = [
    join(repository_root, 'node_modules', '@tauri-apps', 'cli', 'tauri.js'),
    'icon', source_path, '--output', output_directory,
  ];
  if (png_size) arguments_list.push('--png', String(png_size));
  execFileSync(process.execPath, arguments_list, {
    cwd: repository_root,
    stdio: 'pipe',
    timeout: generation_timeout_ms,
    windowsHide: true,
  });
}

export function generate_icons() {
  const temporary_directory = mkdtempSync(join(tmpdir(), temporary_prefix));
  try {
    const application_directory = join(temporary_directory, 'application');
    const large_directory = join(temporary_directory, 'large');
    const tray_directory = join(temporary_directory, 'tray');
    const source_path = join(repository_root, 'ui', 'public', 'app_icon.svg');
    generate_asset(source_path, application_directory);
    generate_asset(source_path, large_directory, 512);
    generate_asset(join(repository_root, 'ui', 'public', 'tray_mark.svg'), tray_directory, 32);

    const sources = Object.fromEntries(Object.keys(png_sizes).map((name) => [name, join(application_directory, name)]));
    sources['icon.png'] = join(large_directory, '512x512.png');
    sources['tray_icon.png'] = join(tray_directory, '32x32.png');
    sources['icon.ico'] = join(application_directory, 'icon.ico');
    sources['icon.icns'] = join(application_directory, 'icon.icns');

    const errors = [];
    for (const [name, size] of Object.entries({ ...png_sizes, 'icon.png': 512, 'tray_icon.png': 32 })) {
      errors.push(...validate_png(readFileSync(sources[name]), size, name));
    }
    errors.push(...validate_ico(readFileSync(sources['icon.ico'])));
    errors.push(...validate_icns(readFileSync(sources['icon.icns'])));
    if (errors.length) throw new Error('Generated icon validation failed:\n' + errors.join('\n'));

    mkdirSync(icon_directory, { recursive: true });
    for (const [name, source] of Object.entries(sources)) {
      const destination = join(icon_directory, name);
      const temporary_target = destination + '.tmp';
      copyFileSync(source, temporary_target);
      renameSync(temporary_target, destination);
      console.log('Generated ' + name);
    }
  } finally {
    const resolved_directory = resolve(temporary_directory);
    if (dirname(resolved_directory) !== resolve(tmpdir()) || !basename(resolved_directory).startsWith(temporary_prefix)) {
      throw new Error('Refusing cleanup outside the owned icon directory');
    }
    rmSync(resolved_directory, { recursive: true, force: true, maxRetries: 3, retryDelay: 100 });
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  generate_icons();
}
