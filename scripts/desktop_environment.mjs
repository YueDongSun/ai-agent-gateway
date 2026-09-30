import { execFile } from 'node:child_process';
import { constants, existsSync } from 'node:fs';
import { access, realpath, stat } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { basename, dirname, join } from 'node:path';
import { find_browser } from './ui_smoke/smoke_runtime.mjs';

const probe_timeout_ms = 5_000;
const probe_output_limit = 8 * 1024;
const supported_scopes = ['node', 'ui', 'mcp', 'dsh', 'all'];
const minimum_node_version = [20, 0, 0];
const minimum_rust_version = [1, 88, 0];

/** Only fixed local version/toolchain probes call this adapter. Never uses a shell. */
export function probe_command(executable, args, { env = process.env, signal, timeout_ms = probe_timeout_ms } = {}) {
  return new Promise((resolve_probe) => {
    execFile(executable, args, {
      env, signal, timeout: timeout_ms, maxBuffer: probe_output_limit,
      killSignal: 'SIGKILL', windowsHide: true, shell: false,
    }, (error, stdout) => {
      if (!error) return resolve_probe({ status: 'passed', stdout });
      const status = error.name === 'AbortError' ? 'cancelled'
        : error.code === 'ENOENT' ? 'unavailable'
          : error.code === 'ERR_CHILD_PROCESS_STDIO_MAXBUFFER' ? 'output_limit'
            : error.killed ? 'timed_out' : 'failed';
      // External stderr and filesystem paths do not belong in the receipt.
      resolve_probe({ status, stdout: '' });
    });
  });
}

function version_from(output) {
  const match = output.match(/\b(\d+)\.(\d+)\.(\d+)\b/);
  return match ? match.slice(1).map(Number) : null;
}

function version_satisfies(version, minimum) {
  return version && version.reduce((result, component, index) => result || Math.sign(component - minimum[index]), 0) >= 0;
}

export async function canonical_temporary_directory(directory = tmpdir()) {
  const canonical_directory = await realpath(directory);
  if (!(await stat(canonical_directory)).isDirectory()) throw new Error('AAG_TEMPORARY_DIRECTORY_REQUIRED');
  await access(canonical_directory, constants.R_OK | constants.W_OK | constants.X_OK);
  return canonical_directory;
}

/** Read-only environment discovery. Missing tools are reported, never installed. */
export async function inspect_desktop_environment({
  root, scope = 'all', platform = process.platform, arch = process.arch,
  node_version = process.versions.node, env = process.env, signal,
  path_exists = existsSync, probe = probe_command, browser = find_browser,
  temporary_directory = canonical_temporary_directory,
} = {}) {
  if (!supported_scopes.includes(scope)) throw new Error('AAG_INVALID_SCOPE');
  const checks = [];
  const add = (id, passed, detail, remediation) => {
    checks.push({ id, status: passed ? 'passed' : 'failed', detail, ...(!passed ? { remediation } : {}) });
  };
  const add_file = (id, relative_path, remediation) => add(id, path_exists(join(root, relative_path)), 'required local dependency', remediation);
  const check_version = async (id, executable, args, minimum, remediation) => {
    const result = await probe(executable, args, { env, signal });
    const version = version_from(result.stdout);
    const passed = result.status === 'passed' && version_satisfies(version, minimum);
    const reason = result.status !== 'passed' ? result.status : version ? 'unsupported_version' : 'unrecognized_version';
    add(id, passed, passed ? version.join('.') : `requires >= ${minimum.join('.')} (${reason})`, remediation);
  };
  add('node', version_satisfies(version_from(node_version), minimum_node_version), node_version, 'Install Node.js 20 or newer.');
  const npm_entry = env.npm_execpath || join(dirname(process.execPath), 'node_modules/npm/bin/npm-cli.js');
  const npm_is_script = /\.[cm]?js$/.test(npm_entry) && path_exists(npm_entry);
  await check_version('npm', npm_is_script ? process.execPath : 'npm', npm_is_script ? [npm_entry, '--version'] : ['--version'], [1, 0, 0], 'Install npm with Node.js and make it available on PATH.');
  if (scope !== 'ui') {
    add_file('node_typescript', 'node_modules/typescript/bin/tsc', 'Run npm ci --ignore-scripts.');
    add_file('node_vitest', 'node_modules/vitest/vitest.mjs', 'Run npm ci --ignore-scripts.');
  }
  try {
    await temporary_directory();
    add('temporary_directory', true, 'canonical writable directory', '');
  } catch {
    add('temporary_directory', false, 'temporary directory unavailable', 'Provide an existing writable temporary directory.');
  }
  if (scope !== 'node') {
    await check_version('git', 'git', ['--version'], [1, 0, 0], 'Install Git and make it available on PATH for the privacy audit.');
    add_file('ui_typescript', 'ui/node_modules/typescript/bin/tsc', 'Run npm ci --prefix ui --ignore-scripts.');
    await check_version('vite', process.execPath, [join(root, 'ui/node_modules/vite/bin/vite.js'), '--version'], [1, 0, 0], 'Run npm ci --prefix ui --ignore-scripts.');
    let browser_path;
    try { browser_path = browser(platform); } catch { browser_path = null; }
    add('browser', Boolean(browser_path), browser_path ? basename(browser_path) : 'supported browser unavailable', 'Install Chrome, Edge, or Chromium, or set AAG_BROWSER_PATH to its executable.');
  }
  if (['mcp', 'dsh', 'all'].includes(scope)) {
    await check_version('tauri', process.execPath, [join(root, 'node_modules/@tauri-apps/cli/tauri.js'), '--version'], [2, 0, 0], 'Run npm ci --ignore-scripts for this operating system and architecture.');
    await check_version('rustc', 'rustc', ['--version'], minimum_rust_version, 'Install a supported Rust toolchain and make rustc available on PATH.');
    await check_version('cargo', 'cargo', ['--version'], minimum_rust_version, 'Install a supported Rust toolchain and make cargo available on PATH.');
    if (platform === 'darwin') {
      for (const [id, executable, args, detail] of [
        ['xcode_tools', '/usr/bin/xcode-select', ['-p'], 'selected developer directory'],
        ['macos_clang', '/usr/bin/xcrun', ['--find', 'clang'], 'available C compiler'],
        ['macos_sdk', '/usr/bin/xcrun', ['--sdk', 'macosx', '--show-sdk-version'], 'available macOS SDK'],
      ]) {
        const result = await probe(executable, args, { env, signal });
        add(id, result.status === 'passed' && Boolean(result.stdout.trim()), result.status === 'passed' ? detail : result.status,
          'Install Xcode Command Line Tools with xcode-select --install, then select a working developer directory.');
      }
    }
  }
  return {
    schema_version: 1, scope, platform, arch,
    status: signal?.aborted ? 'cancelled' : checks.every((check) => check.status === 'passed') ? 'passed' : 'failed',
    checks, evidence_class: 'local_prerequisites_only',
  };
}
