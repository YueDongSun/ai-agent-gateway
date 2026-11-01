import { createHash, randomUUID } from 'node:crypto';
import {
  chmodSync,
  existsSync,
  lstatSync,
  mkdirSync,
  readFileSync,
  realpathSync,
  renameSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { basename, dirname, isAbsolute, posix, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  audit_repository,
  read_export_policy_from_entries,
  verify_npm_pack_manifest,
} from './audit_repository.mjs';
import {
  find_repository_root,
  is_path_inside,
  list_commit_entries,
  list_index_entries,
  normalize_repository_path,
  privacy_error,
  read_commit_entry,
  read_index_entry,
  repository_is_clean,
  resolve_commit,
  source_commit_time,
  source_tree_oid,
} from './git_tree.mjs';

const bound_tool_paths = [
  'scripts/npm_discovery.mjs',
  'scripts/privacy/audit_repository.mjs',
  'scripts/privacy/export_snapshot.mjs',
  'scripts/privacy/git_tree.mjs',
  'scripts/privacy/public_export_policy.json',
];
const local_policy = JSON.parse(readFileSync(new URL('./public_export_policy.json', import.meta.url), 'utf8'));

function sha256(bytes) {
  return createHash('sha256').update(bytes).digest('hex');
}

function normalize_text_line_endings(bytes) {
  const normalized = [];
  for (let index = 0; index < bytes.length; index += 1) {
    if (bytes[index] === 0x0d) {
      if (bytes[index + 1] === 0x0a) index += 1;
      normalized.push(0x0a);
    } else {
      normalized.push(bytes[index]);
    }
  }
  return Buffer.from(normalized);
}

function assert_existing_ancestors_not_linked(path_value) {
  let current = resolve(path_value);
  while (!existsSync(current)) current = dirname(current);
  for (;;) {
    if (lstatSync(current).isSymbolicLink()) throw new privacy_error('linked_output_path');
    const parent = dirname(current);
    if (parent === current) break;
    current = parent;
  }
}

function validate_output(repository_root, output_path) {
  if (!isAbsolute(output_path)) throw new privacy_error('output_must_be_absolute');
  const output = resolve(output_path);
  if (is_path_inside(repository_root, output)) throw new privacy_error('output_inside_source_tree');
  if (existsSync(output)) throw new privacy_error('output_exists');
  const parent = dirname(output);
  if (!existsSync(parent) || !lstatSync(parent).isDirectory()) throw new privacy_error('output_parent_missing');
  assert_existing_ancestors_not_linked(parent);
  return resolve(realpathSync(parent), basename(output));
}

function release_extension(path, policy) {
  const lower = path.toLowerCase();
  return policy.release_asset_extensions.some(extension => lower.endsWith(extension));
}

export function export_path_allowed(path_value, policy = local_policy) {
  const path = normalize_repository_path(path_value);
  if (policy.export_excluded_paths.some(rule => rule.endsWith('/') ? path.startsWith(rule) : path === rule)) return false;
  if (release_extension(path, policy)) return false;
  if (path === 'public_snapshot_manifest.json') return false;
  return true;
}

function assert_source_tooling_bound(entries, read_entry) {
  for (const path of bound_tool_paths) {
    const entry = entries.find(candidate => candidate.path === path);
    if (!entry) throw new privacy_error('source_tooling_missing');
    const local_path = fileURLToPath(new URL('../../' + path, import.meta.url));
    if (
      sha256(normalize_text_line_endings(read_entry(entry))) !==
      sha256(normalize_text_line_endings(readFileSync(local_path)))
    ) {
      throw new privacy_error('source_tooling_mismatch');
    }
  }
}

function destination_path(root, repository_path) {
  const target = resolve(root, ...repository_path.split('/'));
  const relation = relative(root, target);
  if (relation === '' || relation === '..' || relation.startsWith(`..${sep}`) || isAbsolute(relation)) {
    throw new privacy_error('export_path_escape');
  }
  return target;
}

function safe_remove_temporary(parent, temporary) {
  if (!is_path_inside(parent, temporary) || resolve(parent) === resolve(temporary)) throw new privacy_error('temporary_cleanup_refused');
  if (existsSync(temporary)) rmSync(temporary, { recursive: true, force: true });
}

function markdown_line(text, offset) {
  let line = 1;
  for (let index = 0; index < offset; index += 1) if (text.charCodeAt(index) === 10) line += 1;
  return line;
}

function link_fingerprint(path, line) {
  return createHash('sha256').update(`aag_public_link_v1\0${path}\0${line}`).digest('hex').slice(0, 20);
}

// Shared normalization for inline-link and reference-definition targets:
// anchors, schemed, and protocol-relative targets are not snapshot-local;
// the remainder is percent-decoded, stripped of fragment then query, joined
// against the snapshot root (rooted targets) or the containing file's
// directory, and rejected when it escapes the snapshot or carries a
// backslash. Returns a snapshot-root-relative posix path or null.
function normalize_local_link_target(raw_target, file_path) {
  if (!raw_target || raw_target.startsWith('#') || /^[a-z][a-z0-9+.-]*:/i.test(raw_target) || raw_target.startsWith('//')) return null;
  let target;
  try { target = decodeURIComponent(raw_target).split('#', 1)[0].split('?', 1)[0]; }
  catch { target = null; }
  let normalized = null;
  if (target) {
    const joined = target.startsWith('/')
      ? posix.normalize(target.slice(1))
      : posix.normalize(posix.join(posix.dirname(file_path), target));
    if (joined !== '..' && !joined.startsWith('../') && !joined.includes('\\')) normalized = joined.replace(/\/$/, '');
  }
  return normalized;
}

export function verify_snapshot_links(snapshot_root, exported_files, policy = local_policy) {
  const files = new Set(exported_files.map(file => file.path));
  const directories = new Set();
  for (const path of files) {
    let directory = posix.dirname(path);
    while (directory !== '.') {
      directories.add(directory);
      directory = posix.dirname(directory);
    }
  }
  // The policy can declare a list of paths whose targets are expected to
  // disappear from the public tree: dev-tree references like `PROGRESS.md`,
  // `docs/progress/`, `.kilo/`, `.claude/`, `docs/PHASE-ARCHIVE.md`, and
  // markdown-format review files. The post-export surgery in
  // scripts/public_sync.mjs removes every markdown link that still points
  // at one of these; reporting them here as `broken_local_link` would
  // force every release to manually pre-strip every such bullet, which
  // became noisy once the public exclusion list tightened. Security-driven
  // exclusions stay in `export_excluded_paths` and continue to trigger
  // `broken_local_link` here as before.
  const exempt_targets = new Set(policy.link_check_exempt_targets ?? []);
  const findings = [];
  for (const file of exported_files.filter(candidate => candidate.path.toLowerCase().endsWith('.md'))) {
    const text = readFileSync(destination_path(snapshot_root, file.path), 'utf8');
    const inline_pattern = /!?\[[^\]]*\]\(\s*(?:<([^>]+)>|([^\s)]+))(?:\s+["'][^"']*["'])?\s*\)/g;
    // Reference-style definitions: up to three leading spaces, [label]:,
    // an <angle> or bare non-whitespace target, an optional single-line
    // title in double quotes, single quotes, or parentheses, end of line.
    // Usages such as [text][label] and shortcut [label] carry no target
    // here and stay silent.
    const definition_pattern = /^[ ]{0,3}\[([^\]\n]+)\]:[ \t]*(?:<([^>\n]+)>|(\S+))(?:[ \t]+(?:"[^"\n]*"|'[^'\n]*'|\([^()\n]*\)))?[ \t]*$/gm;
    const candidates = [];
    for (const match of text.matchAll(inline_pattern)) candidates.push({ raw_target: match[1] ?? match[2], index: match.index });
    for (const match of text.matchAll(definition_pattern)) candidates.push({ raw_target: match[2] ?? match[3], index: match.index });
    for (const candidate of candidates) {
      const normalized = normalize_local_link_target(candidate.raw_target, file.path);
      if (!normalized) continue;
      if (files.has(normalized) || directories.has(normalized)) continue;
      if (exempt_targets.has(normalized)) continue;
      const line = markdown_line(text, candidate.index);
      findings.push({
        severity: 'blocker',
        category: 'broken_local_link',
        path: file.path,
        line,
        fingerprint: link_fingerprint(file.path, line),
      });
    }
  }
  return findings.sort((left, right) => left.path.localeCompare(right.path, 'en') || left.line - right.line);
}

