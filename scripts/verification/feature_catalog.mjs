/** Shared, pure command catalog. The companion can plan but never execute it. */
export const feature_scopes = ['node', 'ui', 'mcp', 'dsh', 'all'];
const checks = [
  { id: 'workflow_facade_tests', command: 'npm run test:workflow', runtime: 'node', args: ['--test', 'scripts/workflow/workflow_facade.test.mjs'], cwd: '.', scopes: ['mcp'] },
  { id: 'mcp_browser_regression', command: 'node --test scripts/ui_smoke/mcp_checks.test.mjs', runtime: 'node', args: ['--test', 'scripts/ui_smoke/mcp_checks.test.mjs'], cwd: '.', scopes: ['mcp', 'ui'] },
  { id: 'node_typecheck', command: 'npm run typecheck', runtime: 'node', args: ['node_modules/typescript/bin/tsc', '--noEmit'], cwd: '.', scopes: ['node', 'mcp', 'dsh'] },
  { id: 'node_tests', command: 'npm test', runtime: 'node', args: ['node_modules/vitest/vitest.mjs', 'run'], cwd: '.', scopes: ['node', 'mcp', 'dsh'] },
  { id: 'automation_tests', command: 'npm run test:automation', runtime: 'node', args: ['--test', 'scripts/verification/feature_catalog.test.mjs', 'scripts/verification/runner.test.mjs', 'scripts/mcp/aag_mcp.test.mjs', 'scripts/ui_smoke/smoke_runtime.test.mjs', 'scripts/release_preflight.test.mjs', 'scripts/npm_discovery.test.mjs'], cwd: '.', scopes: ['node', 'ui', 'mcp', 'dsh'] },
  { id: 'rust_profile_runner', command: 'npm run test:rust_acceptance', runtime: 'node', args: ['--test', 'scripts/verification/rust_qualification_catalog.test.mjs', 'scripts/verification/rust_test_check.test.mjs', 'scripts/verification/rust_acceptance.test.mjs'], cwd: '.', scopes: ['node', 'ui', 'mcp', 'dsh'] },
  { id: 'desktop_runner', command: 'npm run test:desktop_runner', runtime: 'node', args: ['--test', 'scripts/tauri_dev_config.test.mjs', 'scripts/tauri_build_config.test.mjs', 'scripts/tauri_build.test.mjs', 'scripts/ui_build_config.test.mjs', 'scripts/desktop_environment.test.mjs', 'scripts/desktop_native/pe_image.test.mjs', 'scripts/desktop_native/native_smoke.test.mjs'], cwd: '.', scopes: ['ui', 'mcp', 'dsh'] },
  { id: 'dsh_transport', command: 'npm run test:dsh_transport', runtime: 'node', args: ['--test', 'scripts/test_dsh_transport.mjs'], cwd: '.', scopes: ['dsh'] },
  { id: 'dsh_market', command: 'npm run test:dsh_market_catalog', runtime: 'node', args: ['--test', 'scripts/dsh_market/catalog_core.test.mjs'], cwd: '.', scopes: ['dsh'] },
  { id: 'ui_typecheck', command: 'npx tsc -b (ui)', runtime: 'node', args: ['node_modules/typescript/bin/tsc', '-b'], cwd: 'ui', scopes: ['ui', 'mcp', 'dsh'] },
  { id: 'ui_native_build', command: 'npm run ui:build (Vite stage)', runtime: 'node', args: ['node_modules/vite/bin/vite.js', 'build'], cwd: 'ui', scopes: ['ui', 'mcp', 'dsh'] },
  { id: 'ui_demo_build', command: 'npm run ui:build_demo (Vite stage)', runtime: 'node', args: ['node_modules/vite/bin/vite.js', 'build', '--mode', 'demo'], cwd: 'ui', scopes: ['ui', 'mcp', 'dsh'] },
  { id: 'ui_smoke', command: 'npm run ui:smoke', runtime: 'node', args: ['scripts/smoke-demo.mjs'], cwd: '.', scopes: ['ui', 'mcp', 'dsh'] },
  { id: 'ui_smoke_demo', command: 'node scripts/smoke-demo.mjs --built-demo', runtime: 'node', args: ['scripts/smoke-demo.mjs', '--built-demo'], cwd: '.', scopes: ['ui', 'mcp', 'dsh'] },
  { id: 'release_preflight', command: 'node scripts/release_preflight.mjs', runtime: 'node', args: ['scripts/release_preflight.mjs'], cwd: '.', scopes: ['ui', 'mcp', 'dsh'] },
  { id: 'privacy_audit', command: 'npm run privacy:audit', runtime: 'node', args: ['scripts/privacy/audit_repository.mjs'], cwd: '.', scopes: ['ui', 'mcp', 'dsh'] },
  { id: 'rust_check', command: 'cargo check --locked --offline', runtime: 'cargo', args: ['check', '--locked', '--offline'], cwd: 'src-tauri', scopes: ['mcp', 'dsh'] },
  { id: 'rust_tests', command: 'cargo test --locked --offline --lib -- --test-threads=1', runtime: 'cargo', args: ['test', '--locked', '--offline', '--lib', '--', '--test-threads=1'], cwd: 'src-tauri', scopes: ['mcp', 'dsh'] },
];

export function verification_plan(scope = 'all') {
  if (!feature_scopes.includes(scope)) throw new Error('AAG_INVALID_SCOPE');
  return {
    schema_version: 1, scope, mode: 'plan_only',
    preflight: { command: `npm run desktop:doctor -- --scope ${scope}`, mode: 'on_execution_only' },
    isolation: ['disposable AAG_CONFIG_DIR', 'disposable AAG_HOME_DIR', 'canonical disposable TMPDIR/TMP/TEMP', 'AAG_TEST_CCSWITCH_DB unset', 'no ignored live canaries'],
    exclusions: ['real providers', 'live client switching', 'DSH package installation', 'native installation', 'publication', 'recurring jobs'],
    checks: checks.filter((check) => scope === 'all' || check.scopes.includes(scope)).map(({ scopes, ...check }) => ({ ...check, args: [...check.args] })),
  };
}

export const safety_workflows = {
  mcp: {
    title: 'Review-bound MCP configuration',
    steps: ['Save a validated stdio or HTTP definition in AAG.', 'Choose client flags; this changes only the AAG registry.', 'Review a redacted Claude sync preview.', 'Explicitly approve before backup and atomic apply.', 'Restart Claude Code to load the configuration.'],
    boundaries: ['Codex configuration is preview-only.', 'Redacted previews are not runnable exports.', 'The registry does not install or execute MCP servers.', 'Disable and sync before deleting a definition to remove its client entry.'],
  },
  dsh: {
    title: 'Isolated DSH management',
    steps: ['Discover installed runtime metadata without running packages.', 'Review and apply setup for the AAG-managed profile.', 'Review each plugin operation and exact package version.', 'Start or stop the isolated runtime explicitly.'],
    boundaries: ['Native Claude files remain unchanged.', 'A plugin install can download and execute third-party code and needs user approval.', 'Browser demo evidence is not native runtime acceptance.', 'Only a supported manifest-declared runtime entry is accepted.'],
  },
  verification: {
    title: 'Feature-aware local verification',
    steps: ['Request a bounded feature plan.', 'Review commands and excluded external actions.', 'Run verify:features with --run from the trusted checkout.', 'Read every command outcome; stop on first failure.', 'Separate local, browser-demo, native, and external evidence.'],
    boundaries: ['The MCP companion cannot run the plan or grant approval.', 'Verification executes trusted repository test code.', 'No passing test count establishes live-provider or production readiness.'],
  },
};
