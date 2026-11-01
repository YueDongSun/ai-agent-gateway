//! Isolated native adapter proofs. This module is not production dispatch.

use super::*;
use std::{cell::Cell, fs, io::Read, os::windows::fs::OpenOptionsExt, time::Duration};

struct SourceObject {
    relative: PathBuf,
    metadata: EntryMetadata,
    file: File,
}

#[repr(C)]
struct FixtureFileTime {
    low: u32,
    high: u32,
}
#[link(name = "kernel32")]
extern "system" {
    fn SetFileTime(
        file: *mut c_void,
        created: *const FixtureFileTime,
        accessed: *const FixtureFileTime,
        written: *const FixtureFileTime,
    ) -> i32;
    fn LocalFree(memory: *mut c_void) -> *mut c_void;
}
#[link(name = "advapi32")]
extern "system" {
    fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
        text: *const u16,
        revision: u32,
        descriptor: *mut *mut c_void,
        bytes: *mut u32,
    ) -> i32;
    fn GetSecurityDescriptorSacl(
        descriptor: *const c_void,
        present: *mut i32,
        sacl: *mut *mut c_void,
        defaulted: *mut i32,
    ) -> i32;
    fn SetSecurityInfo(
        object: *mut c_void,
        object_type: u32,
        information: u32,
        owner: *mut c_void,
        group: *mut c_void,
        dacl: *mut c_void,
        sacl: *mut c_void,
    ) -> u32;
}

struct FixtureDescriptor(*mut c_void);
impl Drop for FixtureDescriptor {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                LocalFree(self.0);
            }
        }
    }
}

fn fixture_audit(file: &File) {
    let text: Vec<u16> = "S:(AU;SAFA;FR;;;WD)"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut raw = ptr::null_mut();
    assert_ne!(
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                text.as_ptr(),
                1,
                &mut raw,
                ptr::null_mut(),
            )
        },
        0
    );
    let descriptor = FixtureDescriptor(raw);
    let mut present = 0;
    let mut sacl = ptr::null_mut();
    let mut defaulted = 0;
    assert_ne!(
        unsafe { GetSecurityDescriptorSacl(descriptor.0, &mut present, &mut sacl, &mut defaulted) },
        0
    );
    assert_ne!(present, 0);
    assert!(!sacl.is_null());
    assert_eq!(
        unsafe {
            SetSecurityInfo(
                file.as_raw_handle(),
                1,
                8,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                sacl,
            )
        },
        0
    );
}

fn fixture_paths(root: &Path) -> MigrationPaths {
    let home = root.join("home");
    let source = home.join("source");
    let operation_root = home.join("operation_one");
    private_fs::prepare_private_dir(&source).unwrap();
    private_fs::prepare_private_dir(&operation_root).unwrap();
    MigrationPaths {
        home,
        source,
        destination: root.join("home/destination"),
        source_backup: operation_root.join("source_backup"),
        destination_archive: operation_root.join("destination_archive"),
        journal: operation_root.join("journal.json"),
        operation_root,
    }
}

fn open_source(platform: &WindowsStoragePlatform<'_>, path: &Path) -> File {
    platform.scope(path).unwrap();
    let file = fs::OpenOptions::new()
        .access_mode(0x8102_0189)
        .share_mode(3)
        .custom_flags(0x0220_0000)
        .open(path)
        .unwrap();
    let suppressed = FixtureFileTime {
        low: u32::MAX,
        high: u32::MAX,
    };
    assert_ne!(
        unsafe { SetFileTime(file.as_raw_handle(), ptr::null(), &suppressed, ptr::null()) },
        0
    );
    file
}