function parse_args(argv) {
  const result = { output: null, source_commit: null, repository_root: process.cwd() };
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === '--output' && argv[index + 1]) result.output = argv[++index];
    else if (arg === '--source-commit' && argv[index + 1]) result.source_commit = argv[++index];
    else if (arg === '--repo' && argv[index + 1]) result.repository_root = argv[++index];
    else throw new privacy_error('invalid_arguments');
  }
  if (!result.output) throw new privacy_error('output_required');
  if (result.source_commit && !/^[a-f0-9]{40}$/.test(result.source_commit)) {
    throw new privacy_error('source_commit_must_be_full_hash');
  }
  return result;
}

export function export_snapshot(options) {
  const repository_root = find_repository_root(options.repository_root ?? process.cwd());
  const output = validate_output(repository_root, options.output);
  if (options.source_commit && !/^[a-f0-9]{40}$/.test(options.source_commit)) {
    throw new privacy_error('source_commit_must_be_full_hash');
  }
  let commit;
  let entries;
  let read_entry;
  let audit_report;

  if (options.source_commit) {
    commit = resolve_commit(repository_root, options.source_commit);
    if (commit !== options.source_commit) throw new privacy_error('source_commit_mismatch');
    audit_report = audit_repository({ repository_root, reference: commit });
    entries = list_commit_entries(repository_root, commit);
    read_entry = entry => read_commit_entry(repository_root, entry);
  } else {
    if (!repository_is_clean(repository_root)) throw new privacy_error('dirty_source_tree');
    commit = resolve_commit(repository_root);
    audit_report = audit_repository({ repository_root });
    entries = list_index_entries(repository_root);
    read_entry = entry => read_index_entry(repository_root, entry);
  }
  if (audit_report.summary.blockers > 0) throw new privacy_error('privacy_audit_blocked');
  assert_source_tooling_bound(entries, read_entry);
  const selected_policy = read_export_policy_from_entries(entries, read_entry);
  const policy_entry = entries.find(entry => entry.path === 'scripts/privacy/public_export_policy.json');
  const policy_sha256 = sha256(read_entry(policy_entry));

  const parent = dirname(output);
  const temporary = resolve(parent, `.${basename(output)}.privacy_export_${randomUUID()}`);
  if (!is_path_inside(parent, temporary) || existsSync(temporary)) throw new privacy_error('temporary_output_invalid');
  const exported_files = [];
  try {
    mkdirSync(temporary, { recursive: false });
    for (const entry of entries) {
      if (!export_path_allowed(entry.path, selected_policy)) continue;
      if (entry.mode === '120000' || entry.mode === '160000' || (entry.type && entry.type !== 'blob')) {
        throw new privacy_error('tracked_symlink');
      }
      const bytes = read_entry(entry);
      const target = destination_path(temporary, entry.path);
      mkdirSync(dirname(target), { recursive: true });
      writeFileSync(target, bytes, { flag: 'wx' });
      const executable = entry.mode === '100755';
      if (executable) chmodSync(target, 0o755);
      exported_files.push({
        path: entry.path,
        mode: executable ? '100755' : '100644',
        bytes: bytes.length,
        sha256: sha256(bytes),
      });
    }
    exported_files.sort((left, right) => left.path.localeCompare(right.path, 'en'));
    const link_findings = verify_snapshot_links(temporary, exported_files);
    if (link_findings.length > 0) throw new privacy_error('export_broken_local_link');
    const pack = verify_npm_pack_manifest(temporary, selected_policy);
    if (pack.forbidden.length > 0) throw new privacy_error('npm_pack_forbidden_path');
    const tree_hasher = createHash('sha256');
    for (const file of exported_files) {
      tree_hasher.update(`${file.mode}\0${file.path}\0${file.sha256}\0${file.bytes}\n`);
    }
    const manifest = {
      schema_version: 1,
      snapshot_type: 'history_free_tracked_tree',
      policy_sha256,
      export_tree_sha256: tree_hasher.digest('hex'),
      file_count: exported_files.length,
      excluded_path_rules: [...selected_policy.export_excluded_paths],
      excluded_release_asset_extensions: [...selected_policy.release_asset_extensions],
      privacy_report: {
        schema_version: audit_report.schema_version,
        mode: 'selected_tree',
        summary: audit_report.summary,
        findings: audit_report.findings,
      },
      link_check: {
        markdown_files: exported_files.filter(file => file.path.toLowerCase().endsWith('.md')).length,
        broken_links: 0,
      },
      npm_pack: { file_count: pack.file_count, forbidden_paths: 0 },
      files: exported_files,
    };
    writeFileSync(resolve(temporary, 'public_snapshot_manifest.json'), `${JSON.stringify(manifest, null, 2)}\n`, { flag: 'wx' });
    renameSync(temporary, output);
    return {
      ...manifest,
      private_source: {
        commit,
        commit_time: source_commit_time(repository_root, commit),
        git_tree: source_tree_oid(repository_root, commit),
      },
    };
  } catch (error) {
    safe_remove_temporary(parent, temporary);
    throw error;
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const manifest = export_snapshot(parse_args(process.argv.slice(2)));
    process.stdout.write(`${JSON.stringify({
      schema_version: 1,
      source_commit: manifest.private_source.commit,
      source_tree: manifest.private_source.git_tree,
      export_tree_sha256: manifest.export_tree_sha256,
      file_count: manifest.file_count,
    }, null, 2)}\n`);
  } catch (error) {
    const code = error instanceof privacy_error ? error.code : 'export_failed';
    process.stdout.write(`${JSON.stringify({ schema_version: 1, error: `privacy:${code}` })}\n`);
    process.exitCode = 1;
  }
}
