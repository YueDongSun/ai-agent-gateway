import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import {
  mirror_symlink_ids,
  ordinary_skip_ids,
  rust_qualification_plan,
  windows_qualification_groups,
  windows_qualification_ids,
} from './rust_qualification_catalog.mjs';

const source_by_group = new Map([
  ['windows_home_metadata', 'src-tauri/src/services/codex_home_migration/native_storage/windows_platform/tests.rs'],
  ['windows_backup_transfer', 'src-tauri/src/services/codex_home_migration/native_storage/windows_platform/backup_transfer_tests.rs'],
  ['windows_session_isolation', 'src-tauri/src/services/session_broker/windows_isolation/tests.rs'],
  ['windows_mirror_symlinks', 'src-tauri/src/services/mirror_sync/tests.rs'],
]);

function source_for(path) {
  return readFileSync(new URL(`../../${path}`, import.meta.url), 'utf8');
}

test('qualification manifest is a disjoint exact 26-test partition', () => {
  assert.deepEqual(windows_qualification_groups.map(({ test_ids }) => test_ids.length), [7, 8, 7, 4]);
  assert.equal(windows_qualification_ids.length, 26);
  assert.equal(new Set(windows_qualification_ids).size, 26);
  for (let left = 0; left < windows_qualification_groups.length; left++) {
    for (let right = left + 1; right < windows_qualification_groups.length; right++) {
      const left_ids = new Set(windows_qualification_groups[left].test_ids);
      assert.equal(windows_qualification_groups[right].test_ids.some((id) => left_ids.has(id)), false);
    }
  }
  assert.deepEqual(ordinary_skip_ids('win32'), [...windows_qualification_ids]);
  assert.deepEqual(rust_qualification_plan('windows_qualification', 'win32').windows_qualification.exact_test_ids, [...windows_qualification_ids]);
  assert.deepEqual(rust_qualification_plan('ordinary', 'win32').ordinary.excluded_test_ids, [...windows_qualification_ids]);
});

test('manifest test names exist in their current Rust source modules and are not ignored', () => {
  for (const group of windows_qualification_groups) {
    const source = source_for(source_by_group.get(group.id));
    for (const id of group.test_ids) {
      const function_name = id.split('::').at(-1);
      const declaration = source.indexOf(`fn ${function_name}(`);
      assert.notEqual(declaration, -1, `missing source test ${id}`);
      assert.equal(source.slice(Math.max(0, declaration - 100), declaration).includes('#[ignore'), false, `${id} must remain an ordinary runnable test`);
    }
  }
  assert.equal(windows_qualification_ids.some((id) => id.includes('isolated_authority_fixture') || id.includes('isolated_role_fixture')), false);
});

test('Unix ordinary profile retains all four mirror symlink tests', () => {
  const plan = rust_qualification_plan('ordinary', 'linux');
  assert.deepEqual(ordinary_skip_ids('linux'), []);
  assert.deepEqual(plan.ordinary.excluded_test_ids, []);
  assert.equal(mirror_symlink_ids.length, 4);
  assert.ok(mirror_symlink_ids.every((id) => windows_qualification_ids.includes(id)));
  const source = source_for(source_by_group.get('windows_mirror_symlinks'));
  assert.ok(mirror_symlink_ids.every((id) => source.includes(`fn ${id.split('::').at(-1)}(`)));
});

test('plan defaults to plan-only and identifies the evidence boundary', () => {
  const plan = rust_qualification_plan();
  assert.equal(plan.profile, 'all');
  assert.equal(plan.mode, 'plan_only');
  assert.equal(plan.evidence_class, 'local-only');
  assert.match(plan.acceptance_boundary, /not full product acceptance/);
  assert.throws(() => rust_qualification_plan('all; cargo test', 'win32'), /AAG_INVALID_RUST_PROFILE/);
});
