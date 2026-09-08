import { lstatSync, readFileSync, realpathSync } from 'node:fs';
import { isAbsolute, relative, resolve, sep } from 'node:path';
import { spawnSync } from 'node:child_process';

export class privacy_error extends Error {
  constructor(code) {
    super(`privacy:${code}`);
    this.code = code;
  }
}

export function run_git(repository_root, args, options = {}) {
  const result = spawnSync('git', ['-C', repository_root, ...args], {
    encoding: Object.hasOwn(options, 'encoding') ? options.encoding : 'utf8',
    input: options.input,
    maxBuffer: options.max_buffer ?? 512 * 1024 * 1024,
    windowsHide: true,
  });
  if (result.error || result.status !== 0) throw new privacy_error(options.error_code ?? 'git_command_failed');
  return result.stdout;
}

export function find_repository_root(start = process.cwd()) {
  const root = run_git(resolve(start), ['rev-parse', '--show-toplevel'], { error_code: 'repository_not_found' }).trim();
  return realpathSync(root);
}

export function normalize_repository_path(value) {
  if (typeof value !== 'string' || value.length === 0 || value.includes('\0') || value.includes('\\')) {
    throw new privacy_error('invalid_repository_path');
  }
  if (isAbsolute(value) || value.startsWith('/') || value.split('/').some(part => part === '' || part === '.' || part === '..')) {
    throw new privacy_error('invalid_repository_path');
  }
  return value;
}

function parse_index_record(record) {
  const separator = record.indexOf('\t');
  if (separator < 0) throw new privacy_error('invalid_git_index');
  const metadata = record.slice(0, separator).split(' ');
  if (metadata.length < 3) throw new privacy_error('invalid_git_index');
  return {
    mode: metadata[0],
    oid: metadata[1],
    stage: metadata[2],
    path: normalize_repository_path(record.slice(separator + 1)),
  };
}

export function list_index_entries(repository_root) {
  const output = run_git(repository_root, ['ls-files', '--stage', '-z']);
  const entries = output.split('\0').filter(Boolean).map(parse_index_record);
  if (entries.some(entry => entry.stage !== '0')) throw new privacy_error('unmerged_index');
  return entries.sort((left, right) => left.path.localeCompare(right.path, 'en'));
}

export function list_worktree_entries(repository_root) {
  const deleted = new Set(run_git(repository_root, ['ls-files', '--deleted', '-z']).split('\0').filter(Boolean));
  const entries = list_index_entries(repository_root).filter(entry => !deleted.has(entry.path));
  const known = new Set(entries.map(entry => entry.path));
  const untracked = run_git(repository_root, ['ls-files', '--others', '--exclude-standard', '-z']).split('\0').filter(Boolean);
  for (const path of untracked) {
    const normalized = normalize_repository_path(path);
    if (!known.has(normalized)) entries.push({ mode: '100644', oid: null, stage: '0', path: normalized, untracked: true });
  }
  return entries.sort((left, right) => left.path.localeCompare(right.path, 'en'));
}

export function resolve_commit(repository_root, value = 'HEAD') {
  const commit = run_git(repository_root, ['rev-parse', '--verify', `${value}^{commit}`], { error_code: 'source_commit_not_found' }).trim();
  if (!/^[a-f0-9]{40}$/.test(commit)) throw new privacy_error('invalid_source_commit');
  return commit;
}

export function source_tree_oid(repository_root, commit = 'HEAD') {
  const tree = run_git(repository_root, ['rev-parse', `${commit}^{tree}`], { error_code: 'source_tree_not_found' }).trim();
  if (!/^[a-f0-9]{40}$/.test(tree)) throw new privacy_error('invalid_source_tree');
  return tree;
}

export function source_commit_time(repository_root, commit) {
  const value = run_git(repository_root, ['show', '-s', '--format=%cI', commit], { error_code: 'source_commit_unreadable' }).trim();
  if (!/^\d{4}-\d{2}-\d{2}T/.test(value)) throw new privacy_error('source_commit_time_invalid');
  return value;
}

export function list_commit_entries(repository_root, commit) {
  const output = run_git(repository_root, ['ls-tree', '-r', '-z', '--full-tree', commit]);
  return output.split('\0').filter(Boolean).map(record => {
    const separator = record.indexOf('\t');
    if (separator < 0) throw new privacy_error('invalid_git_tree');
    const [mode, type, oid] = record.slice(0, separator).split(' ');
    if (!mode || !type || !/^[a-f0-9]{40,64}$/.test(oid)) throw new privacy_error('invalid_git_tree');
    return { mode, type, oid, path: normalize_repository_path(record.slice(separator + 1)) };
  }).sort((left, right) => left.path.localeCompare(right.path, 'en'));
}

export function read_index_entry(repository_root, entry) {
  const absolute_path = resolve(repository_root, ...entry.path.split('/'));
  const item = lstatSync(absolute_path);
  if (item.isSymbolicLink() || entry.mode === '120000') throw new privacy_error('tracked_symlink');
  if (!item.isFile()) throw new privacy_error('tracked_entry_not_file');
  return readFileSync(absolute_path);
}

export function read_commit_entry(repository_root, entry) {
  if (entry.type !== 'blob' || entry.mode === '120000' || entry.mode === '160000') {
    throw new privacy_error(entry.mode === '120000' ? 'tracked_symlink' : 'unsupported_git_entry');
  }
  return run_git(repository_root, ['cat-file', 'blob', entry.oid], {
    encoding: null,
    error_code: 'git_blob_unreadable',
  });
}

export function repository_is_clean(repository_root) {
  return run_git(repository_root, ['status', '--porcelain=v1', '--untracked-files=all']).length === 0;
}

export function is_path_inside(parent, child) {
  const relation = relative(resolve(parent), resolve(child));
  return relation === '' || (!relation.startsWith(`..${sep}`) && relation !== '..' && !isAbsolute(relation));
}
