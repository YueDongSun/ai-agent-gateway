import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, unlinkSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';
import test from 'node:test';
import { audit_repository, directory_mode_matches_manifest, scan_content } from '../../scripts/privacy/audit_repository.mjs';
import { create_test_repository, run_git } from './test_repository.mjs';

function adversarial_values() {
  const token = ['sk', 'A1b2C3d4E5f6G7h8I9j0K1l2'].join('-');
  const key = ['-----BEGIN', 'PRIVATE KEY-----'].join(' ');
  const jwt = [`eyJ${'A'.repeat(18)}`, 'B'.repeat(24), 'C'.repeat(24)].join('.');
  const authenticated_url = `https://${'account'}:${'credential'}@${'service'}.company.internal/path`;
  const email = ['person', 'company.internal'].join('@');
  const user_path = ['C:', 'Users', 'private_person', 'workspace'].join('\\');
  const private_address = [10, 23, 45, 67].join('.');
  const tailnet = ['private-node', 'private-mesh', 'ts', 'net'].join('.');
  return { token, key, jwt, authenticated_url, email, user_path, private_address, tailnet };
}

test('public protocol network ranges are not peer inventory but individual peers remain findings', () => {
  for (const range of ['100.64.0.0/10', 'fd7a:115c:a1e0::/48']) {
    assert.equal(scan_content('src/policy.py', Buffer.from(`address in ipaddress.ip_network("${range}")`)).findings.length, 0);
  }
  for (const peer of ['100.64.0.1', '100.64.0.0', '100.64.0.0/32', '100.64.1.0/24', 'fd7a:115c:a1e0::1', 'fd7a:115c:a1e0::/64']) {
    assert.ok(scan_content('src/policy.py', Buffer.from(peer)).findings.some(item => item.category === 'private_network_address'));
  }
});

test('detects adversarial secrets and PII without returning raw matches', () => {
  const values = adversarial_values();
  const text = Object.values(values).join('\n');
  const report = scan_content('fixture.txt', Buffer.from(text));
  const categories = new Set(report.findings.map(finding => finding.category));
  for (const expected of [
    'credential_token',
    'private_key',
    'jwt',
    'authenticated_url',
    'email_address',
    'user_path',
    'private_network_address',
    'tailnet_hostname',
  ]) assert.ok(categories.has(expected), expected);
  const encoded = JSON.stringify(report);
  for (const value of Object.values(values)) assert.equal(encoded.includes(value), false);
  assert.ok(report.findings.every(finding => /^[a-f0-9]{20}$/.test(finding.fingerprint)));
});

test('scans binary payloads while preserving redacted output', () => {
  const token = adversarial_values().token;
  const bytes = Buffer.concat([Buffer.from([0, 1, 2]), Buffer.from(token)]);
  const report = scan_content('fixture.bin', bytes);
  assert.equal(report.binary, true);
  assert.ok(report.findings.some(finding => finding.category === 'credential_token' && finding.line === 0));
  assert.equal(JSON.stringify(report).includes(token), false);
});

test('decodes UTF-16LE and UTF-16BE before scanning secret assignments', () => {
  const secret = ['Ab9', 'private', 'credential', '42!'].join('_');
  const text = 'webhook_secret = "' + secret + '"\n';
  const little_endian = Buffer.concat([Buffer.from([0xff, 0xfe]), Buffer.from(text, 'utf16le')]);
  const little_bytes = Buffer.from(text, 'utf16le');
  const big_body = Buffer.alloc(little_bytes.length);
  for (let index = 0; index < little_bytes.length; index += 2) {
    big_body[index] = little_bytes[index + 1];
    big_body[index + 1] = little_bytes[index];
  }
  const big_endian = Buffer.concat([Buffer.from([0xfe, 0xff]), big_body]);
  for (const bytes of [little_endian, big_endian]) {
    const report = scan_content('docs/encoded_input.txt', bytes);
    assert.equal(report.binary, false);
    assert.ok(report.findings.some(finding => finding.category === 'assigned_secret'));
    assert.equal(JSON.stringify(report).includes(secret), false);
  }
});

