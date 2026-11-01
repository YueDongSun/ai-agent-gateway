import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { audit_repository } from '../../scripts/privacy/audit_repository.mjs';
import { export_path_allowed, export_snapshot, verify_snapshot_links } from '../../scripts/privacy/export_snapshot.mjs';
import { create_test_repository, run_git } from './test_repository.mjs';

test('exports only the sanitized tracked tree with a deterministic hash manifest', () => {
  const fixture = create_test_repository();
  const first = join(fixture.base, 'snapshot_one');
  const second = join(fixture.base, 'snapshot_two');
  try {
    mkdirSync(join(fixture.root, '.claude'), { recursive: true });
    writeFileSync(join(fixture.root, '.claude', 'settings.local.json'), '{"local":true}\n');
    const first_manifest = export_snapshot({ repository_root: fixture.root, output: first });
    const second_manifest = export_snapshot({ repository_root: fixture.root, output: second });
    const disk_manifest = JSON.parse(readFileSync(join(first, 'public_snapshot_manifest.json'), 'utf8'));
    assert.equal(first_manifest.export_tree_sha256, second_manifest.export_tree_sha256);
    assert.deepEqual(
      JSON.parse(readFileSync(join(first, 'public_snapshot_manifest.json'), 'utf8')),
      JSON.parse(readFileSync(join(second, 'public_snapshot_manifest.json'), 'utf8')),
    );
    assert.equal(existsSync(join(first, '.git')), false);
    assert.equal(existsSync(join(first, '.claude', 'settings.local.json')), false);
    assert.equal(existsSync(join(first, 'docs', 'reviews', 'mcp_dsh_workspace_receipt.json')), false);
    assert.ok(first_manifest.files.every(file => /^[a-f0-9]{64}$/.test(file.sha256)));
    assert.equal(first_manifest.privacy_report.summary.blockers, 0);
    assert.match(first_manifest.private_source.commit, /^[a-f0-9]{40}$/);
    assert.equal(Object.hasOwn(disk_manifest, 'source'), false);
    assert.equal(Object.hasOwn(disk_manifest, 'private_source'), false);
    assert.equal(JSON.stringify(disk_manifest).includes('"commit_time":'), false);
    assert.equal(JSON.stringify(disk_manifest).includes('"git_tree":'), false);
    assert.equal(JSON.stringify(disk_manifest).includes('"head_commit":'), false);

    const no_git_audit = spawnSync(process.execPath, ['scripts/privacy/audit_repository.mjs'], {
      cwd: first,
      encoding: 'utf8',
      maxBuffer: 16 * 1024 * 1024,
      windowsHide: true,
    });
    assert.equal(no_git_audit.status, 0, no_git_audit.stdout);
    const report = JSON.parse(no_git_audit.stdout);
    assert.equal(report.mode, 'directory');
    assert.equal(report.summary.blockers, 0);
  } finally {
    rmSync(fixture.base, { recursive: true, force: true });
  }
});

test('refuses dirty default input, overwrite, in-tree output, and traversal', () => {
  const fixture = create_test_repository();
  try {
    writeFileSync(join(fixture.root, 'README.md'), '# Dirty fixture\n');
    assert.throws(
      () => export_snapshot({ repository_root: fixture.root, output: join(fixture.base, 'dirty_output') }),
      /privacy:dirty_source_tree/,
    );
    assert.throws(
      () => export_snapshot({ repository_root: fixture.root, output: join(fixture.root, 'inside') }),
      /privacy:output_inside_source_tree/,
    );
    mkdirSync(join(fixture.base, 'existing'));
    assert.throws(
      () => export_snapshot({ repository_root: fixture.root, output: join(fixture.base, 'existing') }),
      /privacy:output_exists/,
    );
    assert.throws(() => export_path_allowed('../escape'), /privacy:invalid_repository_path/);
  } finally {
    rmSync(fixture.base, { recursive: true, force: true });
  }
});

