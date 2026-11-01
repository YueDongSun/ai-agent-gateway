use super::super::native::{
    ffi::*,
    sid::{descriptor_acl, descriptor_owner, sid_from_pointer, Descriptor},
};
use super::*;
use crate::config::private_fs::{
    inspect_directory_object, inventory_object_metadata, private_temp_dir, PrivateTempDir,
};
use std::{collections::BTreeMap, ptr::null_mut};
use std::{ffi::c_void, fs, os::windows::io::FromRawHandle};

fn roles() -> (Sid, Sid, Sid) {
    (
        Sid::parse("S-1-5-80-1-2-3-4-5").unwrap(),
        Sid::parse("S-1-5-80-6-7-8-9-10").unwrap(),
        Sid::parse("S-1-5-80-11-12-13-14-15").unwrap(),
    )
}
fn grants(sddl: &str) -> (String, BTreeMap<String, (u32, u8)>) {
    let descriptor = Descriptor::new(sddl).unwrap();
    let owner = descriptor_owner(descriptor.pointer())
        .unwrap()
        .value()
        .to_owned();
    let acl = unsafe { descriptor_acl(descriptor.pointer()).unwrap() };
    let mut info: AclSize = unsafe { std::mem::zeroed() };
    assert_ne!(
        unsafe {
            GetAclInformation(
                acl,
                (&mut info as *mut AclSize).cast(),
                std::mem::size_of::<AclSize>() as u32,
                2,
            )
        },
        0
    );
    let mut grants = BTreeMap::new();
    for index in 0..info.count {
        let mut raw = null_mut();
        assert_ne!(unsafe { GetAce(acl, index, &mut raw) }, 0);
        let ace = unsafe { &*raw.cast::<AllowedAce>() };
        assert_eq!(ace.header.kind, 0);
        let sid =
            unsafe { sid_from_pointer((&ace.sid_start as *const u32).cast_mut().cast()).unwrap() };
        assert!(grants
            .insert(sid.value().to_owned(), (ace.mask, ace.header.flags))
            .is_none());
    }
    (owner, grants)
}

#[test]
fn exact_roles_exclude_parent_delete_acl_reset_and_ambient_groups() {
    let (owner, reader, writer) = roles();
    let policy = PrincipalStoragePolicy::new(&owner, &reader, Some(&writer)).unwrap();
    let (actual_owner, actual) = grants(&policy.directory_sddl().unwrap());
    assert_eq!(actual_owner, owner.value());
    let expected = BTreeMap::from([
        (owner.value().to_owned(), (0x001f_01ff, 3)),
        ("S-1-5-18".to_owned(), (0x001f_01ff, 3)),
        ("S-1-5-32-544".to_owned(), (0x001f_01ff, 3)),
        ("S-1-3-4".to_owned(), (0x0002_0000, 3)),
        (reader.value().to_owned(), (0x0012_00a9, 3)),
        (writer.value().to_owned(), (0x0013_00bf, 3)),
    ]);
    assert_eq!(actual, expected);
    assert_eq!(
        actual[writer.value()].0 & (0x40 | 0x100 | 0x40000 | 0x80000),
        0
    );
    let (_, files) = grants(&policy.file_sddl().unwrap());
    assert!(files.values().all(|(_, flags)| *flags == 0));
    assert_eq!(files[writer.value()].0, 0x0013_01bf);
}

#[test]
fn read_only_generation_has_no_writer_grant() {
    let (owner, reader, writer) = roles();
    let policy = PrincipalStoragePolicy::new(&owner, &reader, None).unwrap();
    let (_, actual) = grants(&policy.directory_sddl().unwrap());
    assert_eq!(actual.len(), 5);
    assert_eq!(actual[reader.value()].0, 0x0012_00a9);
    assert!(!actual.contains_key(writer.value()));
}

#[test]
fn role_aliases_and_non_virtual_identities_are_rejected() {
    let (owner, reader, writer) = roles();
    PrincipalStoragePolicy::new(&owner, &reader, Some(&writer)).unwrap();
    assert!(PrincipalStoragePolicy::new(&owner, &owner, None).is_err());
    assert!(PrincipalStoragePolicy::new(&owner, &reader, Some(&owner)).is_err());
    assert!(PrincipalStoragePolicy::new(&owner, &reader, Some(&reader)).is_err());
    for bad in ["S-1-1-0", "S-1-5-80-0", "S-1-5-5-1-2", "S-1-5-32-544"] {
        let bad = Sid::parse(bad).unwrap();
        assert!(PrincipalStoragePolicy::new(&bad, &reader, None).is_err());
        assert!(PrincipalStoragePolicy::new(&owner, &bad, None).is_err());
        assert!(PrincipalStoragePolicy::new(&owner, &reader, Some(&bad)).is_err());
    }
}

fn fixture() -> PrivateTempDir {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/principal_storage_fixtures");
    fs::create_dir_all(&base).unwrap();
    private_temp_dir(&base).unwrap()
}