test('detects private IPv6, arbitrary machine paths, credential URIs, and common secret names', () => {
  const drive_path = ['Q:', 'private_mount', 'workspace'].join('\\');
  const unc_path = ['', '', 'internal_host', 'private_share', 'workspace'].join('\\');
  const wsl_path = ['/mnt', 'z', 'private_mount', 'workspace'].join('/');
  const ula = ['fd7a', '115c', 'a1e0', '', '1234'].join(':');
  const link_local = ['fe80', '', 'abcd'].join(':');
  const credential_uri = 'postgres://' + 'account' + ':' + 'private_credential_91!' + '@' + 'db' + '.company.internal/app';
  const secret = ['Private', 'Signing', 'Credential', '91!'].join('_');
  const report = scan_content('docs/adversarial_input.txt', Buffer.from([
    drive_path,
    unc_path,
    wsl_path,
    ula,
    link_local,
    credential_uri,
    'signing_key = "' + secret + '"',
  ].join('\n')));
  const categories = report.findings.map(finding => finding.category);
  assert.ok(categories.filter(category => category === 'absolute_machine_path').length >= 3);
  assert.ok(categories.filter(category => category === 'private_network_address').length >= 2);
  assert.ok(categories.includes('authenticated_url'));
  assert.ok(categories.includes('assigned_secret'));
  const encoded = JSON.stringify(report);
  for (const value of [drive_path, unc_path, wsl_path, ula, link_local, credential_uri, secret]) {
    assert.equal(encoded.includes(value), false);
  }
});

test('detects AWS and vendor-prefixed secret assignments without substring placeholder suppression', () => {
  const quoted_name = ['AWS', 'SECRET', 'ACCESS', 'KEY'].join('_');
  const vendor_name = ['ACME', 'PRIVATE', 'TOKEN'].join('_');
  const quoted_value = ['Aws', 'Private', 'Credential', '83!'].join('_');
  const unquoted_value = ['Aws', 'Environment', 'Credential', '47!'].join('_');
  const vendor_value = ['Vendor', 'Credential', '29!'].join('_');
  const text = [
    '"' + quoted_name + '": "' + quoted_value + '"',
    quoted_name + '=' + unquoted_value,
    '"' + vendor_name + '": "' + vendor_value + '"',
  ].join('\n');
  const report = scan_content('docs/vendor_credentials.txt', Buffer.from(text));
  assert.ok(report.findings.filter(finding => finding.category === 'assigned_secret').length >= 3);
  for (const value of [quoted_value, unquoted_value, vendor_value]) {
    assert.equal(JSON.stringify(report).includes(value), false);
  }

  const contestant_path = ['R:', 'contestant', 'workspace'].join('\\');
  const path_report = scan_content('docs/machine_path.txt', Buffer.from(contestant_path));
  assert.ok(path_report.findings.some(finding => finding.category === 'absolute_machine_path'));
});

test('accepts placeholders, Home and End labels, loopback, and version-shaped IP text', () => {
  const safe = [
    'C:\\Users\\<user>\\project',
    '/home/<user>/project',
    'person@example.com',
    'Home/End navigation keys',
    'version 10.2.3.4',
    '127.0.0.1',
    'api_key = "replace_me_with_your_key"',
    `https://${'user'}:${'fixture'}@${'service'}.test/path`,
  ].join('\n');
  assert.deepEqual(scan_content('safe.txt', Buffer.from(safe)).findings, []);
  const smoke_canary = 'const fixture = { TEST_TOKEN: "synthetic_mcp_secret" };\n';
  assert.deepEqual(scan_content('scripts/ui_smoke/canary.mjs', Buffer.from(smoke_canary)).findings, []);
});

