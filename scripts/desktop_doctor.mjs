#!/usr/bin/env node
import { fileURLToPath } from 'node:url';
import { inspect_desktop_environment } from './desktop_environment.mjs';

const root = fileURLToPath(new URL('../', import.meta.url));
try {
  let scope = 'all'; let json = false;
  const args = process.argv.slice(2);
  for (let index = 0; index < args.length; index += 1) {
    if (args[index] === '--json') json = true;
    else if (args[index] === '--scope' && args[index + 1]) scope = args[++index];
    else throw new Error('Usage: node scripts/desktop_doctor.mjs [--json] [--scope node|ui|mcp|dsh|all]');
  }
  const controller = new AbortController();
  const cancel = () => controller.abort();
  process.once('SIGINT', cancel); process.once('SIGTERM', cancel);
  try {
    const receipt = await inspect_desktop_environment({ root, scope, signal: controller.signal });
    if (json) process.stdout.write(JSON.stringify(receipt, null, 2) + '\n');
    else {
      process.stdout.write(`[desktop:doctor] ${receipt.platform}/${receipt.arch}: ${receipt.status}\n`);
      for (const check of receipt.checks) process.stdout.write(`${check.status}: ${check.id} — ${check.detail}${check.remediation ? `; ${check.remediation}` : ''}\n`);
    }
    process.exitCode = receipt.status === 'passed' ? 0 : 1;
  } finally {
    process.removeListener('SIGINT', cancel); process.removeListener('SIGTERM', cancel);
  }
} catch (error) {
  process.stderr.write(String(error.message) + '\n');
  process.exitCode = 1;
}