fn collect_sources(
    platform: &WindowsStoragePlatform<'_>,
    root: &Path,
    aliases: &[super::super::super::ChromeLatest],
) -> Vec<SourceObject> {
    fn walk(
        platform: &WindowsStoragePlatform<'_>,
        root: &Path,
        relative: PathBuf,
        aliases: &[super::super::super::ChromeLatest],
        output: &mut Vec<SourceObject>,
    ) {
        let path = root.join(&relative);
        let observed = platform.metadata(&path).unwrap();
        if observed.attributes & 0x400 != 0 {
            let alias = aliases
                .iter()
                .find(|alias| super::super::super::inventory::same(&alias.relative_path, &relative))
                .unwrap();
            assert!(super::super::super::inventory::same(
                &fs::read_link(&path).unwrap(),
                &alias.raw_target
            ));
            return;
        }
        let file = open_source(platform, &path);
        let metadata = metadata::capture_file(platform, &file).unwrap();
        assert_eq!(observed.object, metadata.object);
        let directory = metadata.attributes & 0x10 != 0;
        output.push(SourceObject {
            relative: relative.clone(),
            metadata,
            file,
        });
        if directory {
            let mut names = platform.directory_entries_for_inventory(&path).unwrap();
            names.sort();
            for name in names {
                walk(platform, root, relative.join(name), aliases, output);
            }
        }
    }
    let mut output = Vec::new();
    walk(platform, root, PathBuf::new(), aliases, &mut output);
    output.sort_by(|left, right| left.relative.cmp(&right.relative));
    for object in &mut output {
        object.metadata = metadata::capture_file(platform, &object.file).unwrap();
        object.metadata.accessed -= 864_000_000_000;
        file_times::restore_accessed(&object.file, object.metadata.accessed).unwrap();
    }
    output
}

fn borrowed(objects: &[SourceObject]) -> Vec<BackupObject<'_>> {
    objects
        .iter()
        .map(|object| BackupObject {
            relative: &object.relative,
            metadata: &object.metadata,
            file: &object.file,
        })
        .collect()
}

fn assert_sources_unchanged(
    platform: &WindowsStoragePlatform<'_>,
    objects: &[SourceObject],
    aliases: &[super::super::super::ChromeLatest],
    stage: &str,
) {
    let mut changed = [0usize; 9];
    let mut categories = [0usize; 4];
    let mut access_categories = [0usize; 4];
    for object in objects {
        let before = &object.metadata;
        let after = metadata::capture_file(platform, &object.file).unwrap();
        let flags = [
            before.object != after.object,
            before.attributes != after.attributes,
            before.created != after.created,
            before.accessed != after.accessed,
            before.written != after.written,
            before.unnamed_bytes != after.unnamed_bytes,
            before.security != after.security,
            before.extended_attributes != after.extended_attributes,
            before.streams != after.streams,
        ];
        for (count, changed) in changed.iter_mut().zip(flags) {
            *count += usize::from(changed);
        }
        let kind = if object.relative.as_os_str().is_empty() {
            0
        } else if before.attributes & 0x10 != 0 {
            1
        } else {
            2
        };
        let any = flags.iter().any(|changed| *changed);
        categories[kind] += usize::from(any);
        access_categories[kind] += usize::from(flags[3]);
        if aliases
            .iter()
            .any(|alias| alias.target_identity == before.object)
        {
            categories[3] += usize::from(any);
            access_categories[3] += usize::from(flags[3]);
        }
    }
    eprintln!("direct_source_stage={stage} object={} attrs={} created={} accessed={} written={} size={} security={} ea={} streams={} root={} directory={} file={} alias_target={} root_access={} directory_access={} file_access={} alias_target_access={}", changed[0], changed[1], changed[2], changed[3], changed[4], changed[5], changed[6], changed[7], changed[8], categories[0], categories[1], categories[2], categories[3], access_categories[0], access_categories[1], access_categories[2], access_categories[3]);
    assert!(
        changed.iter().all(|changed| *changed == 0),
        "same-handle source metadata changed at {stage}"
    );
}

#[test]
fn native_layouts_match_windows_sdk_field_offsets() {
    assert_eq!(std::mem::size_of::<BasicInformation>(), 40);
    assert_eq!(std::mem::offset_of!(BasicInformation, attributes), 32);
    assert_eq!(std::mem::offset_of!(ObjectAttributes, length), 0);
    #[cfg(target_pointer_width = "64")]
    {
        assert_eq!(std::mem::size_of::<UnicodeString>(), 16);
        assert_eq!(std::mem::offset_of!(UnicodeString, buffer), 8);
        assert_eq!(std::mem::size_of::<ObjectAttributes>(), 48);
        assert_eq!(std::mem::offset_of!(ObjectAttributes, name), 16);
        assert_eq!(std::mem::size_of::<IoStatus>(), 16);
    }
}

