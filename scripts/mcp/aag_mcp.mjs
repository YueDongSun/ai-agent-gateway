#!/usr/bin/env node
/** Bounded newline-delimited MCP stdio transport; stdout is protocol-only. */
import { once } from 'node:events';
import { create_session, parse_error, size_error } from './protocol.mjs';

const max_message_bytes = 64 * 1024;
const handle = create_session();
const decoder = new TextDecoder('utf-8', { fatal: true });
let pending = Buffer.alloc(0);
let dropping = false;
async function send(response) {
  if (response && !process.stdout.write(JSON.stringify(response) + '\n')) await once(process.stdout, 'drain');
}
async function line(value) {
  if (value.length > max_message_bytes) { await send(size_error()); return; }
  let request;
  try { request = JSON.parse(decoder.decode(value)); }
  catch { await send(parse_error()); return; }
  await send(handle(request));
}
try {
  for await (const chunk of process.stdin) {
    pending = Buffer.concat([pending, chunk]);
    let newline;
    while ((newline = pending.indexOf(10)) !== -1) {
      const next = pending.subarray(0, newline); pending = pending.subarray(newline + 1);
      if (!dropping) await line(next);
      dropping = false;
    }
    if (pending.length > max_message_bytes) {
      if (!dropping) await send(size_error());
      pending = Buffer.alloc(0); dropping = true;
    }
  }
  if (pending.length && !dropping) await send(parse_error());
} catch {
  process.stderr.write('AAG_MCP_TRANSPORT_FAILED\n');
  process.exitCode = 1;
}
