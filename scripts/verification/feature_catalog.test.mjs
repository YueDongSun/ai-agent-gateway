import { test } from 'node:test';
import assert from 'node:assert/strict';
import { feature_scopes, verification_plan } from './feature_catalog.mjs';

test('every scope produces unique allowlisted local checks and explicit exclusions', () => {
  for (const scope of feature_scopes) {
    const plan = verification_plan(scope);
    assert.equal(plan.mode, 'plan_only');
    assert.equal(plan.preflight.mode, 'on_execution_only');
    assert.equal(new Set(plan.checks.map((check) => check.id)).size, plan.checks.length);
    assert.ok(plan.exclusions.includes('real providers'));
    assert.ok(plan.checks.every((check) => ['node', 'cargo'].includes(check.runtime)));
    assert.ok(plan.checks.every((check) => ['.', 'ui', 'src-tauri'].includes(check.cwd)));
    assert.ok(plan.checks.every((check) => !check.args.includes('--ignored')));
  }
});
test('release verification includes both preflight assets and privacy audit', () => {
  const plan = verification_plan('all');
  const ids = plan.checks.map((check) => check.id);
  assert.equal(ids.indexOf('privacy_audit'), ids.indexOf('release_preflight') + 1);
  assert.deepEqual(plan.checks.find((check) => check.id === 'privacy_audit').args, ['scripts/privacy/audit_repository.mjs']);
});
test('MCP and DSH build the frontend before offline serial Rust tests', () => {
  for (const scope of ['mcp', 'dsh']) {
    const plan = verification_plan(scope);
    const ids = plan.checks.map((check) => check.id);
    assert.ok(ids.indexOf('ui_native_build') < ids.indexOf('rust_check'));
    assert.ok(ids.includes('ui_smoke') && ids.includes('ui_smoke_demo'));
    const rust = plan.checks.find((check) => check.id === 'rust_tests');
    assert.ok(rust.args.includes('--locked') && rust.args.includes('--offline') && rust.args.includes('--test-threads=1'));
  }
  assert.ok(verification_plan('dsh').checks.some((check) => check.id === 'dsh_transport'));
  assert.ok(!verification_plan('node').checks.some((check) => check.runtime === 'cargo'));
});
test('unknown scope cannot inject commands, and returned plans do not share arguments', () => {
  for (const scope of ['', '../', 'all; exit', '__proto__', null]) assert.throws(() => verification_plan(scope), /AAG_INVALID_SCOPE/);
  const plan = verification_plan('all'); plan.checks[0].args.push('--injected');
  assert.ok(!verification_plan('all').checks[0].args.includes('--injected'));
});
