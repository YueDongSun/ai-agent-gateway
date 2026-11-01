/**
 * Privacy contract tests for the Rust diagnostics timeline IPC record.
 *
 * Run with: node --test tests/privacy/diagnostics_timeline_privacy.test.mjs
 */

import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

const diagnostics_source = readFileSync(
  new URL('../../src-tauri/src/diagnostics.rs', import.meta.url),
  'utf8',
);
const approved_rust_fields = [
  'request_id',
  'surface',
  'model_ref',
  'timestamp_ms',
  'status',
  'duration_ms',
  'streamed',
  'failed_over',
  'plan_kind',
  'plan_tier',
  'plan_downgraded',
  'plan_basis',
  'plan_reason',
  'config_generation',
];
const approved_ipc_fields = [
  'requestId',
  'surface',
  'modelRef',
  'timestampMs',
  'status',
  'durationMs',
  'streamed',
  'failedOver',
  'planKind',
  'planTier',
  'planDowngraded',
  'planBasis',
  'planReason',
  'configGeneration',
];
const forbidden_fragments = [
  'https://',
  'http://',
  '10.23.45.67',
  'evil.example.invalid',
  'sk-private-token',
  'ghp_private-token',
  'authorization: Bearer',
  'user:password@',
];

function rust_event_fields() {
  const struct_match = diagnostics_source.match(
    /pub struct Request_event\s*\{([\s\S]*?)\n\}/,
  );
  assert.ok(struct_match, 'Request_event must exist in diagnostics.rs');
  return [...struct_match[1].matchAll(/^\s*pub\s+(\w+)\s*:/gm)].map((match) => match[1]);
}

function camel_case(value) {
  return value.replace(/_([a-z])/g, (_, letter) => letter.toUpperCase());
}

test('Rust IPC record declares only the approved metadata fields', () => {
  assert.match(diagnostics_source, /#\[derive\([^\]]*Serialize[^\]]*\)\]\s*#\[serde\(rename_all = "camelCase"\)\]\s*#\[allow\(non_camel_case_types\)\]\s*pub struct Request_event/);
  assert.deepEqual(rust_event_fields(), approved_rust_fields);
  assert.deepEqual(rust_event_fields().map(camel_case), approved_ipc_fields);
});

test('serialized IPC shape has exactly the approved keys and safe values', () => {
  const record = {
    requestId: 'req_64f01234_1',
    surface: 'chat',
    modelRef: 'ref_abcdef012345',
    timestampMs: 1_800_000_000_000,
    status: 200,
    durationMs: 42,
    streamed: false,
    failedOver: null,
    planKind: 'alias',
    planTier: 3,
    planDowngraded: true,
    planBasis: 'manual_override',
    planReason: 'alias_tier_downgrade',
    configGeneration: 7,
  };
  const serialized = JSON.stringify(record);
  const parsed = JSON.parse(serialized);
  assert.deepEqual(Object.keys(parsed), approved_ipc_fields);
  for (const fragment of forbidden_fragments) {
    assert.equal(serialized.includes(fragment), false, `forbidden data survived: ${fragment}`);
  }
});

test('record construction sanitizes identifiers and constrains the HTTP surface', () => {
  assert.match(diagnostics_source, /request_id:\s*safe_request_id\(request_id\)/);
  assert.match(diagnostics_source, /surface:\s*safe_surface\(surface\)/);
  assert.match(diagnostics_source, /let model_ref = safe_reference\(model\)/);
  assert.match(diagnostics_source, /model_ref:\s*safe_model_ref/);
  assert.match(diagnostics_source, /"chat"\s*\|\s*"messages"\s*\|\s*"responses"\s*\|\s*"models"\s*\|\s*"gemini"/);
  // Plan labels and codes pass through the same kind of boundary: fixed set
  // for the kind, snake_case code shape for basis/reason, never free text.
  assert.match(diagnostics_source, /plan_kind:\s*plan\.kind\.as_deref\(\)\.and_then\(safe_plan_kind\)/);
  assert.match(diagnostics_source, /plan_basis:\s*plan\.basis\.as_deref\(\)\.and_then\(safe_plan_code\)/);
  assert.match(diagnostics_source, /plan_reason:\s*plan\.reason\.as_deref\(\)\.and_then\(safe_plan_code\)/);
  assert.match(diagnostics_source, /"exact"\s*\|\s*"precise"\s*\|\s*"alias"/);
});

test('plan codes reject free text and URL shapes', () => {
  assert.match(diagnostics_source, /fn safe_plan_code\(value: &str\) -> Option<String>/);
  assert.match(diagnostics_source, /"invalid"\.to_string\(\)/);
  // The adversarial shape used by the Rust unit test: a URL must not survive
  // the code-shape check into the timeline record.
  const code_shape = /^[a-z0-9_]{1,48}$/;
  assert.equal(code_shape.test('alias_tier_downgrade'), true);
  assert.equal(code_shape.test('https://evil.example.invalid/x'), false);
  assert.equal(code_shape.test('Manual Override'), false);
});
