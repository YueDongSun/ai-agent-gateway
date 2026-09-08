import { resolve } from 'node:path';

export function create_tauri_dev_config(base_config, dev_url) {
  return {
    ...base_config,
    build: {
      ...base_config.build,
      devUrl: dev_url,
      beforeDevCommand: '',
    },
  };
}

export function local_tauri_entry(root_directory) {
  return resolve(root_directory, 'node_modules/@tauri-apps/cli/tauri.js');
}

export function forwarded_arguments(argv) {
  const arguments_after_runtime = argv.slice(2);
  return arguments_after_runtime[0] === '--'
    ? arguments_after_runtime.slice(1)
    : arguments_after_runtime;
}

export function development_url(host, port) {
  if (!host || /[/?#\s]/.test(host)) {
    throw new Error('TAURI_DEV_HOST must be a hostname or IP address');
  }

  if (host.startsWith('[') !== host.endsWith(']')) {
    throw new Error('TAURI_DEV_HOST has mismatched IPv6 brackets');
  }

  const url_host = host.includes(':') && !host.startsWith('[') ? `[${host}]` : host;
  return `http://${url_host}:${port}`;
}
