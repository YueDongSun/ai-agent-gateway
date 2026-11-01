#!/usr/bin/env node
// Bundle-size measurement evidence for the deferred PROGRESS open item
// ("per-row rendering and bundle improvements have measurement evidence").
// Read-only: walks the built ui/dist (native) and ui/dist_demo (demo) trees,
// prints per-build JS/CSS byte totals with the largest assets, and exits
// nonzero when a build is missing or a regression budget is exceeded.
// Run after `npm run ui:build` and `npm run ui:build_demo`.
import { readdirSync, statSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const build_roots = [
  { label: 'native', path: fileURLToPath(new URL('../../ui/dist', import.meta.url)) },
  { label: 'demo', path: fileURLToPath(new URL('../../ui/dist_demo', import.meta.url)) },
];
// Regression gate, not a target: ~1.8x the measured 2026-10 baseline of
// ~570 KiB (489.6 KiB JS + 80.5 KiB CSS) per build.
const js_css_budget_bytes = 1024 * 1024;
const top_assets = 5;

function list_files(root) {
  const files = [];
  const stack = [root];
  while (stack.length) {
    const dir = stack.pop();
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const full = join(dir, entry.name);
      if (entry.isDirectory()) stack.push(full);
      else files.push({ path: full, bytes: statSync(full).size });
    }
  }
  return files;
}

let failed = false;
for (const build of build_roots) {
  let files;
  try {
    files = list_files(build.path);
  } catch (error) {
    console.error(`FAIL: ${build.label} build not readable at ${build.path}: ${error.code ?? error.message}`);
    failed = true;
    continue;
  }
  const sum = (list) => list.reduce((total, file) => total + file.bytes, 0);
  const js_files = files.filter((file) => file.path.endsWith('.js'));
  const css_files = files.filter((file) => file.path.endsWith('.css'));
  const js_bytes = sum(js_files);
  const css_bytes = sum(css_files);
  const other_bytes = sum(files) - js_bytes - css_bytes;
  const transferred = js_bytes + css_bytes;
  console.log(
    `[bundle:${build.label}] js=${(js_bytes / 1024).toFixed(1)} KiB css=${(css_bytes / 1024).toFixed(1)} KiB` +
    ` other=${(other_bytes / 1024).toFixed(1)} KiB files=${files.length}`,
  );
  for (const file of [...files].sort((a, b) => b.bytes - a.bytes).slice(0, top_assets)) {
    const name = file.path.slice(build.path.length + 1).replaceAll('\\', '/');
    console.log(`  ${(file.bytes / 1024).toFixed(1)} KiB  ${name}`);
  }
  if (transferred > js_css_budget_bytes) {
    console.error(
      `FAIL: ${build.label} js+css ${(transferred / 1024).toFixed(1)} KiB exceeds the` +
      ` ${(js_css_budget_bytes / 1024).toFixed(1)} KiB regression budget`,
    );
    failed = true;
  }
}
process.exit(failed ? 1 : 0);