test('manifest snapshots stay directory-bound and reject Git directories and worktree pointer files', () => {
  const fixture = create_test_repository();
  const directory_snapshot = join(fixture.base, 'snapshot_with_git_directory');
  const file_snapshot = join(fixture.base, 'snapshot_with_git_file');
  try {
    export_snapshot({ repository_root: fixture.root, output: directory_snapshot });
    export_snapshot({ repository_root: fixture.root, output: file_snapshot });

    mkdirSync(join(directory_snapshot, '.git'));
    writeFileSync(join(directory_snapshot, '.git', 'config'), '[core]\nrepositoryformatversion = 0\n');
    const directory_report = audit_repository({
      repository_root: directory_snapshot,
      include_npm_pack: false,
    });
    assert.equal(directory_report.mode, 'directory');
    assert.ok(directory_report.findings.some(finding => finding.category === 'public_snapshot_contains_git_metadata'));

    writeFileSync(join(file_snapshot, '.git'), 'gitdir: ../private_git\n');
    const file_report = audit_repository({
      repository_root: file_snapshot,
      include_npm_pack: false,
    });
    assert.equal(file_report.mode, 'directory');
    assert.ok(file_report.findings.some(finding => finding.category === 'public_snapshot_contains_git_metadata'));
  } finally {
    rmSync(fixture.base, { recursive: true, force: true });
  }
});

test('an explicit full commit exports committed content despite unrelated dirty state', () => {
  const fixture = create_test_repository();
  try {
    const commit = run_git(fixture.root, ['rev-parse', 'HEAD']);
    writeFileSync(join(fixture.root, 'README.md'), '# Dirty fixture\n');
    const output = join(fixture.base, 'commit_snapshot');
    const manifest = export_snapshot({ repository_root: fixture.root, output, source_commit: commit });
    assert.equal(manifest.private_source.commit, commit);
    assert.equal(readFileSync(join(output, 'README.md'), 'utf8'), '# Public fixture\n');
    assert.throws(
      () => export_snapshot({ repository_root: fixture.root, output: join(fixture.base, 'short'), source_commit: commit.slice(0, 12) }),
      /privacy:source_commit_must_be_full_hash/,
    );
  } finally {
    rmSync(fixture.base, { recursive: true, force: true });
  }
});

test('refuses a source commit whose privacy tooling or policy differs from the running exporter', () => {
  const fixture = create_test_repository();
  try {
    writeFileSync(
      join(fixture.root, 'scripts', 'privacy', 'git_tree.mjs'),
      readFileSync(join(fixture.root, 'scripts', 'privacy', 'git_tree.mjs'), 'utf8') + '\n// changed source tool\n',
    );
    run_git(fixture.root, ['add', 'scripts/privacy/git_tree.mjs']);
    run_git(fixture.root, ['commit', '--quiet', '-m', 'Change source privacy tool']);
    const commit = run_git(fixture.root, ['rev-parse', 'HEAD']);
    assert.throws(
      () => export_snapshot({ repository_root: fixture.root, output: join(fixture.base, 'tool_mismatch'), source_commit: commit }),
      /privacy:source_tooling_mismatch/,
    );
    assert.equal(existsSync(join(fixture.base, 'tool_mismatch')), false);
  } finally {
    rmSync(fixture.base, { recursive: true, force: true });
  }
});

test('binds shared npm discovery to the source commit before export', () => {
  for (const dependency_state of ['modified', 'missing']) {
    const fixture = create_test_repository();
    try {
      const source_path = 'scripts/npm_discovery.mjs';
      if (dependency_state === 'modified') {
        const source_file = join(fixture.root, source_path);
        writeFileSync(source_file, readFileSync(source_file, 'utf8') + '\nvoid 0;\n');
        run_git(fixture.root, ['add', source_path]);
      } else {
        run_git(fixture.root, ['rm', '--', source_path]);
      }
      run_git(fixture.root, ['commit', '--quiet', '-m', 'Change source npm discovery']);
      const commit = run_git(fixture.root, ['rev-parse', 'HEAD']);
      const output = join(fixture.base, 'dependency_mismatch');
      const expected_error = dependency_state === 'modified'
        ? /privacy:source_tooling_mismatch/ : /privacy:source_tooling_missing/;
      assert.throws(
        () => export_snapshot({ repository_root: fixture.root, output, source_commit: commit }),
        expected_error,
      );
      assert.equal(existsSync(output), false);
    } finally {
      rmSync(fixture.base, { recursive: true, force: true });
    }
  }
});

test('refuses a tracked symbolic-link mode without materializing it', () => {
  const fixture = create_test_repository();
  try {
    const blob = run_git(fixture.root, ['hash-object', '-w', '--stdin'], 'README.md\n');
    run_git(fixture.root, ['update-index', '--add', '--cacheinfo', `120000,${blob},linked_file`]);
    run_git(fixture.root, ['commit', '--quiet', '-m', 'Add link fixture']);
    const commit = run_git(fixture.root, ['rev-parse', 'HEAD']);
    assert.throws(
      () => export_snapshot({ repository_root: fixture.root, output: join(fixture.base, 'linked'), source_commit: commit }),
      /privacy:privacy_audit_blocked/,
    );
    assert.equal(existsSync(join(fixture.base, 'linked')), false);
  } finally {
    rmSync(fixture.base, { recursive: true, force: true });
  }
});