fn create_owned_fixture_file(policy: &PrincipalStoragePolicy, path: &Path) {
    #[repr(C)]
    struct SecurityAttributes {
        length: u32,
        descriptor: *mut c_void,
        inherit: i32,
    }
    let descriptor = Descriptor::new(&policy.file_sddl().unwrap()).unwrap();
    let security = SecurityAttributes {
        length: std::mem::size_of::<SecurityAttributes>() as u32,
        descriptor: descriptor.pointer(),
        inherit: 0,
    };
    let path = super::super::native::sid::wide(path.to_str().unwrap()).unwrap();
    let raw = unsafe {
        CreateFileW(
            path.as_ptr(),
            0x4000_0000,
            3,
            (&security as *const SecurityAttributes).cast(),
            1,
            0x0020_0000,
            null_mut(),
        )
    };
    assert_ne!(raw as isize, -1);
    drop(unsafe { fs::File::from_raw_handle(raw) });
}

#[test]
fn fresh_root_is_owned_at_creation_and_qualified_from_actual_security() {
    let fixture = fixture();
    let policy = PrincipalStoragePolicy::for_fixture(&fixture).unwrap();
    let root = policy.create_fixture_directory(&fixture, "stage").unwrap();
    policy.inspect_owned_root_actual(&root).unwrap();
    assert_eq!(
        inspect_directory_object(root.path()).unwrap(),
        root.identity()
    );
    assert!(fs::rename(root.path(), fixture.path().join("moved")).is_err());
    let path = root.path().to_owned();
    drop(root);
    fs::rename(&path, fixture.path().join("moved")).unwrap();
}

#[test]
fn occupied_original_directory_is_never_adopted_or_replaced() {
    let fixture = fixture();
    let policy = PrincipalStoragePolicy::for_fixture(&fixture).unwrap();
    let existing = fixture.path().join("existing");
    fs::create_dir(&existing).unwrap();
    fs::write(existing.join("original"), b"preserve original").unwrap();
    let before = inspect_directory_object(&existing).unwrap();
    drop(
        policy
            .create_fixture_directory(&fixture, "positive")
            .unwrap(),
    );
    assert!(policy
        .create_fixture_directory(&fixture, "existing")
        .is_err());
    assert_eq!(inspect_directory_object(&existing).unwrap(), before);
    assert_eq!(
        fs::read(existing.join("original")).unwrap(),
        b"preserve original"
    );
    for name in ["..", ".", "nested/leaf", "NUL", "trailing.", "name:stream"] {
        assert!(policy.create_fixture_directory(&fixture, name).is_err());
    }
}

#[test]
fn fresh_object_application_is_identity_bound_and_rejects_hardlink_aliases() {
    // The destination directory must not itself be namespace-leased: link
    // creation changes that directory's entries. Both roots are fresh fixtures.
    let alias_fixture = fixture();
    let fixture = fixture();
    let policy = PrincipalStoragePolicy::for_fixture(&fixture).unwrap();
    let root = policy.create_fixture_directory(&fixture, "stage").unwrap();
    let file = root.path().join("payload");
    create_owned_fixture_file(&policy, &file);
    let identity = inventory_object_metadata(&file).unwrap().object;
    policy
        .apply_fixture_object(&root, Path::new("payload"), identity)
        .unwrap();
    let wrong = Managed_directory_identity {
        volume: identity.volume,
        object: identity.object ^ 1,
    };
    assert!(policy
        .apply_fixture_object(&root, Path::new("payload"), wrong)
        .is_err());
    assert!(policy
        .apply_fixture_object(&root, Path::new("../outside"), identity)
        .is_err());
    fs::hard_link(&file, alias_fixture.path().join("hardlink_alias")).unwrap();
    assert!(policy
        .apply_fixture_object(&root, Path::new("payload"), identity)
        .is_err());
}

#[test]
fn held_object_guard_blocks_target_rename_and_data_writes_until_drop() {
    let fixture = fixture();
    let policy = PrincipalStoragePolicy::for_fixture(&fixture).unwrap();
    let root = policy.create_fixture_directory(&fixture, "stage").unwrap();
    let file = root.path().join("payload");
    let moved = root.path().join("moved_payload");
    create_owned_fixture_file(&policy, &file);
    fs::write(&file, b"synthetic original").unwrap();
    // Isolate the target-file guard from the containing directory leases.
    // Keeping FreshRoot alive would independently refuse entry renames.
    drop(root);
    let object = open_object(&file, true).unwrap();
    assert!(fs::rename(&file, &moved).is_err());
    assert!(fs::OpenOptions::new().write(true).open(&file).is_err());
    assert_eq!(fs::read(&file).unwrap(), b"synthetic original");
    drop(object);
    fs::rename(&file, &moved).unwrap();
    fs::write(&moved, b"synthetic replacement").unwrap();
    assert_eq!(fs::read(&moved).unwrap(), b"synthetic replacement");
}