test('audits the current tree, exact refs, and history with redacted findings', () => {
  const fixture = create_test_repository();
  try {
    assert.equal(audit_repository({ repository_root: fixture.root }).summary.blockers, 0);
    const secret = adversarial_values().token;
    writeFileSync(join(fixture.root, 'private_note.txt'), secret);
    const current = audit_repository({ repository_root: fixture.root, include_npm_pack: false });
    assert.equal(current.source.worktree_dirty, true);
    assert.match(current.source.index_sha256, /^[a-f0-9]{64}$/);
    assert.match(current.source.scanned_tree_sha256, /^[a-f0-9]{64}$/);
    assert.ok(current.findings.some(finding => finding.path === 'private_note.txt' && finding.category === 'credential_token'));
    assert.equal(JSON.stringify(current).includes(secret), false);
    assert.equal(audit_repository({ repository_root: fixture.root, reference: 'HEAD' }).summary.blockers, 0);

    run_git(fixture.root, ['add', 'private_note.txt']);
    run_git(fixture.root, ['commit', '--quiet', '-m', 'Historical fixture']);
    writeFileSync(join(fixture.root, 'private_note.txt'), 'sanitized\n');
    run_git(fixture.root, ['add', 'private_note.txt']);
    run_git(fixture.root, ['commit', '--quiet', '-m', 'Sanitize fixture']);
    const history = audit_repository({ repository_root: fixture.root, history: true });
    assert.ok(history.findings.some(finding => finding.category === 'credential_token'));
    assert.equal(JSON.stringify(history).includes(secret), false);
  } finally {
    rmSync(fixture.base, { recursive: true, force: true });
  }
});

test('requires a tracked public example and release-gate privacy sequencing', () => {
  const manifest = JSON.parse(readFileSync(new URL('../../package.json', import.meta.url), 'utf8'));
  assert.match(manifest.scripts['release:verify'], /npm run privacy:audit/);
  const fixture = create_test_repository();
  try {
    unlinkSync(join(fixture.root, '.env.example'));
    const report = audit_repository({ repository_root: fixture.root, include_npm_pack: false });
    assert.ok(report.findings.some(finding => finding.category === 'required_public_example_missing'));
  } finally {
    rmSync(fixture.base, { recursive: true, force: true });
  }
});

test('flags a force-tracked sensitive filename', () => {
  const fixture = create_test_repository();
  try {
    writeFileSync(join(fixture.root, '.npmrc'), 'registry=https://registry.npmjs.org/\n');
    run_git(fixture.root, ['add', '--force', '.npmrc']);
    const report = audit_repository({ repository_root: fixture.root, include_npm_pack: false });
    assert.ok(report.findings.some(finding => finding.category === 'sensitive_filename' && finding.path === '.npmrc'));
  } finally {
    rmSync(fixture.base, { recursive: true, force: true });
  }
});

test('history audit scans every path for a reused blob and every historical sensitive filename', () => {
  const fixture = create_test_repository();
  try {
    const secret = ['Private', 'History', 'Credential', '73!'].join('_');
    writeFileSync(join(fixture.root, 'src', 'fixture_secret.test.js'), 'api_key = "' + secret + '"\n');
    run_git(fixture.root, ['add', 'src/fixture_secret.test.js']);
    run_git(fixture.root, ['commit', '--quiet', '-m', 'Add test fixture blob']);
    const blob = run_git(fixture.root, ['rev-parse', 'HEAD:src/fixture_secret.test.js']);
    run_git(fixture.root, ['update-index', '--add', '--cacheinfo', '100644,' + blob + ',src/reused_secret.js']);
    const readme_blob = run_git(fixture.root, ['rev-parse', 'HEAD:README.md']);
    run_git(fixture.root, ['update-index', '--add', '--cacheinfo', '100644,' + readme_blob + ',.npmrc']);
    run_git(fixture.root, ['commit', '--quiet', '-m', 'Reuse blob under production and sensitive paths']);
    run_git(fixture.root, ['update-index', '--force-remove', '.npmrc', 'src/reused_secret.js']);
    run_git(fixture.root, ['commit', '--quiet', '-m', 'Remove historical paths']);

    const report = audit_repository({ repository_root: fixture.root, history: true });
    assert.ok(report.findings.some(finding => finding.category === 'assigned_secret' && finding.path === 'src/reused_secret.js'));
    assert.ok(report.findings.some(finding => finding.category === 'sensitive_filename' && finding.path === '.npmrc'));
    assert.equal(JSON.stringify(report).includes(secret), false);
  } finally {
    rmSync(fixture.base, { recursive: true, force: true });
  }
});

