import { constants as fs_constants, promises as fs } from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { z } from 'zod';

const max_descriptor_bytes = 16 * 1024;
const max_frame_bytes = 64 * 1024;
export const instance_id_pattern = /^[0-9a-f]{32}$/;
// Shared cursor contract for the page tools: both the server input schemas and
// the client response schemas must agree, so the schema lives here once.
export const cursor_schema = z.string().regex(/^\d{1,3}$/).refine((cursor) => /^[0-9]{1,3}$/.exec(cursor)?.[0] === cursor);
const descriptor_schema = z.object({
  schema_version: z.literal(1),
  instance_id: z.string().regex(instance_id_pattern),
  pipe_name: z.string().min(1).max(256),
  process_id: z.number().int().positive().safe(),
}).strict();
const utf8_decoder = new TextDecoder('utf-8', { fatal: true });

export class Workflow_error extends Error {
  constructor(code) {
    super(code);
    this.name = 'Workflow_error';
    this.code = code;
  }
}

function workflow_error(code) {
  return new Workflow_error(`workflow:${code}`);
}

export function default_descriptor_file(env = process.env, home_directory = os.homedir()) {
  const config_root = env.AAG_CONFIG_DIR
    ? path.resolve(env.AAG_CONFIG_DIR)
    : path.join(home_directory, '.aag');
  return path.resolve(config_root, 'runtime', 'workflow_ipc.json');
}

export function parse_server_args(argv = process.argv.slice(2), env = process.env) {
  if (argv.length === 0) return { descriptor_file: default_descriptor_file(env) };
  if (argv.length === 2 && argv[0] === '--descriptor-file' && argv[1].length > 0) {
    return { descriptor_file: path.resolve(argv[1]) };
  }
  if (argv.length === 1 && argv[0] === '--help') return { help: true };
  throw workflow_error('configuration_invalid');
}

function validate_private_mode(stats, platform) {
  if (platform !== 'win32' && (stats.mode & 0o077) !== 0) {
    throw workflow_error('descriptor_not_private');
  }
}

export async function read_descriptor(descriptor_file, options = {}) {
  const platform = options.platform ?? process.platform;
  const fs_api = options.fs_api ?? fs;
  const absolute_path = path.resolve(descriptor_file);
  if (platform === 'win32') await reject_reparse_parents(absolute_path, fs_api);
  let path_stats;
  try {
    path_stats = await fs_api.lstat(absolute_path);
  } catch {
    throw workflow_error('owner_unavailable');
  }
  if (path_stats.isSymbolicLink() || path_stats.isBlockDevice?.() || !path_stats.isFile()) {
    throw workflow_error('descriptor_file_invalid');
  }
  if (path_stats.size > max_descriptor_bytes) throw workflow_error('descriptor_too_large');

  let file_handle;
  try {
    const no_follow = fs_constants.O_NOFOLLOW ?? 0;
    file_handle = await fs_api.open(absolute_path, fs_constants.O_RDONLY | no_follow);
    const opened_stats = await file_handle.stat();
    if (!opened_stats.isFile() || opened_stats.dev !== path_stats.dev || opened_stats.ino !== path_stats.ino) {
      throw workflow_error('descriptor_file_invalid');
    }
    if (opened_stats.size > max_descriptor_bytes) throw workflow_error('descriptor_too_large');
    validate_private_mode(opened_stats, platform);

    const chunks = [];
    let total_bytes = 0;
    let position = 0;
    while (total_bytes <= max_descriptor_bytes) {
      const buffer = Buffer.alloc(Math.min(4096, max_descriptor_bytes + 1 - total_bytes));
      const { bytesRead } = await file_handle.read(buffer, 0, buffer.length, position);
      if (bytesRead === 0) break;
      chunks.push(buffer.subarray(0, bytesRead));
      total_bytes += bytesRead;
      position += bytesRead;
    }
    if (total_bytes > max_descriptor_bytes) throw workflow_error('descriptor_too_large');
    let parsed_value;
    try {
      parsed_value = JSON.parse(utf8_decoder.decode(Buffer.concat(chunks)));
    } catch {
      throw workflow_error('descriptor_invalid');
    }
    const descriptor_result = descriptor_schema.safeParse(parsed_value);
    if (!descriptor_result.success) throw workflow_error('descriptor_invalid');
    const expected_pipe_name = `\\\\.\\pipe\\aag_workflow_${descriptor_result.data.instance_id}`;
    if (descriptor_result.data.pipe_name !== expected_pipe_name) throw workflow_error('descriptor_path_invalid');
    return descriptor_result.data;
  } catch (error) {
    if (error instanceof Workflow_error) throw error;
    if (error?.code === 'ELOOP') throw workflow_error('descriptor_file_invalid');
    throw workflow_error('descriptor_unreadable');
  } finally {
    await file_handle?.close().catch(() => {});
  }
}

async function reject_reparse_parents(absolute_path, fs_api) {
  const parsed_path = path.parse(absolute_path);
  const segments = absolute_path.slice(parsed_path.root.length).split(path.sep).filter(Boolean);
  let current_path = parsed_path.root;
  for (const segment of segments) {
    let stats;
    try { stats = await fs_api.lstat(current_path); }
    catch { throw workflow_error('descriptor_path_invalid'); }
    if (stats.isSymbolicLink()) throw workflow_error('descriptor_path_invalid');
    current_path = path.join(current_path, segment);
  }
}

export function validate_process_alive(process_id, process_api = process) {
  try {
    process_api.kill(process_id, 0);
    return true;
  } catch (error) {
    if (error?.code === 'EPERM') return true;
    return false;
  }
}

export const workflow_config_limits = Object.freeze({ max_descriptor_bytes, max_frame_bytes });