#[test]
fn native_context_abort_is_attempted_once_even_when_its_outcome_is_unknown() {
    let fixture = private_fs::private_temp_dir(&std::env::temp_dir()).unwrap();
    let path = fixture.path().join("context_fixture");
    fs::write(&path, b"synthetic context ownership").unwrap();
    let file = fs::File::open(&path).unwrap();
    for success in [0, 1] {
        let attempts = Cell::new(0);
        let mut context = BackupContext {
            file: &file,
            context: std::ptr::dangling_mut::<u8>().cast(),
            writing: false,
        };
        let result = context.release_with(|_, raw, _| {
            assert!(!(*raw).is_null());
            attempts.set(attempts.get() + 1);
            success
        });
        assert_eq!(result.is_ok(), success != 0);
        context
            .release_with(|_, _, _| {
                attempts.set(attempts.get() + 1);
                1
            })
            .unwrap();
        drop(context);
        assert_eq!(attempts.get(), 1);
    }
}

#[test]
fn backup_stream_control_headers_are_rejected_before_native_write() {
    let mut header = [0u8; STREAM_HEADER_BYTES];
    header[0..4].copy_from_slice(&5u32.to_le_bytes());
    assert_eq!(
        stream_header(&header),
        Err(MigrationError::BackupUnverified)
    );
    header[0..4].copy_from_slice(&1u32.to_le_bytes());
    header[4..8].copy_from_slice(&2u32.to_le_bytes());
    assert_eq!(
        stream_header(&header),
        Err(MigrationError::BackupUnverified)
    );
    header[4..8].copy_from_slice(&0x8000_0000u32.to_le_bytes());
    assert_eq!(
        stream_header(&header),
        Err(MigrationError::BackupUnverified)
    );
    header[4..8].fill(0);
    header[8..16].copy_from_slice(&(-1i64).to_le_bytes());
    assert_eq!(
        stream_header(&header),
        Err(MigrationError::BackupUnverified)
    );
}

#[test]
fn operator_old_directory_metadata_subreads_preserve_one_already_opened_object() {
    let fixture = private_fs::private_temp_dir(&std::env::temp_dir()).unwrap();
    let paths = fixture_paths(fixture.path());
    let cancel = AtomicBool::new(false);
    let deadline = Instant::now() + Duration::from_secs(30);
    let validator = |_: &Journal| Err(MigrationError::CallerCutoverRequired);
    let platform = WindowsStoragePlatform::new(paths.clone(), &cancel, deadline, &validator).unwrap();
    let directory = paths.source.join("metadata_fixture");
    private_fs::create_private_dir(&directory).unwrap();
    let file = private_fs::open_inventory_security_object(&directory).unwrap();
    let mut expected = metadata::capture_file(&platform, &file).unwrap();
    expected.accessed -= 864_000_000_000;
    file_times::restore_accessed(&file, expected.accessed).unwrap();
    eprintln!("native_metadata_microfixture stage=after_aging_before_same_handle_subreads");
    let after = metadata::capture_file(&platform, &file).unwrap();
    assert!(after == expected);
}

#[test]
fn operator_old_directory_reopen_observes_each_path_admission_boundary() {
    let fixture = private_fs::private_temp_dir(&std::env::temp_dir()).unwrap();
    let paths = fixture_paths(fixture.path());
    let cancel = AtomicBool::new(false);
    let deadline = Instant::now() + Duration::from_secs(30);
    let validator = |_: &Journal| Err(MigrationError::CallerCutoverRequired);
    let platform = WindowsStoragePlatform::new(paths.clone(), &cancel, deadline, &validator).unwrap();
    let directory = paths.source.join("reopen_fixture");
    private_fs::create_private_dir(&directory).unwrap();
    let observed = private_fs::open_inventory_security_object(&directory).unwrap();
    let mut expected = metadata::capture_file(&platform, &observed).unwrap();
    expected.accessed -= 864_000_000_000;
    file_times::restore_accessed(&observed, expected.accessed).unwrap();
    eprintln!("inventory_open_microfixture stage=after_aging_before_reopen");
    let reopened = private_fs::diagnose_inventory_security_object_open(&directory, &observed).unwrap();
    assert!(metadata::capture_file(&platform, &observed).unwrap() == expected);
    drop(reopened);
    eprintln!("inventory_open_microfixture stage=after_reopened_handle_close");
    assert!(metadata::capture_file(&platform, &observed).unwrap() == expected);
}

