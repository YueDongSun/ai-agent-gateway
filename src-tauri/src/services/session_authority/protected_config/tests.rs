//! No live service config is opened. Native cases own fresh private temp roots.

use super::*;
use crate::config::private_fs::{self, PrivateTempDir};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::windows::fs::OpenOptionsExt,
    path::PathBuf,
};

fn role() -> ServiceRole {
    ServiceRole::Reader
}
fn config_path(root: &PrivateTempDir) -> PathBuf {
    root.path()
        .join("runtime")
        .join(format!("{}.json", role().service_name()))
}
fn fixture(contents: &[u8]) -> PrivateTempDir {
    let root = private_fs::private_temp_dir(&std::env::temp_dir()).unwrap();
    private_fs::create_private_dir(&root.path().join("runtime")).unwrap();
    private_fs::atomic_write(&config_path(&root), contents).unwrap();
    root
}
fn ace(kind: u8, flags: u8, mask: u32) -> Vec<u8> {
    // Synthetic S-1-1-0. Callback application bytes are never evaluated.
    let size = if matches!(kind, 9 | 10) { 24u16 } else { 20u16 };
    let mut bytes = vec![kind, flags];
    bytes.extend(size.to_le_bytes());
    bytes.extend(mask.to_le_bytes());
    bytes.extend([1, 1, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0]);
    if size == 24 {
        bytes.extend([0x61, 0x72, 0x74, 0x78]);
    }
    bytes
}

#[test]
fn policy_owner_requires_the_policy_role_and_a_canonical_virtual_service_sid() {
    let virtual_sid = || Sid::parse("S-1-5-80-1-2-3-4-5").unwrap();
    assert!(ExpectedPolicyOwner::from_scm_role_lookup(ServiceRole::Reader, virtual_sid()).is_err());
    for value in [
        "S-1-5-18",
        "S-1-5-32-544",
        "S-1-5-21-1-2-3-1001",
        "S-1-5-80-1-2-3-4",
    ] {
        assert!(ExpectedPolicyOwner::from_scm_role_lookup(
            ServiceRole::Policy,
            Sid::parse(value).unwrap()
        )
        .is_err());
    }
    let expected =
        ExpectedPolicyOwner::from_scm_role_lookup(ServiceRole::Policy, virtual_sid()).unwrap();
    assert_eq!(expected.sid().value(), "S-1-5-80-1-2-3-4-5");
}

#[test]
fn bounded_acl_policy_allows_reads_and_unrelated_ancestor_creation_but_rejects_mutation() {
    for kind in [0, 9] {
        fixture_ace_check(&ace(kind, 0, 0x0012_0089), false, false).unwrap();
        fixture_ace_check(&ace(kind, 0, 0x0000_0006), false, true).unwrap();
        for mask in [
            0x0000_0002,
            0x0000_0004,
            0x0000_0100,
            0x0004_0000,
            0x0008_0000,
            0x0001_0000,
        ] {
            assert!(fixture_ace_check(&ace(kind, 0, mask), false, false).is_err());
        }
        for mask in [0x0000_0040, 0x0000_0100, 0x0004_0000, 0x0008_0000] {
            assert!(fixture_ace_check(&ace(kind, 0, mask), false, true).is_err());
        }
        fixture_ace_check(&ace(kind, 0, 0x001f_01ff), true, false).unwrap();
        fixture_ace_check(&ace(kind, 8, 0x001f_01ff), false, false).unwrap();
    }
    fixture_ace_check(&ace(1, 0, 0x001f_01ff), false, false).unwrap();
    fixture_ace_check(&ace(10, 0, 0x001f_01ff), false, false).unwrap();
    assert!(fixture_ace_check(&ace(11, 0, 0), true, false).is_err());
    let mut truncated = ace(9, 0, 1);
    truncated[9] = 15;
    assert!(fixture_ace_check(&truncated, true, false).is_err());
}

#[test]
fn native_fixture_returns_same_object_bytes_and_retains_a_readonly_guard() {
    let bytes = b"{\"schema_version\":1,\"fixture\":true}";
    let root = fixture(bytes);
    let before = private_fs::inventory_object_metadata(&config_path(&root)).unwrap();
    let snapshot = load_fixture(&root, role()).unwrap();
    assert_eq!(snapshot.0.contents, bytes);
    assert_eq!(snapshot.0.object, before.object);
    assert!(!snapshot.0.owner_sid.is_empty());
    assert!(OpenOptions::new()
        .write(true)
        .open(config_path(&root))
        .is_err());
    assert!(fs::rename(config_path(&root), root.path().join("moved.json")).is_err());
    assert!(fs::rename(
        root.path().join("runtime"),
        root.path().join("moved_runtime")
    )
    .is_err());
    let mut unrelated = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(root.path().join("unrelated.json"))
        .unwrap();
    unrelated.write_all(b"{}").unwrap();
    drop(unrelated);
    // A directory data-write handle could change a reparse point. The actual
    // protected directory guard must reject it, beyond final-file checks.
    assert!(OpenOptions::new()
        .access_mode(0x0000_0002)
        .share_mode(3)
        .custom_flags(NOFOLLOW_BACKUP_SEMANTICS)
        .open(root.path().join("runtime"))
        .is_err());
    // The metadata-only OS-ancestor guard permits atomic writes to a separate
    // child. This isolated temp root tests sharing behavior, not OS ownership.
    let higher = private_fs::private_temp_dir(&std::env::temp_dir()).unwrap();
    let higher_guard = open_object(higher.path(), true, false).unwrap();
    private_fs::atomic_write(&higher.path().join("ordinary_child.json"), b"{}").unwrap();
    drop(higher_guard);
    assert_eq!(fs::read(config_path(&root)).unwrap(), bytes);
    drop(snapshot);
    OpenOptions::new()
        .write(true)
        .open(config_path(&root))
        .unwrap();
}

#[test]
fn native_fixture_rejects_hardlinks_oversize_and_a_preexisting_writer() {
    let root = fixture(b"{}");
    let path = config_path(&root);
    let alias = root.path().join("alias.json");
    fs::hard_link(&path, &alias).unwrap();
    assert!(matches!(
        load_fixture(&root, role()),
        Err(ConfigError::ObjectInvalid)
    ));
    fs::remove_file(&alias).unwrap();
    private_fs::atomic_write(&path, &vec![b' '; MAX_CONFIG_BYTES + 1]).unwrap();
    assert!(matches!(
        load_fixture(&root, role()),
        Err(ConfigError::FileTooLarge)
    ));
    private_fs::atomic_write(&path, b"{}").unwrap();
    let writer = OpenOptions::new().write(true).open(&path).unwrap();
    assert!(matches!(
        load_fixture(&root, role()),
        Err(ConfigError::OpenFailed)
    ));
    drop(writer);
    load_fixture(&root, role()).unwrap();
}
