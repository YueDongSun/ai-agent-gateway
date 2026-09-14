import { defineConfig, type Plugin } from 'vite';
import react from '@vitejs/plugin-react';

/// Dev-only request logger: prints every HTTP request Vite serves, so
/// `tauri:dev` output shows whether the Tauri webview actually fetched the
/// page (and which URL). Invaluable when the window shows a blank page.
function requestLogger(): Plugin {
  return {
    name: 'aag-request-logger',
    apply: 'serve',
    configureServer(server) {
      server.middlewares.use((req, _res, next) => {
        const ua = String(req.headers['user-agent'] ?? '');
        const client = ua.includes('WebView2') ? 'webview' : ua.includes('Edg/') ? 'edge' : 'other';
        console.log(`[vite-req] ${req.method} ${req.url} (${client})`);
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
const preferred = Number(process.env.AAG_DEV_PORT) || 1421;
const strict = process.env.AAG_DEV_STRICT_PORT === '1';

export default defineConfig({
  plugins: [react(), requestLogger()],
  clearScreen: false,
  server: {
    port: preferred,
    strictPort: strict,
    host: process.env.TAURI_DEV_HOST || false,
    hmr: process.env.TAURI_DEV_HOST
      ? {
          protocol: 'ws',
          host: process.env.TAURI_DEV_HOST,
          port: preferred + 1,
        }
      : undefined,
    watch: { ignored: ['**/src-tauri/**'] },
  },
});