#[test]
fn operator_direct_backup_flat_object_preserves_reopened_images() {
    let fixture = private_fs::private_temp_dir(&std::env::temp_dir()).unwrap();
    let paths = fixture_paths(fixture.path());
    let cancel = AtomicBool::new(false);
    let deadline = Instant::now() + Duration::from_secs(30);
    let validator = |_: &Journal| Err(MigrationError::CallerCutoverRequired);
    let platform =
        WindowsStoragePlatform::new(paths.clone(), &cancel, deadline, &validator).unwrap();
    fs::write(
        paths.source.join("history.jsonl"),
        b"complete synthetic flat transcript\n",
    )
    .unwrap();
    let storage =
        super::super::super::NativeStorage::new(paths.clone(), &platform, &cancel, deadline)
            .unwrap();
    let objects = collect_sources(&platform, &paths.source, &[]);
    let source = super::super::super::inventory::scan(&storage, &paths.source, &[], &[]).unwrap();
    private_fs::create_private_dir(&paths.source_backup).unwrap();
    let backup_root = private_fs::inspect_directory_object(&paths.source_backup).unwrap();
    copy(
        &platform,
        &paths.source_backup,
        backup_root,
        &borrowed(&objects),
    )
    .unwrap();
    drop(objects);
    super::super::super::Storage::verify_snapshot(&storage, &paths.source, &source.snapshot)
        .unwrap();
    let backup =
        super::super::super::inventory::scan(&storage, &paths.source_backup, &[], &[]).unwrap();
    assert_eq!(backup.snapshot.image, source.snapshot.image);
    assert_eq!(backup.snapshot.entries, source.snapshot.entries);
    assert_eq!(backup.snapshot.bytes, source.snapshot.bytes);
}

