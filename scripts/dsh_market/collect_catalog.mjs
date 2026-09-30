/** Explicit maintainer command; no schedules, credentials, installs, or plugin execution. */
import { mkdir, open, rename, unlink, lstat } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { randomUUID } from 'node:crypto';
import { collect_catalog, max_catalog_bytes } from './catalog_core.mjs';

export async function write_catalog(catalog, output_path) {
  const encoded = `${JSON.stringify(catalog)}\n`;
  if (Buffer.byteLength(encoded) > max_catalog_bytes) throw new Error('dsh_market:catalog_too_large');
  // Refuse symbolic-link output/ancestors rather than overwrite another project's state.
  for (let current = resolve(output_path);; current = dirname(current)) {
    try { if ((await lstat(current)).isSymbolicLink()) throw new Error('dsh_market:linked_output_path'); }
    catch (error) { if (error.code !== 'ENOENT') throw error; }
    if (dirname(current) === current) break;
  }
  await mkdir(dirname(output_path), { recursive: true });
  const temporary = `${output_path}.${randomUUID()}.tmp`;
  let handle;
  try {
    handle = await open(temporary, 'wx');
    await handle.writeFile(encoded); await handle.sync(); await handle.close(); handle = null;
    await rename(temporary, output_path);
  } finally {
    await handle?.close();
    await unlink(temporary).catch(error => { if (error.code !== 'ENOENT') throw error; });
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const args = process.argv.slice(2);
    if (args.length > 2 || (args.length && (args[0] !== '--output' || !args[1]))) throw new Error('Usage: node scripts/dsh_market/collect_catalog.mjs [--output PATH]');
    const output_path = args.length ? resolve(args[1]) : fileURLToPath(new URL('../../shared/dsh_market_catalog.json', import.meta.url));
    const catalog = await collect_catalog();
    await write_catalog(catalog, output_path);
    console.log(JSON.stringify({ entries: catalog.entry_count, registry_commit: catalog.registry_commit, fetched_at: catalog.fetched_at }));
  } catch (error) { console.error(error.message); process.exitCode = 1; }
}
