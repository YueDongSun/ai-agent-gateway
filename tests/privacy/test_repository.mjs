import { mkdirSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { tmpdir } from 'node:os';
import { spawnSync } from 'node:child_process';

const repository_root = new URL('../../', import.meta.url);

export function run_git(root, args, input) {
  const result = spawnSync('git', ['-C', root, ...args], {
    encoding: 'utf8',
    input,
    maxBuffer: 16 * 1024 * 1024,
    windowsHide: true,
  });
  if (result.error || result.status !== 0) throw new Error('test_git_failed');
  return result.stdout.trim();
}

function write(root, path, value) {
  const target = join(root, ...path.split('/'));
  mkdirSync(dirname(target), { recursive: true });
  writeFileSync(target, value);
}

export function create_test_repository() {
  const base = mkdtempSync(join(tmpdir(), 'aag_privacy_test_'));
  const root = join(base, 'repository');
  mkdirSync(root);
  run_git(root, ['init', '--quiet']);
  run_git(root, ['config', 'user.name', 'Public Fixture']);
  run_git(root, ['config', 'user.email', 'build@example.com']);
  write(root, '.gitignore', readFileSync(new URL('../../.gitignore', import.meta.url), 'utf8'));
  write(root, '.env.example', readFileSync(new URL('../../.env.example', import.meta.url), 'utf8'));
  write(root, '.npmignore', readFileSync(new URL('../../.npmignore', import.meta.url), 'utf8'));
  write(root, 'scripts/privacy/audit_repository.mjs', readFileSync(new URL('../../scripts/privacy/audit_repository.mjs', import.meta.url), 'utf8'));
  write(root, 'scripts/privacy/export_snapshot.mjs', readFileSync(new URL('../../scripts/privacy/export_snapshot.mjs', import.meta.url), 'utf8'));
  write(root, 'scripts/privacy/git_tree.mjs', readFileSync(new URL('../../scripts/privacy/git_tree.mjs', import.meta.url), 'utf8'));
  write(root, 'scripts/privacy/public_export_policy.json', readFileSync(new URL('../../scripts/privacy/public_export_policy.json', import.meta.url), 'utf8'));
  write(root, 'package.json', `${JSON.stringify({
    name: 'aag-privacy-fixture',
    version: '1.0.0',
    private: true,
    files: ['dist'],
    scripts: {
      'privacy:audit': 'node scripts/privacy/audit_repository.mjs',
    },
  }, null, 2)}\n`);
  write(root, 'README.md', '# Public fixture\n');
  write(root, 'src/index.js', 'export const ready = true;\n');
  write(root, 'shared/dsh_market_catalog.json', `${JSON.stringify({ schema_version: 1, entry_count: 0, entries: [] })}\n`);
  write(root, 'docs/reviews/mcp_dsh_workspace_receipt.json', '{"internal":true}\n');
  run_git(root, ['add', '.']);
  run_git(root, ['commit', '--quiet', '-m', 'Create public fixture']);
  return { base, root };
}