#[test]
fn operator_direct_backup_streams_preserve_security_ea_ads_and_exact_images() {
    let fixture = private_fs::private_temp_dir(&std::env::temp_dir()).unwrap();
    let paths = fixture_paths(fixture.path());
    let cancel = AtomicBool::new(false);
    let deadline = Instant::now() + Duration::from_secs(60);
    let validator = |_: &Journal| Err(MigrationError::CallerCutoverRequired);
    let platform =
        WindowsStoragePlatform::new(paths.clone(), &cancel, deadline, &validator).unwrap();
    let transcript = paths.source.join("history.jsonl");
    fs::write(
        &transcript,
        b"synthetic native transfer bytes\n".repeat(BUFFER_BYTES / 16 + 3),
    )
    .unwrap();
    let writable = fs::OpenOptions::new()
        .write(true)
        .open(&transcript)
        .unwrap();
    extended_attributes::set_fixture_ea(&writable, b"AAG_FIXTURE_EA", b"native backup EA bytes")
        .unwrap();
    drop(writable);
    let audited = open_source(&platform, &transcript);
    fixture_audit(&audited);
    drop(audited);
    fs::write(
        paths.source.join("history.jsonl:fixture:$DATA"),
        b"named stream bytes",
    )
    .unwrap();
    fs::write(paths.source.join("history.jsonl:empty:$DATA"), b"").unwrap();
    fs::write(
        paths.source.join("history.jsonl:large:$DATA"),
        vec![b'z'; BUFFER_BYTES + 7],
    )
    .unwrap();
    fs::write(paths.source.join("empty.bin"), b"").unwrap();
    let version_relative = Path::new("plugins/cache/openai-bundled/chrome/fixture_version");
    let alias_relative = Path::new("plugins/cache/openai-bundled/chrome/latest");
    let version = paths.source.join(version_relative);
    fs::create_dir_all(&version).unwrap();
    fs::write(
        version.join("fixture.bin"),
        b"preserve concrete version bytes",
    )
    .unwrap();
    let alias = paths.source.join(alias_relative);
    fs::create_dir(&alias).unwrap();
    junction::create_fixture(&alias, &version).unwrap();
    let storage =
        super::super::super::NativeStorage::new(paths.clone(), &platform, &cancel, deadline)
            .unwrap();
    let admitted = super::super::super::inventory::scan(&storage, &paths.source, &[], &[]).unwrap();
    let objects = collect_sources(&platform, &paths.source, &admitted.snapshot.reparse_entries);
    assert_sources_unchanged(
        &platform,
        &objects,
        &admitted.snapshot.reparse_entries,
        "before_baseline_scan",
    );
    let source = super::super::super::inventory::scan(
        &storage,
        &paths.source,
        &admitted.snapshot.reparse_entries,
        &[],
    )
    .unwrap();
    assert_sources_unchanged(
        &platform,
        &objects,
        &admitted.snapshot.reparse_entries,
        "after_baseline_scan",
    );
    private_fs::create_private_dir(&paths.source_backup).unwrap();
    let backup_root = private_fs::inspect_directory_object(&paths.source_backup).unwrap();
    copy(
        &platform,
        &paths.source_backup,
        backup_root,
        &borrowed(&objects),
    )
    .unwrap();
    super::super::super::Storage::verify_snapshot(&storage, &paths.source, &source.snapshot)
        .unwrap();
    let backup =
        super::super::super::inventory::scan(&storage, &paths.source_backup, &[], &source.aliases)
            .unwrap();
    assert_eq!(backup.snapshot.root, backup_root);
    assert_eq!(backup.snapshot.image, source.snapshot.image);
    assert_eq!(backup.snapshot.entries, source.snapshot.entries);
    assert_eq!(backup.snapshot.bytes, source.snapshot.bytes);
    assert!(fs::symlink_metadata(paths.source_backup.join(alias_relative)).is_err());
    for object in &objects {
        assert!(metadata::capture_file(&platform, &object.file).unwrap() == object.metadata);
    }
    drop(objects);
    super::super::super::Storage::verify_snapshot(&storage, &paths.source, &source.snapshot)
        .unwrap();
    let reopened_backup =
        super::super::super::inventory::scan(&storage, &paths.source_backup, &[], &source.aliases)
            .unwrap();
    assert_eq!(reopened_backup.snapshot.image, source.snapshot.image);
}

#[test]
fn operator_direct_backup_rejects_preexisting_descendant_without_overwrite() {
    let fixture = private_fs::private_temp_dir(&std::env::temp_dir()).unwrap();
    let paths = fixture_paths(fixture.path());
    let cancel = AtomicBool::new(false);
    let deadline = Instant::now() + Duration::from_secs(30);
    let validator = |_: &Journal| Err(MigrationError::CallerCutoverRequired);
    let platform =
        WindowsStoragePlatform::new(paths.clone(), &cancel, deadline, &validator).unwrap();
    fs::write(paths.source.join("history.jsonl"), b"new source bytes").unwrap();
    let objects = collect_sources(&platform, &paths.source, &[]);
    private_fs::create_private_dir(&paths.source_backup).unwrap();
    let backup_root = private_fs::inspect_directory_object(&paths.source_backup).unwrap();
    let existing = paths.source_backup.join("history.jsonl");
    fs::write(&existing, b"retain unknown destination").unwrap();
    assert_eq!(
        copy(
            &platform,
            &paths.source_backup,
            backup_root,
            &borrowed(&objects)
        ),
        Err(MigrationError::ObjectConflict)
    );
    let mut bytes = Vec::new();
    private_fs::open_inventory_stream(&existing)
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    assert_eq!(bytes, b"retain unknown destination");
}

