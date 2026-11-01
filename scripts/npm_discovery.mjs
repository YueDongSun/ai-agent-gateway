import { existsSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';

/**
 * @param {object} [options]
 * @param {NodeJS.ProcessEnv} [options.env]
 * @param {string} [options.node_executable]
 * @param {NodeJS.Platform} [options.platform]
 * @param {(path: string) => boolean} [options.path_exists]
 * @returns {string | null}
 */
export function find_npm_cli({
  env = process.env,
  node_executable = process.execPath,
  platform = process.platform,
  path_exists = existsSync,
} = {}) {
  const candidates = [
    env.npm_execpath,
    join(dirname(node_executable), 'node_modules', 'npm', 'bin', 'npm-cli.js'),
  ];
  if (platform === 'win32') {
    candidates.push(join(env.ProgramFiles || 'C:/Program Files', 'nodejs', 'node_modules', 'npm', 'bin', 'npm-cli.js'));
  }
  candidates.push(join(dirname(node_executable), '..', 'lib', 'node_modules', 'npm', 'bin', 'npm-cli.js'));

  for (const candidate of candidates) {
    if (typeof candidate !== 'string' || candidate.length === 0) continue;
    const absolute_candidate = resolve(candidate);
    if (path_exists(absolute_candidate)) return absolute_candidate;
  }
  return null;
}
