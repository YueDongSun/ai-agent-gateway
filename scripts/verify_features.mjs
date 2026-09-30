#!/usr/bin/env node
import { fileURLToPath } from 'node:url';
import { verification_plan } from './verification/feature_catalog.mjs';
import { execute_plan } from './verification/runner.mjs';

const root = fileURLToPath(new URL('../', import.meta.url));
let scope = 'all'; let run = false;
try {
  const args = process.argv.slice(2);
  for (let index = 0; index < args.length; index += 1) {
    if (args[index] === '--scope' && args[index + 1]) scope = args[++index];
    else if (args[index] === '--run') run = true;
    else throw new Error('Usage: node scripts/verify_features.mjs [--scope node|ui|mcp|dsh|all] [--run]');
  }
  const plan = verification_plan(scope);
  if (!run) process.stdout.write(JSON.stringify(plan, null, 2) + '\n');
  else {
    const controller = new AbortController();
    const cancel = () => controller.abort();
    process.once('SIGINT', cancel); process.once('SIGTERM', cancel);
    try {
      const receipt = await execute_plan(plan, { root, signal: controller.signal, on_check: (check) => process.stdout.write(`[verify] ${check.id}: ${check.command}\n`) });
      process.stdout.write(JSON.stringify(receipt, null, 2) + '\n');
      process.exitCode = receipt.status === 'passed' ? 0 : 1;
    } finally { process.removeListener('SIGINT', cancel); process.removeListener('SIGTERM', cancel); }
  }
} catch (error) { process.stderr.write(String(error.message) + '\n'); process.exitCode = 1; }
