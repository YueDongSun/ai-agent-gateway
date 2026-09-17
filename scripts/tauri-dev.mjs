import { once } from 'node:events';
import { spawn } from 'node:child_process';
import { readFileSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import {
  create_tauri_dev_config,
  development_url,
  forwarded_arguments,
  local_tauri_entry,
} from './tauri_dev_config.mjs';

const root_directory = resolve(process.cwd());
const ui_directory = resolve(root_directory, 'ui');
const tauri_config_path = resolve(root_directory, 'src-tauri/tauri.conf.json');
const tauri_dev_config_path = resolve(root_directory, 'src-tauri/tauri.dev.json');
const vite_entry_path = resolve(ui_directory, 'node_modules/vite/dist/node/index.js');
const startup_timeout_ms = 30_000;

async function load_vite_server() {
  const vite_module = await import(pathToFileURL(vite_entry_path).href);
  return vite_module.createServer({
    root: ui_directory,
    configFile: resolve(ui_directory, 'vite.config.ts'),
  });
}

function resolved_dev_url(vite_server) {
  const address = vite_server.httpServer?.address();
  if (!address || typeof address === 'string') {
    throw new Error('Vite did not expose a TCP listening address');
  }

  return development_url(process.env.TAURI_DEV_HOST || 'localhost', address.port);
}

async function start_vite_server() {
  const vite_server = await load_vite_server();
  let startup_timer;
  try {
    const startup_timeout = new Promise((_, reject) => {
      startup_timer = setTimeout(
        () => reject(new Error(`Vite did not start within ${startup_timeout_ms}ms`)),
        startup_timeout_ms,
      );
    });
    await Promise.race([vite_server.listen(), startup_timeout]);
    vite_server.printUrls();
    return vite_server;
  } catch (error) {
    await vite_server.close();
    throw error;
  } finally {
    clearTimeout(startup_timer);
  }
}

function launch_tauri() {
  const tauri_entry = local_tauri_entry(root_directory);
  return spawn(
    process.execPath,
    [tauri_entry, 'dev', '--config', 'src-tauri/tauri.dev.json', ...forwarded_arguments(process.argv)],
    { stdio: 'inherit' },
  );
}

async function wait_for_child(child) {
  const [code] = await once(child, 'exit');
  return code ?? 1;
}

async function run() {
  const base_config = JSON.parse(readFileSync(tauri_config_path, 'utf8'));
  const vite_server = await start_vite_server();
  let tauri_process;
  let exit_code = 1;
  let resolve_termination = () => {};
  const termination = new Promise((resolve_termination_signal) => {
    resolve_termination = resolve_termination_signal;
  });
  const handle_signal = (signal_exit_code) => {
    if (tauri_process && !tauri_process.killed) tauri_process.kill();
    resolve_termination(signal_exit_code);
  };
  const sigint_handler = () => handle_signal(130);
  const sigterm_handler = () => handle_signal(143);
  process.once('SIGINT', sigint_handler);
  process.once('SIGTERM', sigterm_handler);
  try {
    const dev_url = resolved_dev_url(vite_server);
    const dev_config = create_tauri_dev_config(base_config, dev_url);
    writeFileSync(tauri_dev_config_path, `${JSON.stringify(dev_config, null, 2)}\n`);
    console.log(`[tauri-dev] serving ${dev_url}`);
    tauri_process = launch_tauri();
    exit_code = await Promise.race([wait_for_child(tauri_process), termination]);
  } finally {
    process.removeListener('SIGINT', sigint_handler);
    process.removeListener('SIGTERM', sigterm_handler);
    if (tauri_process && !tauri_process.killed) tauri_process.kill();
    await vite_server.close();
    process.exitCode = exit_code;
  }
}

run().catch((error) => {
  console.error(`[tauri-dev] ${error instanceof Error ? error.message : String(error)}`);
  process.exitCode = 1;
});