test('refuses links to files excluded from the public snapshot', () => {
  const fixture = create_test_repository();
  try {
    writeFileSync(
      join(fixture.root, 'README.md'),
      '# Public fixture\n\n[Internal receipt](docs/reviews/mcp_dsh_workspace_receipt.json)\n',
    );
    run_git(fixture.root, ['add', 'README.md']);
    run_git(fixture.root, ['commit', '--quiet', '-m', 'Add excluded link fixture']);
    const commit = run_git(fixture.root, ['rev-parse', 'HEAD']);
    assert.throws(
      () => export_snapshot({ repository_root: fixture.root, output: join(fixture.base, 'broken_links'), source_commit: commit }),
      /privacy:export_broken_local_link/,
    );
    assert.equal(existsSync(join(fixture.base, 'broken_links')), false);
  } finally {
    rmSync(fixture.base, { recursive: true, force: true });
  }
});

test('verify_snapshot_links reports a missing reference-definition target once at the definition line', () => {
  const base = mkdtempSync(join(tmpdir(), 'aag_link_check_'));
  try {
    writeFileSync(join(base, 'README.md'), '# Readme\n\n[docs]: docs/missing.md\n');
    const findings = verify_snapshot_links(base, [{ path: 'README.md' }], {});
    assert.equal(findings.length, 1);
    assert.equal(findings[0].severity, 'blocker');
    assert.equal(findings[0].category, 'broken_local_link');
    assert.equal(findings[0].path, 'README.md');
    assert.equal(findings[0].line, 3);
    assert.match(findings[0].fingerprint, /^[a-f0-9]{20}$/);
  } finally {
    rmSync(base, { recursive: true, force: true });
  }
});

test('verify_snapshot_links accepts definitions targeting exported files and directories', () => {
  const base = mkdtempSync(join(tmpdir(), 'aag_link_check_'));
  try {
    mkdirSync(join(base, 'docs'), { recursive: true });
    writeFileSync(join(base, 'docs', 'guide.md'), '# Guide\n');
    writeFileSync(
      join(base, 'README.md'),
      '# Readme\n\n[guide]: <docs/guide.md> "Guide"\n[manual]: docs/\n',
    );
    const findings = verify_snapshot_links(base, [{ path: 'README.md' }, { path: 'docs/guide.md' }], {});
    assert.equal(findings.length, 0);
  } finally {
    rmSync(base, { recursive: true, force: true });
  }
});

test('verify_snapshot_links ignores external, mailto, and anchor-only definitions', () => {
  const base = mkdtempSync(join(tmpdir(), 'aag_link_check_'));
  try {
    writeFileSync(
      join(base, 'README.md'),
      '# Readme\n\n[site]: https://example.com/page.md "Site"\n' +
        "[mail]: mailto:someone@example.net 'Mail'\n" +
        '[anchor]: #section (Section)\n',
    );
    const findings = verify_snapshot_links(base, [{ path: 'README.md' }], {});
    assert.equal(findings.length, 0);
  } finally {
    rmSync(base, { recursive: true, force: true });
  }
});

test('verify_snapshot_links honors link_check_exempt_targets for definitions', () => {
  const base = mkdtempSync(join(tmpdir(), 'aag_link_check_'));
  try {
    writeFileSync(join(base, 'README.md'), '# Readme\n\n[archive]: docs/progress/archived.md\n');
    const findings = verify_snapshot_links(base, [{ path: 'README.md' }], {
      link_check_exempt_targets: ['docs/progress/archived.md'],
    });
    assert.equal(findings.length, 0);
  } finally {
    rmSync(base, { recursive: true, force: true });
  }
});

test('verify_snapshot_links stays silent for unresolvable traversal and backslash definitions', () => {
  const base = mkdtempSync(join(tmpdir(), 'aag_link_check_'));
  try {
    writeFileSync(
      join(base, 'README.md'),
      '# Readme\n\n[escape]: ../outside.md\n[windows]: docs\\internal.md\n',
    );
    const findings = verify_snapshot_links(base, [{ path: 'README.md' }], {});
    assert.equal(findings.length, 0);
  } finally {
    rmSync(base, { recursive: true, force: true });
  }
});