test('directory_mode_matches_manifest treats the executable bit as not applicable on win32', () => {
  assert.equal(directory_mode_matches_manifest('100644', '100644', 'win32'), 'not_applicable');
  assert.equal(directory_mode_matches_manifest('100644', '100755', 'win32'), 'not_applicable');
  assert.equal(directory_mode_matches_manifest('100755', '100644', 'win32'), 'not_applicable');
});

test('directory_mode_matches_manifest compares listed and observed modes off windows', () => {
  assert.equal(directory_mode_matches_manifest('100644', '100644', 'linux'), true);
  assert.equal(directory_mode_matches_manifest('100755', '100755', 'linux'), true);
  assert.equal(directory_mode_matches_manifest('100644', '100755', 'linux'), false);
  assert.equal(directory_mode_matches_manifest('100755', '100644', 'linux'), false);
});

test('audits a plain exported directory against its public snapshot manifest modes', () => {
  const base = mkdtempSync(join(tmpdir(), 'privacy_audit_fixture_'));
  try {
    const policy_bytes = Buffer.from(`${JSON.stringify({
      schema_version: 1,
      history_policy: 'history_free',
      package_surface: 'private',
      required_ignored_paths: [],
      required_public_examples: [],
      maximum_text_file_bytes: 1048576,
    }, null, 2)}\n`, 'utf8');
    const file_bytes = new Map([
      ['.npmignore', Buffer.from('node_modules\n', 'utf8')],
      ['NOTES.md', Buffer.from('Snapshot notes.\n', 'utf8')],
      ['README.md', Buffer.from('Fixture snapshot readme.\n', 'utf8')],
      ['package.json', Buffer.from(JSON.stringify({ private: true, files: ['dist'] }), 'utf8')],
      ['scripts/privacy/public_export_policy.json', policy_bytes],
    ]);
    const manifest_files = [...file_bytes].map(([path, bytes]) => ({
      path,
      mode: path === 'README.md' ? '100755' : '100644',
      bytes: bytes.length,
      sha256: createHash('sha256').update(bytes).digest('hex'),
    }));
    const tree_hasher = createHash('sha256');
    for (const file of [...manifest_files].sort((left, right) => left.path.localeCompare(right.path, 'en'))) {
      tree_hasher.update(file.mode + '\0' + file.path + '\0' + file.sha256 + '\0' + file.bytes + '\n');
    }
    mkdirSync(join(base, 'scripts', 'privacy'), { recursive: true });
    for (const [path, bytes] of file_bytes) writeFileSync(join(base, ...path.split('/')), bytes);
    writeFileSync(join(base, 'public_snapshot_manifest.json'), Buffer.from(`${JSON.stringify({
      schema_version: 1,
      snapshot_type: 'history_free_tracked_tree',
      policy_sha256: createHash('sha256').update(policy_bytes).digest('hex'),
      export_tree_sha256: tree_hasher.digest('hex'),
      files: manifest_files,
    }, null, 2)}\n`, 'utf8'));

    const report = audit_repository({ repository_root: base, directory: true, include_npm_pack: false });
    const mismatches = report.findings.filter(finding => finding.category === 'public_snapshot_mode_mismatch');
    const expected_mismatches = process.platform === 'win32' ? 0 : 1;
    assert.equal(report.summary.blockers, expected_mismatches);
    assert.equal(mismatches.length, expected_mismatches);
    if (expected_mismatches === 1) assert.equal(mismatches[0].path, 'README.md');
  } finally {
    rmSync(base, { recursive: true, force: true });
  }
});

