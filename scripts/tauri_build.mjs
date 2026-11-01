import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { fileURLToPath } from 'node:url';
import { create_tauri_build_arguments } from './tauri_build_config.mjs';

const root_directory = fileURLToPath(new URL('../', import.meta.url));
const signal_exit_codes = { SIGINT: 130, SIGTERM: 143 };
const termination_timeout_ms = 2_000;

async function run_build(cli_arguments) {
  let build_process;
  let termination_signal;
  let termination_timer;

  function terminate_build(signal) {
    termination_signal ??= signal;
    if (!build_process) return;
    if (termination_timer) {
      build_process.kill('SIGKILL');
      return;
    }
    build_process.kill(termination_signal);
    termination_timer = setTimeout(() => build_process.kill('SIGKILL'), termination_timeout_ms);
    termination_timer.unref();
  }

  const sigint_handler = () => terminate_build('SIGINT');
  const sigterm_handler = () => terminate_build('SIGTERM');
  process.on('SIGINT', sigint_handler);
  process.on('SIGTERM', sigterm_handler);

  try {
    if (termination_signal) return signal_exit_codes[termination_signal];
    build_process = spawn(
      process.execPath,
      create_tauri_build_arguments(root_directory, cli_arguments),
      { cwd: root_directory, stdio: 'inherit', shell: false },
    );
    const completion = once(build_process, 'close');
    if (termination_signal) terminate_build(termination_signal);
    const [exit_code, exit_signal] = await completion;
    return signal_exit_codes[termination_signal] ?? exit_code ?? signal_exit_codes[exit_signal] ?? 1;
  } finally {
    process.removeListener('SIGINT', sigint_handler);
    process.removeListener('SIGTERM', sigterm_handler);
    if (termination_timer) clearTimeout(termination_timer);
  }
}

try {
  process.exitCode = await run_build(process.argv.slice(2));
} catch (error) {
  console.error(`tauri_build_launch_failed:${error.code ?? 'unknown'}`);
  process.exitCode = 1;
}
