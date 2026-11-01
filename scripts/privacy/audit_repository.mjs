import { createHash } from 'node:crypto';
import { existsSync, lstatSync, readFileSync, readdirSync, realpathSync } from 'node:fs';
import { isIP } from 'node:net';
import { basename, isAbsolute, join, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';
import { find_npm_cli } from '../npm_discovery.mjs';
import {
  find_repository_root,
  list_commit_entries,
  list_index_entries,
  list_worktree_entries,
  normalize_repository_path,
  privacy_error,
  read_commit_entry,
  read_index_entry,
  repository_is_clean,
  resolve_commit,
  run_git,
  source_tree_oid,
} from './git_tree.mjs';

const policy_path = fileURLToPath(new URL('./public_export_policy.json', import.meta.url));
const default_policy = JSON.parse(readFileSync(policy_path, 'utf8'));
const binary_probe_bytes = 8192;
const example_domains = new Set(['example.com', 'example.org', 'example.net', 'invalid']);
const placeholder_words = /(?:^|[^A-Za-z0-9])(?:demo|example|fixture|placeholder|dummy|sample|sentinel|redacted|replace(?:[_-]?me)?|your(?:[_-][A-Za-z0-9]+)*|test|not[_-]?a[_-]?real)(?=$|[^A-Za-z0-9])/i;
const placeholder_path_users = new Set(['example', 'fixture', 'linuxbrew', 'me', 'public', 'root', 'runner', 'test', 'user', 'username']);
const sensitive_basenames = new Set([
  '.env',
  '.git-credentials',
  '.netrc',
  '.npmrc',
  '.pypirc',
  '.claude.json',
  'credentials.json',
  'private.json',
  'secrets.json',
  'service_account.json',
  'terraform.tfvars',
]);
const sensitive_extensions = new Set([
  '.auto.tfvars',
  '.db',
  '.jks',
  '.kdbx',
  '.key',
  '.keystore',
  '.mobileprovision',
  '.ovpn',
  '.p12',
  '.pfx',
  '.pem',
  '.sqlite',
  '.sqlite3',
  '.tfstate',
]);

function stable_fingerprint(category, path, line) {
  return createHash('sha256').update(`aag_privacy_finding_v1\0${category}\0${path}\0${line}`).digest('hex').slice(0, 20);
}

function line_number(text, offset, binary) {
  if (binary) return 0;
  let line = 1;
  for (let index = 0; index < offset; index += 1) if (text.charCodeAt(index) === 10) line += 1;
  return line;
}

function add_finding(findings, seen, category, path, line, severity = 'blocker') {
  const key = `${severity}\0${category}\0${path}\0${line}`;
  if (seen.has(key)) return;
  seen.add(key);
  findings.push({ severity, category, path, line, fingerprint: stable_fingerprint(category, path, line) });
}

function placeholder_value(value) {
  return placeholder_words.test(value) || /^\$\{?[A-Z][A-Z0-9_]*\}?$/.test(value) || /^<[^>]+>$/.test(value);
}

function scan_regex(text, pattern, category, path, binary, findings, seen, predicate = () => true) {
  pattern.lastIndex = 0;
  for (const match of text.matchAll(pattern)) {
    if (!predicate(match, text)) continue;
    add_finding(findings, seen, category, path, line_number(text, match.index, binary));
  }
}

function is_private_network(value) {
  const octets = value.split('.').map(Number);
  if (octets.length !== 4 || octets.some(octet => !Number.isInteger(octet) || octet < 0 || octet > 255)) return false;
  return octets[0] === 10
    || (octets[0] === 172 && octets[1] >= 16 && octets[1] <= 31)
    || (octets[0] === 192 && octets[1] === 168)
    || (octets[0] === 100 && octets[1] >= 64 && octets[1] <= 127);
}

function is_private_ipv6(value) {
  const address = value.replace(/^\[|\]$/g, '').split('%', 1)[0];
  if (isIP(address) !== 6) return false;
  const first_hextet = Number.parseInt(address.split(':', 1)[0], 16);
  if (!Number.isInteger(first_hextet)) return false;
  return (first_hextet & 0xfe00) === 0xfc00 || (first_hextet & 0xffc0) === 0xfe80;
}

function is_version_context(text, offset) {
  return /(?:\bversion|\bver|\bv)\s*=?\s*$/i.test(text.slice(Math.max(0, offset - 18), offset));
}

function strict_secret_value(value) {
  if (placeholder_value(value) || value.length < 12 || value.length > 512) return false;
  if (/^\s*(?:\+|\.|\$\{)/.test(value)) return false;
  if (/^(?:process\.env\.|env\.|config\.)[A-Z0-9_.-]+$/i.test(value)) return false;
  if (/^(?:\$\{?\w+\}?|%\w+%|[A-Z][A-Z0-9_]{2,})$/.test(value)) return false;
  const classes = [/[a-z]/.test(value), /[A-Z]/.test(value), /[0-9]/.test(value), /[^A-Za-z0-9]/.test(value)]
    .filter(Boolean).length;
  return classes >= 2 && new Set(value).size >= 7;
}

function test_context(path, text, offset) {
  if (/^(?:tests?|scripts\/ui_smoke|scripts\/[^/]+\/.*\.test)\//i.test(path)
      || /(?:^|\/)(?:tests?|test_[^/]+|[^/]+\.test|[^/]+_tests?)\.[^/]+$/i.test(path)
      || /(?:^|\/)tests\.rs$/i.test(path)) return true;
  return path.endsWith('.rs') && text.lastIndexOf('#[cfg(test)]', offset) >= 0;
}

function synthetic_test_context(path, text, offset) {
  if (!test_context(path, text, offset)) return false;
  return /(?:canary|fixture|private|redact|sanitize|sentinel|test)/i.test(
    text.slice(Math.max(0, offset - 240), Math.min(text.length, offset + 240)),
  );
}

// These complete, publicly documented protocol ranges are not host inventory.
// Bare peer addresses and narrower allocations still undergo the normal scan.
const public_protocol_ranges = ['100.64.0.0/10', 'fd7a:115c:a1e0::/48'];
function public_protocol_range_context(text, offset) {
  return public_protocol_ranges.some(range => text.startsWith(range, offset)
    && !/[A-Za-z0-9.:/]/.test(text[offset + range.length] ?? ''));
}

function authenticated_url_is_sensitive(value) {
  try {
    const hostname = new URL(value).hostname.toLowerCase();
    return !['example.com', 'example.org', 'example.net'].includes(hostname)
      && !hostname.endsWith('.example')
      && !hostname.endsWith('.invalid')
      && !hostname.endsWith('.test');
  } catch {
    return true;
  }
}

function machine_path_is_sensitive(value) {
  if (placeholder_value(value)) return false;
  if (/[\[\]{}^$]|\(\?:/.test(value)) return false;
  if (/^[A-Za-z]:[\\/]+(?:Program Files(?: \(x86\))?|Windows)[\\/]/i.test(value)) return false;
  const normalized = value.replace(/\\/g, '/').replace(/\/+/g, '/');
  if (/^[A-Za-z]:\/(?:Program Files(?: \(x86\))?|Windows)(?:\/|$)/i.test(normalized)) return false;
  return !normalized.split('/').some(part => placeholder_value(part) || placeholder_path_users.has(part.toLowerCase()));
}

function decode_utf16_be(bytes) {
  const even_length = bytes.length - (bytes.length % 2);
  const swapped = Buffer.allocUnsafe(even_length);
  for (let index = 0; index < even_length; index += 2) {
    swapped[index] = bytes[index + 1];
    swapped[index + 1] = bytes[index];
  }
  return swapped.toString('utf16le');
}

function probable_utf16(bytes, little_endian) {
  const sample_length = Math.min(bytes.length - (bytes.length % 2), binary_probe_bytes);
  if (sample_length < 8) return false;
  let expected_nulls = 0;
  let unexpected_nulls = 0;
  let pairs = 0;
  for (let index = 0; index < sample_length; index += 2) {
    pairs += 1;
    const expected_index = little_endian ? index + 1 : index;
    const unexpected_index = little_endian ? index : index + 1;
    if (bytes[expected_index] === 0) expected_nulls += 1;
    if (bytes[unexpected_index] === 0) unexpected_nulls += 1;
  }
  return expected_nulls / pairs >= 0.55 && unexpected_nulls / pairs <= 0.10;
}

function decode_scan_bytes(bytes) {
  if (bytes.length >= 2 && bytes[0] === 0xff && bytes[1] === 0xfe) {
    return { binary: false, text: bytes.subarray(2).toString('utf16le') };
  }
  if (bytes.length >= 2 && bytes[0] === 0xfe && bytes[1] === 0xff) {
    return { binary: false, text: decode_utf16_be(bytes.subarray(2)) };
  }
  if (probable_utf16(bytes, true)) return { binary: false, text: bytes.toString('utf16le') };
  if (probable_utf16(bytes, false)) return { binary: false, text: decode_utf16_be(bytes) };
  const binary = bytes.subarray(0, binary_probe_bytes).includes(0);
  return { binary, text: bytes.toString(binary ? 'latin1' : 'utf8') };
}

function email_domain_is_sensitive(value) {
  const domain = value.toLowerCase();
  return !example_domains.has(domain)
    && !domain.endsWith('.example')
    && !domain.endsWith('.invalid')
    && !domain.endsWith('.test');
}

function image_asset_email_false_positive(value) {
  return /^\d+x\d+@\d+x\.(?:gif|ico|jpe?g|png|svg|webp)$/i.test(value);
}

export function scan_content(path_value, bytes, options = {}) {
  const path = normalize_repository_path(path_value);
  const findings = [];
  const seen = new Set();
  const { binary, text } = decode_scan_bytes(bytes);

  scan_regex(text, /-----BEGIN (?:OPENSSH |RSA |EC |DSA |PGP )?PRIVATE KEY-----/g, 'private_key', path, binary, findings, seen);
  scan_regex(text, /\b(?:sk-(?:ant-)?[A-Za-z0-9_-]{20,}|gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{40,}|AKIA[A-Z0-9]{16}|AIza[A-Za-z0-9_-]{30,}|xox[baprs]-[A-Za-z0-9-]{20,})\b/g, 'credential_token', path, binary, findings, seen, match => !placeholder_value(match[0]));
  scan_regex(text, /\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\b/g, 'jwt', path, binary, findings, seen, match => !placeholder_value(match[0]));
  scan_regex(text, /\bBearer\s+([A-Za-z0-9._~+/-]{20,}=*)/gi, 'bearer_token', path, binary, findings, seen, match => strict_secret_value(match[1]) && !synthetic_test_context(path, text, match.index));
  scan_regex(text, /\b(?:[a-z0-9]+[_-])*(?:api[_-]?(?:key|secret|token)|auth[_-]?token|access[_-]?(?:key|token)|client[_-]?(?:secret|token)|connection[_-]?string|credential|database[_-]?url|pass(?:word|phrase|wd)?|private[_-]?key|secret[_-]?access[_-]?key|secret(?:[_-]?key)?|signing[_-]?key|token|webhook[_-]?secret)\b\s*["']?\s*[:=]\s*(?:[rubf]{0,2})?(["'])([^"'\r\n]{12,})\1/gi, 'assigned_secret', path, binary, findings, seen, match => strict_secret_value(match[2]) && !synthetic_test_context(path, text, match.index));
  scan_regex(text, /^\s*(?:export\s+)?(?:[A-Z0-9]+_)*(?:API_KEY|API_SECRET|AUTH_TOKEN|ACCESS_KEY|ACCESS_TOKEN|CLIENT_SECRET|CONNECTION_STRING|CREDENTIAL|DATABASE_URL|PASSWORD|PASSPHRASE|PRIVATE_KEY|SECRET_ACCESS_KEY|SECRET|SECRET_KEY|SIGNING_KEY|TOKEN|WEBHOOK_SECRET)\s*=\s*([^\s#;]{12,})/gm, 'assigned_secret', path, binary, findings, seen, match => strict_secret_value(match[1]) && !synthetic_test_context(path, text, match.index));
  scan_regex(text, /\b[a-z][a-z0-9+.-]{1,31}:\/\/[^\s/@:"'<>]+:[^\s/@"'<>]+@[^\s/"'<>]+(?:\/[^\s"'<>]*)?/gi, 'authenticated_url', path, binary, findings, seen, match => authenticated_url_is_sensitive(match[0]) && !synthetic_test_context(path, text, match.index));
  scan_regex(text, /\b([A-Z0-9._%+-]+)@([A-Z0-9.-]+\.[A-Z]{2,})\b/gi, 'email_address', path, binary, findings, seen, match => /[A-Za-z]/.test(match[1]) && email_domain_is_sensitive(match[2]) && !image_asset_email_false_positive(match[0]) && !synthetic_test_context(path, text, match.index));
  scan_regex(text, /\b[A-Za-z]:[\\/]Users[\\/]([^\\/\s<>"']+)[\\/][^\r\n"']*/g, 'user_path', path, binary, findings, seen, match => !placeholder_value(match[1]) && !placeholder_path_users.has(match[1].toLowerCase()) && !synthetic_test_context(path, text, match.index));
  scan_regex(text, /(?<![A-Za-z0-9_])([A-Za-z]:[\\/](?![\\/])[^\r\n"'|]*)/g, 'absolute_machine_path', path, binary, findings, seen, match => machine_path_is_sensitive(match[1]) && !synthetic_test_context(path, text, match.index));
  scan_regex(text, /(?<![^\s"'(=\[{};,])((?:\\\\\\\\|\\\\|\/\/)[A-Za-z0-9._-]+[\\/][^\r\n"'<>|\s,}\]]{1,1024})/g, 'absolute_machine_path', path, binary, findings, seen, match => !/(?:^|\/)(?:package-lock|npm-shrinkwrap)\.json$/i.test(path) && machine_path_is_sensitive(match[1]) && !synthetic_test_context(path, text, match.index));
  scan_regex(text, /(?<![A-Za-z0-9_])(\/mnt\/[a-z]\/[^\r\n"'<>|\s][^\r\n"'<>|]*)/gi, 'absolute_machine_path', path, binary, findings, seen, match => machine_path_is_sensitive(match[1]) && !synthetic_test_context(path, text, match.index));
  scan_regex(text, /\/(?:Users|home)\/([^/\s<>"']+)\/[^\r\n"']*/g, 'user_path', path, binary, findings, seen, match => !placeholder_value(match[1]) && !placeholder_path_users.has(match[1].toLowerCase()) && !synthetic_test_context(path, text, match.index));
  scan_regex(text, /\b\d{1,3}(?:\.\d{1,3}){3}\b/g, 'private_network_address', path, binary, findings, seen, match => is_private_network(match[0]) && !is_version_context(text, match.index) && !public_protocol_range_context(text, match.index) && !synthetic_test_context(path, text, match.index));
  scan_regex(text, /(?<![A-Za-z0-9])\[?(?:[A-Fa-f0-9]{0,4}:){2,7}[A-Fa-f0-9]{0,4}\]?(?![A-Za-z0-9])/g, 'private_network_address', path, binary, findings, seen, match => is_private_ipv6(match[0]) && !public_protocol_range_context(text, match.index) && !synthetic_test_context(path, text, match.index));
  scan_regex(text, /\b(?:[a-z0-9-]+\.)+[a-z0-9-]+\.ts\.net\b/gi, 'tailnet_hostname', path, binary, findings, seen, match => !placeholder_value(match[0]) && !synthetic_test_context(path, text, match.index));
  return { binary, findings };
}

function sensitive_path_category(path) {
  const lower = path.toLowerCase();
  const name = basename(lower);
  if (name.startsWith('.env.') && !name.endsWith('.example')) return 'sensitive_filename';
  if (lower.endsWith('/.aws/credentials') || lower === '.aws/credentials'
      || lower.endsWith('/.docker/config.json') || lower === '.docker/config.json') return 'sensitive_filename';
  if (lower === '.claude/settings.local.json' || lower === '.codex/config.toml') return 'sensitive_filename';
  if (sensitive_basenames.has(name)) return 'sensitive_filename';
  if (name === 'id_rsa' || name === 'id_ed25519' || name === 'id_ecdsa') return 'sensitive_filename';
  for (const extension of sensitive_extensions) if (lower.endsWith(extension)) return 'sensitive_filename';
  return null;
}

export function read_export_policy_from_entries(entries, read_entry) {
  const entry = entries.find(candidate => candidate.path === 'scripts/privacy/public_export_policy.json');
  if (!entry) throw new privacy_error('export_policy_missing');
  try {
    const policy = JSON.parse(read_entry(entry).toString('utf8'));
    if (policy.schema_version !== 1 || policy.history_policy !== 'history_free' || policy.package_surface !== 'private') throw new Error();
    return policy;
  } catch (error) {
    if (error instanceof privacy_error) throw error;
    throw new privacy_error('export_policy_invalid');
  }
}

function evaluate_generated_assets(entries, read_entry, policy, findings, seen) {
  for (const [path, limit] of Object.entries(policy.generated_asset_limits ?? {})) {
    const entry = entries.find(candidate => candidate.path === path);
    if (!entry) {
      add_finding(findings, seen, 'generated_asset_missing', path, 0);
      continue;
    }
    if (read_entry(entry).length > limit) add_finding(findings, seen, 'generated_asset_oversized', path, 0);
  }
}

function evaluate_package_metadata(entries, read_entry, findings, seen) {
  const entry = entries.find(candidate => candidate.path === 'package.json');
  if (!entry) return add_finding(findings, seen, 'package_manifest_missing', 'package.json', 0);
  try {
    const manifest = JSON.parse(read_entry(entry).toString('utf8'));
    if (manifest.private !== true) add_finding(findings, seen, 'npm_package_not_private', 'package.json', 1);
    if (!Array.isArray(manifest.files) || !manifest.files.includes('dist')) add_finding(findings, seen, 'npm_files_allowlist_missing', 'package.json', 1);
  } catch {
    add_finding(findings, seen, 'package_manifest_invalid', 'package.json', 1);
  }
  if (!entries.some(candidate => candidate.path === '.npmignore')) add_finding(findings, seen, 'npm_ignore_missing', '.npmignore', 0);
}

export function verify_npm_pack_manifest(repository_root, policy = default_policy) {
  const npm_cli = find_npm_cli();
  if (!npm_cli) throw new privacy_error('npm_pack_unavailable');
  const result = spawnSync(process.execPath, [npm_cli, 'pack', '--dry-run', '--json', '--ignore-scripts'], {
    cwd: repository_root,
    encoding: 'utf8',
    maxBuffer: 64 * 1024 * 1024,
    windowsHide: true,
  });
  if (result.error || result.status !== 0) throw new privacy_error('npm_pack_unavailable');
  let data;
  try { data = JSON.parse(result.stdout); } catch { throw new privacy_error('npm_pack_manifest_invalid'); }
  const files = data?.[0]?.files?.map(file => normalize_repository_path(file.path)).sort() ?? [];
  if (files.length === 0) throw new privacy_error('npm_pack_manifest_empty');
  const allowed = policy.npm_allowed_paths ?? [];
  const forbidden = files.filter(path => !allowed.some(rule => rule.endsWith('/') ? path.startsWith(rule) : path === rule));
  return { file_count: files.length, forbidden };
}

function evaluate_ignore_policy(repository_root, policy, entries, findings, seen) {
  const public_probes = [...policy.required_public_examples, ...(policy.public_example_probes ?? [])];
  const probes = [...policy.required_ignored_paths, ...public_probes];
  const input = `${probes.join('\n')}\n`;
  const result = spawnSync('git', ['-C', repository_root, 'check-ignore', '--stdin'], {
    input,
    encoding: 'utf8',
    maxBuffer: 1024 * 1024,
    windowsHide: true,
  });
  if (result.error || ![0, 1].includes(result.status)) {
    add_finding(findings, seen, 'ignore_policy_unverifiable', '.gitignore', 0);
    return;
  }
  const ignored = new Set(result.stdout.split(/\r?\n/).filter(Boolean));
  for (const path of policy.required_ignored_paths) if (!ignored.has(path)) add_finding(findings, seen, 'sensitive_path_not_ignored', '.gitignore', 0);
  const entry_paths = new Set(entries.map(entry => entry.path));
  for (const path of policy.required_public_examples) {
    if (!entry_paths.has(path)) add_finding(findings, seen, 'required_public_example_missing', path, 0);
  }
  for (const path of public_probes) if (ignored.has(path)) add_finding(findings, seen, 'public_example_ignored', '.gitignore', 0);
}

function list_directory_entries(directory_root) {
  const entries = [];
  let contains_git_metadata = false;
  function walk(absolute_directory, prefix = '') {
    const items = readdirSync(absolute_directory, { withFileTypes: true })
      .sort((left, right) => left.name.localeCompare(right.name, 'en'));
    for (const item of items) {
      const path = normalize_repository_path(prefix ? prefix + '/' + item.name : item.name);
      const absolute_path = resolve(directory_root, ...path.split('/'));
      const relation = relative(directory_root, absolute_path);
      if (relation === '..' || relation.startsWith('..' + sep) || isAbsolute(relation)) {
        throw new privacy_error('directory_path_escape');
      }
      if (path === '.git' || path.startsWith('.git/')) {
        contains_git_metadata = true;
        continue;
      }
      const stat = lstatSync(absolute_path);
      if (stat.isSymbolicLink()) {
        entries.push({ mode: '120000', path });
      } else if (stat.isDirectory()) {
        walk(absolute_path, path);
      } else if (stat.isFile()) {
        entries.push({ mode: stat.mode & 0o111 ? '100755' : '100644', path });
      } else {
        entries.push({ mode: 'unsupported', path });
      }
    }
  }
  walk(directory_root);
  return {
    contains_git_metadata,
    entries: entries.sort((left, right) => left.path.localeCompare(right.path, 'en')),
  };
}

function read_directory_entry(directory_root, entry) {
  const target = resolve(directory_root, ...entry.path.split('/'));
  const relation = relative(directory_root, target);
  if (relation === '..' || relation.startsWith('..' + sep) || isAbsolute(relation)) {
    throw new privacy_error('directory_path_escape');
  }
  const stat = lstatSync(target);
  if (entry.mode === '120000' || stat.isSymbolicLink()) throw new privacy_error('tracked_symlink');
  if (!stat.isFile()) throw new privacy_error('tracked_entry_not_file');
  return readFileSync(target);
}

function evaluate_directory_policy(policy, entries, findings, seen) {
  const entry_paths = new Set(entries.map(entry => entry.path));
  for (const path of policy.required_public_examples) {
    if (!entry_paths.has(path)) add_finding(findings, seen, 'required_public_example_missing', path, 0);
  }
}

function public_manifest_has_private_provenance(value) {
  if (!value || typeof value !== 'object') return false;
  for (const [key, child] of Object.entries(value)) {
    if (['commit', 'commit_time', 'git_tree', 'head_commit', 'head_tree', 'source', 'private_source'].includes(key)) return true;
    if (public_manifest_has_private_provenance(child)) return true;
  }
  return false;
}

export function directory_mode_matches_manifest(observed_mode, listed_mode, platform = process.platform) {
  if (platform === 'win32') return 'not_applicable';
  return observed_mode === listed_mode;
}

function evaluate_public_snapshot_manifest(entries, read_entry, findings, seen) {
  const manifest_entry = entries.find(entry => entry.path === 'public_snapshot_manifest.json');
  if (!manifest_entry) {
    add_finding(findings, seen, 'public_snapshot_manifest_missing', 'public_snapshot_manifest.json', 0);
    return;
  }
  let manifest;
  try {
    manifest = JSON.parse(read_entry(manifest_entry).toString('utf8'));
    if (manifest.schema_version !== 1
        || manifest.snapshot_type !== 'history_free_tracked_tree'
        || !Array.isArray(manifest.files)
        || typeof manifest.policy_sha256 !== 'string'
        || !/^[a-f0-9]{64}$/.test(manifest.policy_sha256)
        || public_manifest_has_private_provenance(manifest)) throw new Error();
  } catch {
    add_finding(findings, seen, 'public_snapshot_manifest_invalid', 'public_snapshot_manifest.json', 1);
    return;
  }
  const expected = new Map();
  try {
    for (const file of manifest.files) {
      const path = normalize_repository_path(file.path);
      if (path === 'public_snapshot_manifest.json'
          || expected.has(path)
          || !['100644', '100755'].includes(file.mode)
          || !Number.isSafeInteger(file.bytes)
          || file.bytes < 0
          || !/^[a-f0-9]{64}$/.test(file.sha256)) throw new Error();
      expected.set(path, file);
    }
  } catch {
    add_finding(findings, seen, 'public_snapshot_manifest_invalid', 'public_snapshot_manifest.json', 1);
    return;
  }
  const actual = new Map(entries
    .filter(entry => entry.path !== 'public_snapshot_manifest.json')
    .map(entry => [entry.path, entry]));
  for (const [path, file] of expected) {
    const entry = actual.get(path);
    if (!entry) {
      add_finding(findings, seen, 'public_snapshot_file_missing', path, 0);
      continue;
    }
    try {
      const bytes = read_entry(entry);
      if (bytes.length !== file.bytes || createHash('sha256').update(bytes).digest('hex') !== file.sha256) {
        add_finding(findings, seen, 'public_snapshot_file_mismatch', path, 0);
      }
    } catch {
      add_finding(findings, seen, 'public_snapshot_file_unreadable', path, 0);
    }
    if (directory_mode_matches_manifest(entry.mode, file.mode) === false) {
      add_finding(findings, seen, 'public_snapshot_mode_mismatch', path, 0);
    }
  }
  for (const path of actual.keys()) {
    if (!expected.has(path)) add_finding(findings, seen, 'public_snapshot_unmanifested_file', path, 0);
  }
  const policy_entry = actual.get('scripts/privacy/public_export_policy.json');
  if (policy_entry) {
    try {
      const policy_bytes = read_entry(policy_entry);
      if (createHash('sha256').update(policy_bytes).digest('hex') !== manifest.policy_sha256) {
        add_finding(findings, seen, 'public_snapshot_policy_mismatch', 'scripts/privacy/public_export_policy.json', 0);
      }
    } catch {
      // Unreadable policy entries already surface as public_snapshot_file_unreadable above.
    }
  }
  const tree_hasher = createHash('sha256');
  for (const file of [...expected.values()].sort((left, right) => left.path.localeCompare(right.path, 'en'))) {
    tree_hasher.update(file.mode + '\0' + file.path + '\0' + file.sha256 + '\0' + file.bytes + '\n');
  }
  if (tree_hasher.digest('hex') !== manifest.export_tree_sha256) {
    add_finding(findings, seen, 'public_snapshot_tree_mismatch', 'public_snapshot_manifest.json', 1);
  }
}

function scan_entries(entries, read_entry, policy) {
  const findings = [];
  const seen = new Set();
  let bytes_scanned = 0;
  let binary_files = 0;
  const tree_hasher = createHash('sha256');
  for (const entry of entries) {
    const sensitive = sensitive_path_category(entry.path);
    if (sensitive) add_finding(findings, seen, sensitive, entry.path, 0);
    let bytes;
    try { bytes = read_entry(entry); } catch (error) {
      add_finding(findings, seen, error.code === 'tracked_symlink' ? 'tracked_symlink' : 'tracked_file_unreadable', entry.path, 0);
      tree_hasher.update(`${entry.mode}\0${entry.path}\0unreadable\n`);
      continue;
    }
    bytes_scanned += bytes.length;
    tree_hasher.update(`${entry.mode}\0${entry.path}\0${createHash('sha256').update(bytes).digest('hex')}\0${bytes.length}\n`);
    if (bytes.length > policy.maximum_text_file_bytes) {
      add_finding(findings, seen, 'tracked_file_oversized', entry.path, 0);
      continue;
    }
    const result = scan_content(entry.path, bytes);
    if (result.binary) binary_files += 1;
    for (const finding of result.findings) add_finding(findings, seen, finding.category, finding.path, finding.line, finding.severity);
  }
  evaluate_generated_assets(entries, read_entry, policy, findings, seen);
  evaluate_package_metadata(entries, read_entry, findings, seen);
  return { findings, bytes_scanned, binary_files, scanned_tree_sha256: tree_hasher.digest('hex') };
}

function sorted_findings(findings) {
  return findings.sort((left, right) => left.severity.localeCompare(right.severity, 'en')
    || left.category.localeCompare(right.category, 'en')
    || left.path.localeCompare(right.path, 'en')
    || left.line - right.line
    || left.fingerprint.localeCompare(right.fingerprint, 'en'));
}

function finalize_report(mode, source, stats, findings) {
  const ordered = sorted_findings(findings);
  return {
    schema_version: 1,
    mode,
    source,
    summary: {
      files_scanned: stats.files_scanned,
      binary_files: stats.binary_files,
      bytes_scanned: stats.bytes_scanned,
      blockers: ordered.filter(finding => finding.severity === 'blocker').length,
      warnings: ordered.filter(finding => finding.severity === 'warning').length,
      findings: ordered.length,
    },
    findings: ordered,
  };
}

function audit_current(repository_root, options) {
  const entries = list_worktree_entries(repository_root);
  const index_entries = list_index_entries(repository_root);
  const read_entry = entry => read_index_entry(repository_root, entry);
  const policy = read_export_policy_from_entries(entries, read_entry);
  const scanned = scan_entries(entries, read_entry, policy);
  const seen = new Set(scanned.findings.map(item => `${item.severity}\0${item.category}\0${item.path}\0${item.line}`));
  evaluate_ignore_policy(repository_root, policy, entries, scanned.findings, seen);
  if (options.include_npm_pack !== false) {
    try {
      const pack = verify_npm_pack_manifest(repository_root, policy);
      for (const path of pack.forbidden) add_finding(scanned.findings, seen, 'npm_pack_forbidden_path', path, 0);
    } catch (error) {
      add_finding(scanned.findings, seen, error.code ?? 'npm_pack_unverifiable', 'package.json', 0);
    }
  }
  const commit = resolve_commit(repository_root);
  const index_hasher = createHash('sha256');
  for (const entry of index_entries) index_hasher.update(`${entry.mode}\0${entry.oid}\0${entry.stage}\0${entry.path}\n`);
  return finalize_report('current', {
    head_commit: commit,
    head_tree: source_tree_oid(repository_root, commit),
    worktree_dirty: !repository_is_clean(repository_root),
    index_sha256: index_hasher.digest('hex'),
    scanned_tree_sha256: scanned.scanned_tree_sha256,
  }, {
    files_scanned: entries.length,
    binary_files: scanned.binary_files,
    bytes_scanned: scanned.bytes_scanned,
  }, scanned.findings);
}

function audit_ref(repository_root, reference) {
  const commit = resolve_commit(repository_root, reference);
  const entries = list_commit_entries(repository_root, commit);
  const read_entry = entry => read_commit_entry(repository_root, entry);
  const policy = read_export_policy_from_entries(entries, read_entry);
  const scanned = scan_entries(entries, read_entry, policy);
  return finalize_report('ref', { commit, tree: source_tree_oid(repository_root, commit) }, {
    files_scanned: entries.length,
    binary_files: scanned.binary_files,
    bytes_scanned: scanned.bytes_scanned,
  }, scanned.findings);
}

function audit_directory(directory_root, options) {
  const root = realpathSync(resolve(directory_root));
  const listed = list_directory_entries(root);
  const entries = listed.entries;
  const read_entry = entry => read_directory_entry(root, entry);
  const policy = read_export_policy_from_entries(entries, read_entry);
  const scanned = scan_entries(entries, read_entry, policy);
  const seen = new Set(scanned.findings.map(item => item.severity + '\0' + item.category + '\0' + item.path + '\0' + item.line));
  evaluate_directory_policy(policy, entries, scanned.findings, seen);
  evaluate_public_snapshot_manifest(entries, read_entry, scanned.findings, seen);
  if (listed.contains_git_metadata) {
    add_finding(scanned.findings, seen, 'public_snapshot_contains_git_metadata', '.git', 0);
  }
  if (options.include_npm_pack !== false) {
    try {
      const pack = verify_npm_pack_manifest(root, policy);
      for (const path of pack.forbidden) add_finding(scanned.findings, seen, 'npm_pack_forbidden_path', path, 0);
    } catch (error) {
      add_finding(scanned.findings, seen, error.code ?? 'npm_pack_unverifiable', 'package.json', 0);
    }
  }
  return finalize_report('directory', {
    snapshot_type: 'history_free_tracked_tree',
    manifest_bound: true,
    scanned_tree_sha256: scanned.scanned_tree_sha256,
  }, {
    files_scanned: entries.length,
    binary_files: scanned.binary_files,
    bytes_scanned: scanned.bytes_scanned,
  }, scanned.findings);
}

function parse_batch_blobs(output, expected) {
  const blobs = new Map();
  let offset = 0;
  while (offset < output.length) {
    const newline = output.indexOf(10, offset);
    if (newline < 0) throw new privacy_error('history_blob_stream_invalid');
    const header = output.subarray(offset, newline).toString('ascii');
    const [oid, type, size_text] = header.split(' ');
    const size = Number(size_text);
    if (!expected.has(oid) || type !== 'blob' || !Number.isSafeInteger(size) || size < 0) throw new privacy_error('history_blob_stream_invalid');
    const start = newline + 1;
    const end = start + size;
    if (end >= output.length || output[end] !== 10) throw new privacy_error('history_blob_stream_invalid');
    blobs.set(oid, output.subarray(start, end));
    offset = end + 1;
  }
  if (blobs.size !== expected.size) throw new privacy_error('history_blob_stream_incomplete');
  return blobs;
}

function enumerate_history_blob_paths(repository_root) {
  const trees = [...new Set(run_git(repository_root, ['log', '--all', '--format=%T'])
    .split(/\r?\n/)
    .filter(value => /^[a-f0-9]{40,64}$/.test(value)))].sort();
  const paths_by_blob = new Map();
  const all_paths = new Set();
  for (const tree of trees) {
    for (const entry of list_commit_entries(repository_root, tree)) {
      all_paths.add(entry.path);
      if (entry.type !== 'blob') continue;
      const paths = paths_by_blob.get(entry.oid) ?? new Set();
      paths.add(entry.path);
      paths_by_blob.set(entry.oid, paths);
    }
  }
  return { all_paths, paths_by_blob };
}

function audit_history(repository_root, options) {
  const policy = options.policy ?? default_policy;
  const { all_paths, paths_by_blob } = enumerate_history_blob_paths(repository_root);
  const ids = [...paths_by_blob.keys()];
  const checked = run_git(repository_root, ['cat-file', '--batch-check=%(objectname) %(objecttype) %(objectsize)'], { input: `${ids.join('\n')}\n` });
  const blobs = new Map();
  let total_bytes = 0;
  for (const line of checked.split(/\r?\n/).filter(Boolean)) {
    const [oid, type, size_text] = line.split(' ');
    const size = Number(size_text);
    if (type === 'blob' && paths_by_blob.has(oid) && Number.isSafeInteger(size) && size >= 0) {
      total_bytes += size;
      blobs.set(oid, size);
    }
  }
  const findings = [];
  const seen = new Set();
  for (const path of [...all_paths].sort()) {
    const sensitive = sensitive_path_category(path);
    if (sensitive) add_finding(findings, seen, sensitive, path, 0);
  }
  if (total_bytes > policy.maximum_history_bytes) {
    add_finding(findings, seen, 'history_scan_oversized', '<git_history>', 0);
    return finalize_report('history', { refs: 'all' }, { files_scanned: 0, binary_files: 0, bytes_scanned: 0 }, findings);
  }
  const blob_ids = [...blobs.keys()].sort();
  const batch = run_git(repository_root, ['cat-file', '--batch'], { input: `${blob_ids.join('\n')}\n`, encoding: null });
  const content = parse_batch_blobs(batch, new Set(blob_ids));
  let binary_files = 0;
  let file_instances = 0;
  for (const oid of blob_ids) {
    for (const path of [...paths_by_blob.get(oid)].sort()) {
      const result = scan_content(path, content.get(oid));
      file_instances += 1;
      if (result.binary) binary_files += 1;
      for (const finding of result.findings) add_finding(findings, seen, finding.category, finding.path, finding.line);
    }
  }
  const metadata = run_git(repository_root, ['log', '--all', '--format=%H%x09%ae%x09%ce']);
  for (const line of metadata.split(/\r?\n/).filter(Boolean)) {
    const [commit, ...values] = line.split('\t');
    const result = scan_content(`git_metadata/${commit.slice(0, 12)}.txt`, Buffer.from(values.join('\n')));
    for (const finding of result.findings) add_finding(findings, seen, finding.category, finding.path, finding.line);
  }
  return finalize_report('history', { refs: 'all' }, {
    files_scanned: file_instances,
    binary_files,
    bytes_scanned: total_bytes,
  }, findings);
}

export function audit_repository(options = {}) {
  const requested_root = resolve(options.repository_root ?? process.cwd());
  if (options.directory || existsSync(join(requested_root, 'public_snapshot_manifest.json'))) {
    return audit_directory(requested_root, options);
  }
  const repository_root = find_repository_root(requested_root);
  if (options.history) return audit_history(repository_root, options);
  if (options.reference) return audit_ref(repository_root, options.reference);
  return audit_current(repository_root, options);
}

function parse_args(argv) {
  const result = { directory: false, history: false, reference: null, repository_root: process.cwd() };
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === '--history' && !result.reference) result.history = true;
    else if (arg === '--ref' && !result.history && argv[index + 1]) result.reference = argv[++index];
    else if (arg === '--directory' && !result.history && !result.reference && argv[index + 1]) {
      result.directory = true;
      result.repository_root = resolve(argv[++index]);
    }
    else if (arg === '--repo' && argv[index + 1]) result.repository_root = resolve(argv[++index]);
    else throw new privacy_error('invalid_arguments');
  }
  return result;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const report = audit_repository(parse_args(process.argv.slice(2)));
    process.stdout.write(`${JSON.stringify(report, null, 2)}\n`);
    if (report.summary.blockers > 0) process.exitCode = 2;
  } catch (error) {
    const code = error instanceof privacy_error ? error.code : 'audit_failed';
    process.stdout.write(`${JSON.stringify({ schema_version: 1, error: `privacy:${code}` })}\n`);
    process.exitCode = 1;
  }
}