function write_snapshot_directory_fixture(base, manifest_policy_sha256) {
  const policy_bytes = Buffer.from(`${JSON.stringify({
    schema_version: 1,
    history_policy: 'history_free',
    package_surface: 'private',
    required_ignored_paths: [],
    required_public_examples: [],
    maximum_text_file_bytes: 1048576,
  }, null, 2)}\n`, 'utf8');
  const file_bytes = new Map([
    ['.npmignore', Buffer.from('node_modules\n', 'utf8')],
    ['NOTES.md', Buffer.from('Snapshot notes.\n', 'utf8')],
    ['README.md', Buffer.from('Fixture snapshot readme.\n', 'utf8')],
    ['package.json', Buffer.from(JSON.stringify({ private: true, files: ['dist'] }), 'utf8')],
    ['scripts/privacy/public_export_policy.json', policy_bytes],
  ]);
  const manifest_files = [...file_bytes].map(([path, bytes]) => ({
    path,
    mode: '100644',
    bytes: bytes.length,
    sha256: createHash('sha256').update(bytes).digest('hex'),
  }));
  const tree_hasher = createHash('sha256');
  for (const file of [...manifest_files].sort((left, right) => left.path.localeCompare(right.path, 'en'))) {
    tree_hasher.update(file.mode + '\0' + file.path + '\0' + file.sha256 + '\0' + file.bytes + '\n');
  }
  mkdirSync(join(base, 'scripts', 'privacy'), { recursive: true });
  for (const [path, bytes] of file_bytes) writeFileSync(join(base, ...path.split('/')), bytes);
  const manifest = {
    schema_version: 1,
    snapshot_type: 'history_free_tracked_tree',
    export_tree_sha256: tree_hasher.digest('hex'),
    files: manifest_files,
  };
  if (manifest_policy_sha256 !== undefined) manifest.policy_sha256 = manifest_policy_sha256;
  writeFileSync(join(base, 'public_snapshot_manifest.json'), Buffer.from(`${JSON.stringify(manifest, null, 2)}\n`, 'utf8'));
}

test('flags a snapshot whose manifest policy_sha256 does not match the exported policy bytes', () => {
  const base = mkdtempSync(join(tmpdir(), 'privacy_audit_fixture_'));
  try {
    write_snapshot_directory_fixture(base, 'f'.repeat(64));
    const report = audit_repository({ repository_root: base, directory: true, include_npm_pack: false });
    const mismatches = report.findings.filter(finding => finding.category === 'public_snapshot_policy_mismatch');
    assert.equal(mismatches.length, 1);
    assert.equal(mismatches[0].path, 'scripts/privacy/public_export_policy.json');
    assert.equal(mismatches[0].line, 0);
    assert.equal(mismatches[0].severity, 'blocker');
    assert.equal(report.summary.blockers, 1);
  } finally {
    rmSync(base, { recursive: true, force: true });
  }
});

test('rejects a snapshot manifest without policy_sha256 as invalid without a policy mismatch', () => {
  const base = mkdtempSync(join(tmpdir(), 'privacy_audit_fixture_'));
  try {
    write_snapshot_directory_fixture(base, undefined);
    const report = audit_repository({ repository_root: base, directory: true, include_npm_pack: false });
    assert.ok(report.findings.some(finding => finding.category === 'public_snapshot_manifest_invalid'));
    assert.equal(report.findings.some(finding => finding.category === 'public_snapshot_policy_mismatch'), false);
  } finally {
    rmSync(base, { recursive: true, force: true });
  }
});
