import { resolve } from 'node:path';

const tauri_cli_relative_path = 'node_modules/@tauri-apps/cli/tauri.js';
const tauri_config_relative_path = 'src-tauri/tauri.conf.json';
const locked_cargo_argument = '--locked';

/**
 * Construct argv for the installed local Tauri CLI and its Cargo runner.
 *
 * The first exact `--` in `cli_arguments` divides Tauri options from runner
 * arguments. Cargo's locking flag stays before any later runner separator;
 * an existing runner option `--locked` is not duplicated.
 *
 * @param {string} root_directory Project root used to resolve local paths.
 * @param {readonly string[]} [cli_arguments=[]] Arguments forwarded to Tauri and Cargo.
 * @returns {string[]} Fresh argv beginning with the absolute local CLI entry.
 * @throws {TypeError} If the root or argument list is not a valid string input.
 */
export function create_tauri_build_arguments(root_directory, cli_arguments = []) {
  if (typeof root_directory !== 'string' || root_directory.length === 0 || root_directory.includes('\0')) {
    throw new TypeError('root_directory must be a non-empty path string');
  }

  if (!Array.isArray(cli_arguments)) {
    throw new TypeError('cli_arguments must be an array of strings');
  }

  for (const argument of cli_arguments) {
    if (typeof argument !== 'string' || argument.includes('\0')) {
      throw new TypeError('cli_arguments must be an array of strings');
    }
  }

  const absolute_root_directory = resolve(root_directory);
  const tauri_arguments = [];
  const runner_arguments = [];
  const separator_index = cli_arguments.indexOf('--');

  if (separator_index === -1) {
    tauri_arguments.push(...cli_arguments);
  } else {
    tauri_arguments.push(...cli_arguments.slice(0, separator_index));
    runner_arguments.push(...cli_arguments.slice(separator_index + 1));
  }

  const runner_separator_index = runner_arguments.indexOf('--');
  const runner_options_end = runner_separator_index === -1 ? runner_arguments.length : runner_separator_index;
  if (!runner_arguments.slice(0, runner_options_end).includes(locked_cargo_argument)) {
    runner_arguments.splice(runner_options_end, 0, locked_cargo_argument);
  }

  return [
    resolve(absolute_root_directory, tauri_cli_relative_path),
    'build',
    '--config',
    resolve(absolute_root_directory, tauri_config_relative_path),
    ...tauri_arguments,
    '--',
    ...runner_arguments,
  ];
}
