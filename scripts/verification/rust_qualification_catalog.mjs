const codex_home_tests = 'services::codex_home_migration::native_storage::windows_platform::tests';
const backup_transfer_tests = 'services::codex_home_migration::native_storage::windows_platform::backup_transfer::tests';
const session_isolation_tests = 'services::session_broker::windows_isolation::tests';
const mirror_sync_tests = 'services::mirror_sync::tests';

export const windows_qualification_groups = Object.freeze([
  Object.freeze({
    id: 'windows_home_metadata',
    test_ids: Object.freeze([
      `${codex_home_tests}::operator_metadata_preserves_real_security_ea_ads_and_access_times`,
      `${codex_home_tests}::operator_known_chrome_junction_retargets_in_place_and_rolls_back_with_metadata`,
      `${codex_home_tests}::operator_robocopy_backup_is_independently_verified_with_ea_ads_and_omitted_alias_receipt`,
      `${codex_home_tests}::operator_installed_validation_delegates_the_exact_journal_to_parent`,
      `${codex_home_tests}::operator_durable_memento_recovers_access_times_after_adapter_restart`,
      `${codex_home_tests}::operator_access_recovery_rejects_non_access_changes_before_any_timestamp_write`,
      `${codex_home_tests}::operator_old_directory_access_times_survive_repeated_whole_home_inventory`,
    ]),
  }),
  Object.freeze({
    id: 'windows_backup_transfer',
    test_ids: Object.freeze([
      `${backup_transfer_tests}::operator_old_directory_metadata_subreads_preserve_one_already_opened_object`,
      `${backup_transfer_tests}::operator_old_directory_reopen_observes_each_path_admission_boundary`,
      `${backup_transfer_tests}::operator_direct_backup_flat_object_preserves_reopened_images`,
      `${backup_transfer_tests}::operator_direct_backup_streams_preserve_security_ea_ads_and_exact_images`,
      `${backup_transfer_tests}::operator_direct_backup_rejects_preexisting_descendant_without_overwrite`,
      `${backup_transfer_tests}::operator_direct_backup_rejects_changed_source_before_destination_creation`,
      `${backup_transfer_tests}::native_child_creation_rejects_ads_dot_and_case_collision_without_sentinel_changes`,
      `${backup_transfer_tests}::operator_direct_backup_rejects_replaced_root_without_adopting_it`,
    ]),
  }),
  Object.freeze({
    id: 'windows_session_isolation',
    test_ids: Object.freeze([
      `${session_isolation_tests}::reader_can_read_but_cannot_append_delete_rename_or_reset_acl`,
      `${session_isolation_tests}::writer_a_cannot_write_delete_or_rename_scope_b`,
      `${session_isolation_tests}::revocation_blocks_new_writes_using_a_retained_writer_token`,
      `${session_isolation_tests}::restricted_child_cannot_read_duplicate_or_inject_into_unrestricted_authority`,
      `${session_isolation_tests}::nested_restriction_cannot_add_a_different_source_write_capability`,
      `${session_isolation_tests}::deleted_privileges_cannot_be_reenabled`,
      `${session_isolation_tests}::restricted_token_cannot_get_set_or_suspend_authority_threads`,
    ]),
  }),
  Object.freeze({
    id: 'windows_mirror_symlinks',
    test_ids: Object.freeze([
      `${mirror_sync_tests}::trusted_top_level_skill_links_are_materialized_and_target_updates_are_observed`,
      `${mirror_sync_tests}::escaped_or_nested_source_links_block_the_whole_inventory_before_deletions`,
      `${mirror_sync_tests}::destination_links_never_redirect_an_instruction_write`,
      `${mirror_sync_tests}::missing_mirror_below_an_existing_source_alias_is_rejected_before_creation`,
    ]),
  }),
]);

export const windows_qualification_ids = Object.freeze(
  windows_qualification_groups.flatMap(({ test_ids }) => test_ids),
);

export const mirror_symlink_ids = windows_qualification_groups
  .find(({ id }) => id === 'windows_mirror_symlinks')
  .test_ids;

const accepted_profiles = Object.freeze(['ordinary', 'windows_qualification', 'all']);

export function ordinary_skip_ids(platform = process.platform) {
  return platform === 'win32' ? [...windows_qualification_ids] : [];
}

export function rust_qualification_plan(profile = 'all', platform = process.platform) {
  if (!accepted_profiles.includes(profile)) throw new Error('AAG_INVALID_RUST_PROFILE');
  if (typeof platform !== 'string' || platform.length === 0) throw new Error('AAG_INVALID_PLATFORM');

  const ordinary = {
    id: 'ordinary',
    status: 'planned',
    command: 'cargo test --locked --offline --lib --color never -- --test-threads=1',
    excluded_test_ids: ordinary_skip_ids(platform),
    test_selection: 'all ordinary library tests; ignored tests remain ignored',
  };
  const windows_qualification = {
    id: 'windows_qualification',
    status: 'planned',
    required_platform: 'win32',
    exact_test_ids: [...windows_qualification_ids],
    unrun_test_ids: [...windows_qualification_ids],
    prerequisites: [
      'original primary process token; no thread impersonation or token restrictions',
      'TOKEN_ALL_ACCESS open and exactly one enabled, non-deny logon SID',
      'SeBackupPrivilege, SeRestorePrivilege, and SeSecurityPrivilege assigned and query/adjust-access permitted',
      'System32 Robocopy help satisfies the native adapter contract',
      'directory symlink creation succeeds in a disposable fixture without changing Developer Mode',
    ],
  };

  return {
    schema_version: 1,
    profile,
    mode: 'plan_only',
    platform,
    evidence_class: 'local-only',
    ordinary: profile === 'windows_qualification' ? undefined : ordinary,
    windows_qualification: profile === 'ordinary' ? undefined : windows_qualification,
    acceptance_boundary: 'Rust test profile evidence only; not full product acceptance',
  };
}
