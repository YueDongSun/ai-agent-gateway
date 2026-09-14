// scripts/tauri-dev.mjs
// Resolves the port Vite actually landed on, then writes a per-run
// `src-tauri/tauri.dev.json` whose `build.devUrl` points there, and
// finally exec's `tauri dev --config src-tauri/tauri.dev.json`.
//
// Vite prints a single line like:
//
//   VITE v5.x.x  ready in 412 ms
//     ➜  Local:   http://localhost:1421/
//     ➜  Network: use --host to expose
//
// when the preferred port is taken, Vite instead prints something like:
//
//   Port 1421 is in use, trying 1422 instead.
//     ➜  Local:   http://localhost:1422/
//
// We need the *actual* port (the last "Local: http://localhost:NNN/" match)
// before Tauri spawns its webview, otherwise the window opens a blank page.
//
// Strategy:
//   1. Spawn `npm run ui:dev` (which is `npm run dev --prefix ui`) as a child
//      and tee its stdout/stderr through this script.
//   2. Regex out the resolved port from each line and rewrite
//      `src-tauri/tauri.dev.json` on the fly.
//   3. Once we know the port, exec `tauri dev --config src-tauri/tauri.dev.json`
//      so the webview points at the live Vite server.
//
// `--strictPort` can be re-enabled with AAG_DEV_STRICT_PORT=1; in that case
// we don't need the rewrite step and can hand off directly to Tauri.

import { spawn } from 'node:child_process';
import { readFileSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';

const ROOT = resolve(process.cwd());
const TAURI_TEMPLATE = resolve(ROOT, 'src-tauri/tauri.conf.json');
const TAURI_DEV = resolve(ROOT, 'src-tauri/tauri.dev.json');

const strict = process.env.AAG_DEV_STRICT_PORT === '1';

if (strict) {
  // Loud-fail path: port collisions stop the build with a clear error.
  const cli = process.argv.includes('cli');
  const args = cli
    ? process.argv.slice(process.argv.indexOf('cli') + 1)
    : ['dev', '--config', 'src-tauri/tauri.conf.json'];
  const child = spawn('npx', ['tauri', ...args], { stdio: 'inherit', shell: true });
  child.on('exit', (code) => process.exit(code ?? 1));
  process.exit(0);
}

// 1. Read the canonical config so we can derive the dev override.
const baseConfig = JSON.parse(readFileSync(TAURI_TEMPLATE, 'utf8'));

// 2. Spawn Vite via `npm run dev --prefix ui`. We stream stdout/stderr
//    through us so the user sees Vite's normal output, and we parse every
//    line for the resolved port.
const vite = spawn('npm', ['run', 'dev', '--prefix', 'ui'], {
  stdio: ['ignore', 'pipe', 'pipe'],
  shell: true,
});

let resolvedPort = null;
const localRe = /Local:\s+(?:https?:\/\/)?(?:localhost|127\.0\.0\.1|\[::1?\]):(\d+)/i;
// Vite (via chalk) wraps "Local" / the URL in ANSI color codes, so the raw
// line is e.g. "[32m➜[39m  [1mLocal[22m:   [36mhttp://localhost:[1m1423[22m/".
// Strip ANSI before regex-matching, otherwise "Local:" never lines up.
const ANSI_RE = /\[[0-9;]*m/g;
function stripAnsi(s) {
  return s.replace(ANSI_RE, '');
}

function handleLine(line) {
  process.stdout.write(line + '\n');
  const m = localRe.exec(stripAnsi(line));
  if (m) {
    const port = Number(m[1]);
    if (port !== resolvedPort) {
      resolvedPort = port;
      const devUrl = `http://localhost:${port}`;
      // Note: `beforeDevCommand` is intentionally nulled — we already
      // spawned Vite ourselves so we can resolve its actual port. Leaving
      // the original command in place makes Tauri spawn a *second* Vite
      // whose port we don't see, which races the webview's first paint.
      const override = {
        ...baseConfig,
        build: {
          ...baseConfig.build,
          devUrl,
          beforeDevCommand: '',
          beforeBuildCommand: baseConfig.build.beforeBuildCommand,
        },
      };
      writeFileSync(TAURI_DEV, JSON.stringify(override, null, 2) + '\n');
      if (!tauriSpawned) {
        spawnTauri();
      } else {
        // Tauri picks up the devUrl from the conf it was launched with, so
        // mid-run port bumps are a no-op for the webview. We still log it.
        console.log(`[tauri-dev] vite port bumped to ${port}; webview URL won't change mid-run`);
      }
    }
  }
}

function pipe(stream) {
  let buf = '';
  stream.setEncoding('utf8');
  stream.on('data', (chunk) => {
    buf += chunk;
    const lines = buf.split(/\r?\n/);
    buf = lines.pop(); // last partial line stays in buf
    for (const line of lines) handleLine(line);
  });
  stream.on('end', () => {
    if (buf.length) handleLine(buf);
  });
}

pipe(vite.stdout);
pipe(vite.stderr);

vite.on('exit', (code) => {
  if (!tauriSpawned) {
    console.error(`[tauri-dev] vite exited with code ${code} before port was resolved`);
    process.exit(code ?? 1);
  }
});

// 3. As soon as we know the port, spawn Tauri.
let tauriSpawned = false;
function spawnTauri() {
  tauriSpawned = true;
  console.log(`[tauri-dev] resolved Vite port ${resolvedPort}, launching Tauri`);
  // Forward any extra args (e.g. `-- --no-watch`) to Tauri.
  const extraIdx = process.argv.indexOf('--');
  const extra = extraIdx >= 0 ? process.argv.slice(extraIdx + 1) : [];
  const tauri = spawn(
    'npx',
    ['tauri', 'dev', '--config', 'src-tauri/tauri.dev.json', ...extra],
    { stdio: 'inherit', shell: true },
  );
  tauri.on('exit', (code) => {
    // If Tauri exits, kill Vite too so we don't leave a zombie.
    if (vite.exitCode === null) vite.kill();
    process.exit(code ?? 0);
  });
  // Tauri child in front — Ctrl+C in the terminal sends to the whole group.
}

// If Vite crashes before printing its URL, surface the failure.
process.on('SIGINT', () => {
  vite.kill();
  process.exit(130);
});
process.on('SIGTERM', () => {
  vite.kill();
  process.exit(143);
});