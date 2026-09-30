import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { defineConfig, type Plugin } from 'vite';
import react from '@vitejs/plugin-react';
import { build_settings } from './vite_build_config.mjs';

const tauri_config = JSON.parse(
  readFileSync(resolve(dirname(fileURLToPath(import.meta.url)), '../src-tauri/tauri.conf.json'), 'utf8'),
) as { version: string };

/// Dev-only request logger: prints every HTTP request Vite serves, so
/// `tauri:dev` output shows whether the Tauri webview actually fetched the
/// page (and which URL). Invaluable when the window shows a blank page.
export function safe_request_log_record(request: {
  method?: string;
  url?: string;
  headers?: Record<string, string | string[] | undefined>;
}): string {
  const method = ['GET', 'HEAD', 'POST', 'PUT', 'PATCH', 'DELETE', 'OPTIONS'].includes(
    request.method ?? '',
  )
    ? request.method
    : 'UNKNOWN';
  let pathname = '/invalid';
  try {
    pathname = new URL(request.url ?? '/', 'http://local.invalid').pathname;
  } catch {
    // Keep a stable category and never include the invalid value.
  }
  const route = pathname === '/'
    ? 'root'
    : pathname.startsWith('/@vite') || pathname.startsWith('/@react-refresh')
      ? 'vite_internal'
      : pathname.startsWith('/src/')
        ? 'source_module'
        : /\.[a-zA-Z0-9]{1,8}$/.test(pathname)
          ? 'asset'
          : 'application_route';
  const ua = String(request.headers?.['user-agent'] ?? '');
  const client = ua.includes('WebView2') ? 'webview' : ua.includes('Edg/') ? 'edge' : 'other';
  return `[vite-req] method=${method} route=${route} client=${client}`;
}

function requestLogger(): Plugin {
  return {
    name: 'aag-request-logger',
    apply: 'serve',
    configureServer(server) {
      server.middlewares.use((req, _res, next) => {
        console.log(safe_request_log_record(req));
        next();
      });
    },
  };
}

// Tauri expects a fixed port and to not obscure Rust errors.
//
// Port strategy:
//   * Default to 1421 (the canonical Tauri dev port was 1420; 1421 is the new
//     "AAG dev" port so other Tauri apps on the machine don't collide).
//   * `strictPort: false` lets Vite auto-increment to 1422, 1423, ... when the
//     preferred port is taken. Vite prints "Port X is in use, trying Y" so
//     the chosen port is visible in the dev log.
//   * `AAG_DEV_PORT` overrides the default — useful for CI / scripted runs.
//   * `AAG_DEV_STRICT_PORT=1` restores the old "fail hard if taken" behavior
//     for users who want the loud failure instead of silent fallback.
//
// Tauri-side: `tauri.conf.json` reads `devUrl` from the env-rewritten config
// below; see the post-build step in package.json for the port-discovery flow.
const preferred_port = Number(process.env.AAG_DEV_PORT) || 1421;
const strict_port = process.env.AAG_DEV_STRICT_PORT === '1';

export default defineConfig(({ command, mode }) => {
  const { build_channel, is_demo_build, out_dir } = build_settings(command, mode);

  return {
    plugins: [react(), requestLogger()],
    clearScreen: false,
    define: {
      __AAG_APP_VERSION__: JSON.stringify(tauri_config.version),
      __AAG_BUILD_CHANNEL__: JSON.stringify(build_channel),
      __AAG_DEMO_BUILD__: JSON.stringify(is_demo_build),
    },
    build: {
      outDir: out_dir,
      emptyOutDir: true,
    },
    server: {
      port: preferred_port,
      strictPort: strict_port,
      host: process.env.TAURI_DEV_HOST || false,
      hmr: process.env.TAURI_DEV_HOST
        ? {
            protocol: 'ws',
            host: process.env.TAURI_DEV_HOST,
            port: preferred_port + 1,
          }
        : undefined,
      watch: { ignored: ['**/src-tauri/**'] },
    },
  };
});
