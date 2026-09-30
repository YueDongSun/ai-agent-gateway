/** Explicit offline maintainer command; reads only the existing normalized snapshot. */
import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { sanitize_catalog_snapshot } from './catalog_core.mjs';
import { write_catalog } from './collect_catalog.mjs';

const bundled_path = fileURLToPath(new URL('../../shared/dsh_market_catalog.json', import.meta.url));

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    if (process.argv.length !== 2) throw new Error('Usage: node scripts/dsh_market/rescrub_catalog.mjs');
    const source = JSON.parse(await readFile(bundled_path, 'utf8'));
    const catalog = sanitize_catalog_snapshot(source);
    await write_catalog(catalog, bundled_path);
    console.log(JSON.stringify({
      entries: catalog.entry_count,
      registry_commit: catalog.registry_commit,
      privacy_scrub: catalog.privacy_scrub,
    }));
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