#[test]
fn operator_direct_backup_rejects_changed_source_before_destination_creation() {
    let fixture = private_fs::private_temp_dir(&std::env::temp_dir()).unwrap();
    let paths = fixture_paths(fixture.path());
    let cancel = AtomicBool::new(false);
    let deadline = Instant::now() + Duration::from_secs(30);
    let validator = |_: &Journal| Err(MigrationError::CallerCutoverRequired);
    let platform =
        WindowsStoragePlatform::new(paths.clone(), &cancel, deadline, &validator).unwrap();
    let transcript = paths.source.join("history.jsonl");
    fs::write(&transcript, b"admitted source bytes").unwrap();
    let objects = collect_sources(&platform, &paths.source, &[]);
    fs::write(&transcript, b"source bytes changed after admission").unwrap();
    private_fs::create_private_dir(&paths.source_backup).unwrap();
    let backup_root = private_fs::inspect_directory_object(&paths.source_backup).unwrap();
    assert_eq!(
        copy(
            &platform,
            &paths.source_backup,
            backup_root,
            &borrowed(&objects)
        ),
        Err(MigrationError::SourceChanged)
    );
    assert!(platform
        .directory_entries_for_inventory(&paths.source_backup)
        .unwrap()
        .is_empty());
}

#[test]
fn native_child_creation_rejects_ads_dot_and_case_collision_without_sentinel_changes() {
    let mut privileges = privileges::ProcessPrivileges::acquire().unwrap();
    let fixture = private_fs::private_temp_dir(&std::env::temp_dir()).unwrap();
    let root = fixture.path().join("destination");
    private_fs::create_private_dir(&root).unwrap();
    let sentinel = root.join("sentinel");
    let outside = fixture.path().join("outside_sentinel");
    fs::write(&sentinel, b"preserve sentinel data").unwrap();
    fs::write(&outside, b"preserve outside sibling").unwrap();
    let parent = fs::OpenOptions::new()
        .access_mode(DESTINATION_ACCESS)
        .share_mode(1)
        .custom_flags(0x0220_0000)
        .open(&root)
        .unwrap();
    for name in ["sentinel:qualification", ".", ".."] {
        assert!(matches!(
            create_child(&parent, std::ffi::OsStr::new(name), false),
            Err(MigrationError::InvalidPaths)
        ));
    }
    assert!(matches!(
        create_child(&parent, std::ffi::OsStr::new("SENTINEL"), false),
        Err(MigrationError::ObjectConflict)
    ));
    drop(parent);
    assert_eq!(fs::read(&sentinel).unwrap(), b"preserve sentinel data");
    assert_eq!(fs::read(&outside).unwrap(), b"preserve outside sibling");
    assert!(fs::symlink_metadata(root.join("sentinel:qualification")).is_err());
    privileges.restore().unwrap();
}

#[test]
fn operator_direct_backup_rejects_replaced_root_without_adopting_it() {
    let fixture = private_fs::private_temp_dir(&std::env::temp_dir()).unwrap();
    let paths = fixture_paths(fixture.path());
    let cancel = AtomicBool::new(false);
    let deadline = Instant::now() + Duration::from_secs(30);
    let validator = |_: &Journal| Err(MigrationError::CallerCutoverRequired);
    let platform =
        WindowsStoragePlatform::new(paths.clone(), &cancel, deadline, &validator).unwrap();
    fs::write(
        paths.source.join("history.jsonl"),
        b"complete admitted source\n",
    )
    .unwrap();
    let objects = collect_sources(&platform, &paths.source, &[]);
    private_fs::create_private_dir(&paths.source_backup).unwrap();
    let backup_root = private_fs::inspect_directory_object(&paths.source_backup).unwrap();
    let retained = paths.operation_root.join("retained_original_backup");
    fs::rename(&paths.source_backup, &retained).unwrap();
    private_fs::create_private_dir(&paths.source_backup).unwrap();
    assert_eq!(
        copy(
            &platform,
            &paths.source_backup,
            backup_root,
            &borrowed(&objects)
        ),
        Err(MigrationError::ObjectConflict)
    );
    assert!(platform
        .directory_entries_for_inventory(&paths.source_backup)
        .unwrap()
        .is_empty());
    assert_eq!(
        private_fs::inspect_directory_object(&retained).unwrap(),
        backup_root
    );
}
