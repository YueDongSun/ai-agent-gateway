use super::*;
use crate::services::owned_process::{encode_environment, run_capture, NativeChild, SpawnSpec};
use native::{
    ffi::*,
    sid::{wide, BinarySid, Descriptor},
    token::{
        own, token_privileges, token_user, Authority, AuthorityGroupMode, FixtureObjectProfile,
        FixtureSystemProfile,
    },
};
use std::{
    ffi::OsString,
    fs,
    io::{Read, Write},
    os::windows::io::AsRawHandle,
    path::{Path, PathBuf},
    ptr::{null, null_mut},
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

pub(super) const AUTHORITY_FIXTURE: &str =
    "services::session_broker::windows_isolation::tests::isolated_authority_fixture";
const ROLE_FIXTURE: &str =
    "services::session_broker::windows_isolation::tests::isolated_role_fixture";
const OUTPUT_LIMIT: usize = 64 * 1024;
const FIXTURE_DEADLINE: Duration = Duration::from_secs(30);

pub(super) fn fixture_base() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("session_isolation_fixtures")
}
fn source_id(value: &str) -> CanonicalSessionId {
    CanonicalSessionId(value.into())
}
fn capabilities(value: &str) -> SourceCapabilities {
    SourceCapabilities::derive(&source_id(value), 0).unwrap()
}

#[test]
fn capability_sids_are_source_and_role_specific() {
    let a = capabilities("fixture-source-a");
    let b = capabilities("fixture-source-b");
    let again = capabilities("fixture-source-a");
    assert!(a.read_sid() != a.write_sid());
    assert!(a.read_sid() != b.read_sid());
    assert!(a.write_sid() != b.write_sid());
    assert!(a.read_sid() == again.read_sid() && a.write_sid() == again.write_sid());
    let next = SourceCapabilities::derive(&source_id("fixture-source-a"), 1).unwrap();
    assert!(a.read_sid() != next.read_sid() && a.write_sid() != next.write_sid());
    assert!(a.read_sid() != capabilities("Fixture-source-a").read_sid());
    assert!(Sid::parse("BA").is_err());
    assert!(Sid::parse("S-1-5-21-1-2-3-1001\0ignored").is_err());
}

#[test]
fn storage_descriptor_suppresses_owner_dac_and_limits_capability_masks() {
    let owner = Sid::parse("S-1-5-80-1-2-3-4-5").unwrap();
    let runtime = Sid::parse("S-1-5-80-6-7-8-9-10").unwrap();
    let caps = capabilities("fixture-source-a");
    let writer = RoleStorageAcl::new(&owner, &runtime, &caps, true).unwrap();
    assert!(writer.sddl().contains("(A;OICI;RC;;;OW)"));
    assert!(writer
        .sddl()
        .contains(&format!("(A;OICI;0x001301ff;;;{})", runtime.value())));
    let reader = RoleStorageAcl::new(&owner, &runtime, &caps, false).unwrap();
    assert!(!reader.sddl().contains(caps.write_sid().value()));
    assert!(reader
        .sddl()
        .contains(&format!("(A;OICI;GRGX;;;{})", runtime.value())));
    assert!(RoleStorageAcl::new(&owner, &owner, &caps, true).is_err());
    for value in ["S-1-1-0", "S-1-5-33", "S-1-5-5-1-2", "S-1-5-80-1-2-3-4"] {
        let invalid = Sid::parse(value).unwrap();
        assert!(RoleStorageAcl::new(&owner, &invalid, &caps, true).is_err());
        assert!(RoleStorageAcl::new(&invalid, &runtime, &caps, true).is_err());
    }
}

struct FixtureDirectory {
    root: PathBuf,
}
impl FixtureDirectory {
    fn create() -> (Self, String) {
        let mut bytes = [0u8; 16];
        getrandom::getrandom(&mut bytes).unwrap();
        let nonce: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        let base = fixture_base();
        fs::create_dir_all(&base).unwrap();
        let root = base.join(&nonce);
        // TokenOwner can be Administrators in an elevated harness. Create the
        // exclusive fixture with the actual TokenUser as its explicit owner;
        // do not adopt an existing directory or mutate the harness token.
        let owner = token_user((-4isize) as Handle).unwrap();
        create_owned_directory(&root, &owner);
        (Self { root }, nonce)
    }
}

#[repr(C)]
struct FixtureSecurityAttributes {
    length: u32,
    descriptor: *mut std::ffi::c_void,
    inherit: i32,
}
fn fixture_creation_security(owner: &Sid) -> Descriptor {
    Descriptor::new(&format!(
        "O:{}D:P(A;OICI;FA;;;{})(A;OICI;RC;;;OW)",
        owner.value(),
        owner.value()
    ))
    .unwrap()
}
fn create_owned_directory(path: &Path, owner: &Sid) {
    let descriptor = fixture_creation_security(owner);
    let attributes = FixtureSecurityAttributes {
        length: std::mem::size_of::<FixtureSecurityAttributes>() as u32,
        descriptor: descriptor.pointer(),
        inherit: 0,
    };
    use std::os::windows::ffi::OsStrExt;
    let name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    assert_ne!(
        unsafe {
            CreateDirectoryW(
                name.as_ptr(),
                (&attributes as *const FixtureSecurityAttributes).cast(),
            )
        },
        0,
        "exclusive fixture directory creation failed"
    );
}
fn create_owned_payload(path: &Path, owner: &Sid) {
    let descriptor = fixture_creation_security(owner);
    let attributes = FixtureSecurityAttributes {
        length: std::mem::size_of::<FixtureSecurityAttributes>() as u32,
        descriptor: descriptor.pointer(),
        inherit: 0,
    };
    use std::os::windows::ffi::OsStrExt;
    let name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let handle = own(
        unsafe {
            CreateFileW(
                name.as_ptr(),
                0x4002_0000,
                3,
                (&attributes as *const FixtureSecurityAttributes).cast(),
                1,
                0x80,
                null_mut(),
            )
        },
        IsolationError::FixtureFailed,
    )
    .unwrap();
    let mut bytes = native::sid::security_descriptor(handle.as_raw_handle()).unwrap();
    assert!(
        native::sid::descriptor_owner(bytes.as_mut_ptr().cast()).unwrap() == *owner,
        "fresh payload did not retain the explicit owner"
    );
    let mut file = fs::File::from(handle);
    file.write_all(b"fixture").unwrap();
}
impl Drop for FixtureDirectory {
    fn drop(&mut self) {
        // Only the exact task-created nonce directory is eligible for removal.
        if self.root.parent() == Some(fixture_base().as_path())
            && self
                .root
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name.len() == 32 && name.bytes().all(|byte| byte.is_ascii_hexdigit())
                })
            && fs::canonicalize(&self.root).is_ok_and(|root| {
                fs::canonicalize(fixture_base())
                    .is_ok_and(|base| root.parent() == Some(base.as_path()))
            })
        {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}
fn fixture_args(name: &str) -> Vec<String> {
    vec![
        "--exact".into(),
        name.into(),
        "--ignored".into(),
        "--nocapture".into(),
        "--test-threads=1".into(),
    ]
}
fn environment(pairs: &[(&str, String)]) -> Vec<u16> {
    let mut values: Vec<(OsString, OsString)> = pairs
        .iter()
        .map(|(key, value)| (key.into(), value.into()))
        .collect();
    if let Some(root) = std::env::var_os("SystemRoot") {
        values.push(("SystemRoot".into(), root));
    }
    encode_environment(&values).unwrap()
}

fn selected_profile() -> (FixtureSystemProfile, &'static str) {
    match std::env::var("AAG_ISOLATION_FIXTURE_PROFILE").as_deref() {
        Err(std::env::VarError::NotPresent) | Ok("source_only") => {
            (FixtureSystemProfile::SourceOnly, "source_only")
        }
        Ok("write_restricted_code") => (
            FixtureSystemProfile::WriteRestrictedCode,
            "write_restricted_code",
        ),
        Ok("service_runtime") => (FixtureSystemProfile::ServiceRuntime, "service_runtime"),
        _ => panic!("unknown isolated fixture profile"),
    }
}

fn selected_child() -> &'static str {
    match std::env::var("AAG_ISOLATION_FIXTURE_CHILD").as_deref() {
        Err(std::env::VarError::NotPresent) | Ok("tauri") => "tauri",
        Ok("minimal") => "minimal",
        _ => panic!("unknown isolated fixture child"),
    }
}

fn selected_objects() -> (FixtureObjectProfile, &'static str) {
    match std::env::var("AAG_ISOLATION_FIXTURE_OBJECTS").as_deref() {
        Err(std::env::VarError::NotPresent) | Ok("protected") => {
            (FixtureObjectProfile::Protected, "protected")
        }
        Ok("child_query") => (FixtureObjectProfile::ChildQuery, "child_query"),
        Ok("default_objects") => (FixtureObjectProfile::DefaultObjects, "default_objects"),
        _ => panic!("unknown isolated fixture object profile"),
    }
}

fn selected_logon() -> (AuthorityGroupMode, &'static str) {
    match std::env::var("AAG_ISOLATION_FIXTURE_LOGON").as_deref() {
        Err(std::env::VarError::NotPresent) | Ok("deny_only") => {
            (AuthorityGroupMode::DenyOnly, "deny_only")
        }
        Ok("enabled") => (AuthorityGroupMode::KeepEnabled, "enabled"),
        _ => panic!("unknown isolated fixture logon profile"),
    }
}

fn role_program() -> (PathBuf, Vec<String>) {
    if selected_child() == "minimal" {
        let executable = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target/session_fabric/session_role_fixture.exe");
        let metadata = fs::symlink_metadata(&executable).unwrap();
        use std::os::windows::fs::MetadataExt;
        assert!(
            metadata.is_file() && metadata.file_attributes() & 0x400 == 0,
            "minimal fixture must be the regular parent-built workspace executable"
        );
        (executable, Vec::new())
    } else {
        (std::env::current_exe().unwrap(), fixture_args(ROLE_FIXTURE))
    }
}

fn fixture_token(
    authority: &CallerAuthority,
    source: &SourceCapabilities,
    role: CoreRole,
) -> RoleToken {
    RoleToken {
        token: authority
            .inner
            .restrict_fixture_system(
                source,
                role,
                selected_profile().0,
                selected_objects().0,
                selected_logon().0,
            )
            .unwrap(),
        role,
        source_id: source.source_id.clone(),
        generation: source.generation,
    }
}

fn isolated_fixture(scenario: &str) {
    let (directory, nonce) = FixtureDirectory::create();
    let profile = selected_profile().1;
    let child = selected_child();
    let objects = selected_objects().1;
    let logon = selected_logon().1;
    println!("AAG_ISOLATION_CANDIDATE profile={profile} objects={objects} logon={logon} child={child} scenario={scenario}");
    let environment = environment(&[
        ("AAG_ISOLATION_MODE", "authority".into()),
        ("AAG_ISOLATION_NONCE", nonce),
        ("AAG_ISOLATION_SCENARIO", scenario.into()),
        ("AAG_ISOLATION_FIXTURE_PROFILE", profile.into()),
        ("AAG_ISOLATION_FIXTURE_CHILD", child.into()),
        ("AAG_ISOLATION_FIXTURE_OBJECTS", objects.into()),
        ("AAG_ISOLATION_FIXTURE_LOGON", logon.into()),
    ]);
    let executable = std::env::current_exe().unwrap();
    let args = fixture_args(AUTHORITY_FIXTURE);
    let spec = SpawnSpec {
        environment: Some(&environment),
        cwd: Some(&directory.root),
        ..SpawnSpec::new(&executable, &args)
    };
    let output = run_capture(
        &spec,
        &AtomicBool::new(false),
        Instant::now() + FIXTURE_DEADLINE,
        OUTPUT_LIMIT,
    )
    .unwrap();
    // The only failure payload is synthetic fixture assertion/Win32 status.
    assert_eq!(
        output.exit_code,
        0,
        "isolated authority failed: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("AAG_ISOLATION_RESULT_OK"));
}
#[test]
fn reader_can_read_but_cannot_append_delete_rename_or_reset_acl() {
    isolated_fixture("reader");
}
#[test]
fn writer_a_cannot_write_delete_or_rename_scope_b() {
    isolated_fixture("writer");
}
#[test]
fn revocation_blocks_new_writes_using_a_retained_writer_token() {
    isolated_fixture("revoked");
}
#[test]
fn restricted_child_cannot_read_duplicate_or_inject_into_unrestricted_authority() {
    isolated_fixture("authority");
}
#[test]
fn nested_restriction_cannot_add_a_different_source_write_capability() {
    isolated_fixture("nested");
}
#[test]
fn deleted_privileges_cannot_be_reenabled() {
    isolated_fixture("privileges");
}

#[test]
fn restricted_token_cannot_get_set_or_suspend_authority_threads() {
    isolated_fixture("authority_threads");
}

struct HeldThread {
    id: u32,
    release: Option<std::sync::mpsc::Sender<()>>,
    join: Option<std::thread::JoinHandle<()>>,
}
impl HeldThread {
    fn start() -> Self {
        let (ready, receive_ready) = std::sync::mpsc::channel();
        let (release, receive_release) = std::sync::mpsc::channel();
        let join = std::thread::spawn(move || {
            ready.send(unsafe { GetCurrentThreadId() }).unwrap();
            let _ = receive_release.recv_timeout(Duration::from_secs(15));
        });
        Self {
            id: receive_ready.recv_timeout(Duration::from_secs(3)).unwrap(),
            release: Some(release),
            join: Some(join),
        }
    }
}
impl Drop for HeldThread {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn assert_thread_power_denied_under_role(token: &RoleToken, ids: &[u32]) {
    struct Impersonation;
    impl Drop for Impersonation {
        fn drop(&mut self) {
            assert_ne!(unsafe { RevertToSelf() }, 0);
        }
    }
    assert_ne!(
        unsafe { ImpersonateLoggedOnUser(token.token().as_handle().as_raw_handle()) },
        0
    );
    let impersonation = Impersonation;
    let mut observed = Vec::new();
    for id in ids {
        for access in [0x8, 0x10, 0x2, 0x20, 0x80, 0x100, 0x200, 0x40000, 0x80000] {
            let handle = unsafe { OpenThread(access, 0, *id) };
            let error = unsafe { GetLastError() };
            let allowed = !handle.is_null();
            if allowed {
                drop(own(handle, IsolationError::FixtureFailed).unwrap());
            }
            observed.push((access, allowed, error));
        }
    }
    // Assert only after restoring the isolated authority's original context.
    drop(impersonation);
    for (access, allowed, error) in observed {
        assert!(
            !allowed,
            "authority thread access {access:#x} unexpectedly granted"
        );
        assert_eq!(
            error, 5,
            "thread-denial probe must address a live owned thread"
        );
    }
}

fn fixture_acl(
    authority: &CallerAuthority,
    caps: &SourceCapabilities,
    writable: bool,
) -> RoleStorageAcl {
    RoleStorageAcl::build(
        authority.owner_sid(),
        authority.owner_sid(),
        caps,
        writable,
        true,
    )
    .unwrap()
}
fn append(path: &Path) -> std::io::Result<()> {
    fs::OpenOptions::new()
        .append(true)
        .open(path)?
        .write_all(b"+")
}

fn run_role(
    token: &crate::services::owned_process::OwnedToken,
    mode: &str,
    root: &Path,
    peer: u32,
) {
    let environment = environment(&[
        ("AAG_ISOLATION_MODE", "role".into()),
        ("AAG_ISOLATION_ROLE", mode.into()),
        ("AAG_ISOLATION_PARENT", std::process::id().to_string()),
        ("AAG_ISOLATION_PEER", peer.to_string()),
        ("AAG_ISOLATION_FIXTURE_OBJECTS", selected_objects().1.into()),
    ]);
    let (executable, args) = role_program();
    let spec = SpawnSpec {
        primary_token: Some(token),
        pipe_stdin: true,
        capture_stdout: true,
        environment: Some(&environment),
        cwd: Some(root),
        ..SpawnSpec::new(&executable, &args)
    };
    let mut child = NativeChild::create_suspended(&spec).unwrap();
    child.admit_and_resume().unwrap();
    let mut stdin = child.take_stdin().unwrap();
    stdin.write_all(b"guard-probe\n").unwrap();
    drop(stdin);
    let deadline = Instant::now() + Duration::from_secs(12);
    let mut output = Vec::new();
    loop {
        let eof = child.read_available(&mut output, OUTPUT_LIMIT).unwrap();
        if let Some(code) = child.try_exit().unwrap() {
            if eof {
                assert_eq!(
                    code,
                    0,
                    "isolated role failed: {}",
                    String::from_utf8_lossy(&output)
                );
                assert!(String::from_utf8_lossy(&output).contains("AAG_ROLE_RESULT_OK"));
                break;
            }
        }
        assert!(Instant::now() < deadline, "isolated role deadline");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
#[ignore = "selected owned authority subprocess; never run as a global harness"]
fn isolated_authority_fixture() {
    std::panic::set_hook(Box::new(|info| {
        println!("AAG_AUTHORITY_FIXTURE_FAILURE: {info}")
    }));
    assert_eq!(std::env::var("AAG_ISOLATION_MODE").unwrap(), "authority");
    let nonce = std::env::var("AAG_ISOLATION_NONCE").unwrap();
    let root = fixture_base().join(nonce);
    let scenario = std::env::var("AAG_ISOLATION_SCENARIO").unwrap();
    let mut authority = CallerAuthority {
        inner: Authority::capture_fixture(&root).unwrap(),
    };
    authority.protect_current_objects().unwrap();
    native::storage::initialize_fixture_container(&authority.inner, &root).unwrap();
    let a = capabilities("fixture-source-a");
    let b = capabilities("fixture-source-b");
    for name in ["a", "b"] {
        create_owned_directory(&root.join(name), authority.owner_sid());
        create_owned_payload(&root.join(name).join("payload.txt"), authority.owner_sid());
    }
    fixture_acl(&authority, &a, true)
        .apply_tree(&authority, &root.join("a"))
        .unwrap();
    fixture_acl(&authority, &b, true)
        .apply_tree(&authority, &root.join("b"))
        .unwrap();
    match scenario.as_str() {
        "authority_threads" => {
            let dynamic = HeldThread::start();
            let token = fixture_token(&authority, &a, CoreRole::ReadWrite);
            // This kernel-access RED is independent of the child loader. It
            // does not replace actual native-child compatibility qualification.
            assert_thread_power_denied_under_role(
                &token,
                &[unsafe { GetCurrentThreadId() }, dynamic.id],
            );
        }
        "reader" => {
            let token = fixture_token(&authority, &a, CoreRole::ReadOnly);
            run_role(token.token(), "reader", &root, 0);
        }
        "writer" => {
            let token = fixture_token(&authority, &a, CoreRole::ReadWrite);
            run_role(token.token(), "writer", &root, 0);
            assert_eq!(fs::read(root.join("b/payload.txt")).unwrap(), b"fixture");
            assert_eq!(fs::read(root.join("a/payload.txt")).unwrap(), b"fixture+");
        }
        "revoked" => {
            let retained = fixture_token(&authority, &a, CoreRole::ReadWrite);
            assert_eq!(retained.generation(), 0);
            assert!(retained.source_id() == &source_id("fixture-source-a"));
            let sealed = SourceCapabilities::derive(&source_id("fixture-source-a"), 1).unwrap();
            fixture_acl(&authority, &sealed, false)
                .apply_tree(&authority, &root.join("a"))
                .unwrap();
            run_role(retained.token(), "revoked", &root, 0);
            let reopened = SourceCapabilities::derive(&source_id("fixture-source-a"), 2).unwrap();
            fixture_acl(&authority, &reopened, true)
                .apply_tree(&authority, &root.join("a"))
                .unwrap();
            run_role(retained.token(), "stale_generation", &root, 0);
            assert_eq!(fs::read(root.join("a/payload.txt")).unwrap(), b"fixture");
            let fresh = fixture_token(&authority, &reopened, CoreRole::ReadWrite);
            assert_eq!(fresh.generation(), 2);
            run_role(fresh.token(), "fresh_generation", &root, 0);
            assert_eq!(fs::read(root.join("a/payload.txt")).unwrap(), b"fixture+");
        }
        "authority" => {
            let other = fixture_token(&authority, &b, CoreRole::ReadWrite);
            let environment = environment(&[
                ("AAG_ISOLATION_MODE", "role".into()),
                ("AAG_ISOLATION_ROLE", "hold".into()),
            ]);
            let (executable, args) = role_program();
            let spec = SpawnSpec {
                primary_token: Some(other.token()),
                environment: Some(&environment),
                cwd: Some(&root),
                ..SpawnSpec::new(&executable, &args)
            };
            let mut peer = NativeChild::create_suspended(&spec).unwrap();
            peer.admit_and_resume().unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            while !root.join("b/ready.txt").exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(
                root.join("b/ready.txt").exists(),
                "peer did not reach its restricted entrypoint"
            );
            let token = fixture_token(&authority, &a, CoreRole::ReadWrite);
            run_role(token.token(), "authority", &root, peer.process_id());
            peer.terminate(Instant::now() + Duration::from_secs(2))
                .unwrap();
        }
        "nested" => {
            let token = fixture_token(&authority, &a, CoreRole::ReadWrite);
            let read = BinarySid::new(b.read_sid()).unwrap();
            let write = BinarySid::new(b.write_sid()).unwrap();
            let restrict = [
                SidAndAttributes {
                    sid: read.pointer(),
                    attributes: 0,
                },
                SidAndAttributes {
                    sid: write.pointer(),
                    attributes: 0,
                },
            ];
            let mut nested = null_mut();
            let created = unsafe {
                CreateRestrictedToken(
                    token.token().as_handle().as_raw_handle(),
                    8,
                    0,
                    null(),
                    0,
                    null(),
                    restrict.len() as u32,
                    restrict.as_ptr(),
                    &mut nested,
                )
            };
            if created != 0 {
                let nested = own(nested, IsolationError::TokenCreationFailed).unwrap();
                assert!(token_privileges(nested.as_raw_handle()).unwrap().is_empty());
                let mut nested = unsafe {
                    crate::services::owned_process::OwnedToken::from_caller_derived_restricted(
                        nested,
                        token.token().kernel_descriptor().to_vec(),
                        token.token().child_token_descriptor().to_vec(),
                    )
                };
                if selected_objects().0 == FixtureObjectProfile::ChildQuery {
                    nested = unsafe {
                        nested.with_fixture_child_object_descriptors(
                            token.token().process_descriptor().to_vec(),
                            token.token().thread_descriptor().to_vec(),
                        )
                    };
                }
                run_role(&nested, "nested", &root, 0);
            } else {
                assert!(
                    [5, 87, 1346].contains(&unsafe { GetLastError() }),
                    "unexpected nested-restriction error"
                );
            }
        }
        "privileges" => {
            authority
                .inner
                .verify_filtered_authority_rejected()
                .unwrap();
            let token = fixture_token(&authority, &a, CoreRole::ReadWrite);
            assert!(token_privileges(token.token().as_handle().as_raw_handle())
                .unwrap()
                .is_empty());
            run_role(token.token(), "privileges", &root, 0);
        }
        _ => panic!("unknown synthetic scenario"),
    }
    println!("AAG_ISOLATION_RESULT_OK");
}

fn deny_process_and_token(pid: u32, allow_query: bool) {
    assert_ne!(pid, 0);
    for access in [0x10, 0x40, 0x2, 0x8, 0x20, 0x40000, 0x80000, 0x400] {
        let handle = unsafe { OpenProcess(access, 0, pid) };
        assert!(
            handle.is_null(),
            "process access {access:#x} unexpectedly granted"
        );
        assert_eq!(unsafe { GetLastError() }, 5);
    }
    let query = unsafe { OpenProcess(0x1000, 0, pid) };
    if allow_query {
        drop(own(query, IsolationError::FixtureFailed).unwrap());
    } else {
        assert!(
            query.is_null(),
            "authority process query unexpectedly granted"
        );
    }
    // A READ_CONTROL handle is intentionally harmless; it cannot satisfy
    // OpenProcessToken's QUERY_LIMITED_INFORMATION prerequisite.
    let process = own(
        unsafe { OpenProcess(if allow_query { 0x1000 } else { 0x20000 }, 0, pid) },
        IsolationError::FixtureFailed,
    )
    .unwrap();
    for access in [0x2, 0x4, 0x20, 0x80, 0x100] {
        let mut token = null_mut();
        assert_eq!(
            unsafe { OpenProcessToken(process.as_raw_handle(), access, &mut token) },
            0
        );
        assert!(token.is_null());
    }
}

#[test]
#[ignore = "owned restricted role subprocess"]
fn isolated_role_fixture() {
    std::panic::set_hook(Box::new(|info| {
        println!("AAG_ROLE_FIXTURE_FAILURE: {info}")
    }));
    assert_eq!(std::env::var("AAG_ISOLATION_MODE").unwrap(), "role");
    let mode = std::env::var("AAG_ISOLATION_ROLE").unwrap();
    let mut self_query = null_mut();
    assert_ne!(
        unsafe { OpenProcessToken(GetCurrentProcess(), 0x8, &mut self_query) },
        0,
        "restricted role must be able to query its own token"
    );
    let self_query = own(self_query, IsolationError::FixtureFailed).unwrap();
    assert!(token_privileges(self_query.as_raw_handle())
        .unwrap()
        .is_empty());
    if mode == "hold" {
        fs::write("b/ready.txt", b"ready").unwrap();
        loop {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    let mut input = [0u8; 12];
    std::io::stdin().read_exact(&mut input).unwrap();
    assert_eq!(&input, b"guard-probe\n");
    for which in [(-10i32) as u32, (-11i32) as u32] {
        let handle = unsafe { GetStdHandle(which) };
        let mut flags = 0;
        assert_ne!(
            unsafe { GetNamedPipeInfo(handle, &mut flags, null_mut(), null_mut(), null_mut()) },
            0
        );
        assert_eq!(flags & 1, 0, "restricted child received a pipe server end");
        let impersonated = unsafe { ImpersonateNamedPipeClient(handle) };
        if impersonated != 0 {
            unsafe {
                RevertToSelf();
            }
        }
        assert_eq!(impersonated, 0, "stdio exposed client impersonation");
    }
    let a = Path::new("a/payload.txt");
    let b = Path::new("b/payload.txt");
    match mode.as_str() {
        "reader" | "revoked" | "stale_generation" => {
            assert_eq!(fs::read(a).unwrap(), b"fixture");
            assert!(append(a).is_err());
            assert!(fs::remove_file(a).is_err());
            assert!(fs::rename(a, "a/renamed.txt").is_err());
            assert!(fs::rename("a", "renamed_source").is_err());
            assert!(!native::storage::fixture_descriptor_attempt(a));
            assert!(!native::storage::fixture_descriptor_attempt(Path::new("a")));
        }
        "writer" => {
            append(a).unwrap();
            fs::write("a/new_payload.txt", b"new").unwrap();
            assert!(append(b).is_err());
            assert!(fs::remove_file(b).is_err());
            assert!(fs::rename(b, "b/renamed.txt").is_err());
            assert!(fs::rename("b", "renamed_source").is_err());
            assert!(!native::storage::fixture_descriptor_attempt(b));
        }
        "fresh_generation" => {
            append(a).unwrap();
            assert_eq!(fs::read(a).unwrap(), b"fixture+");
        }
        "authority" => {
            deny_process_and_token(
                std::env::var("AAG_ISOLATION_PARENT")
                    .unwrap()
                    .parse()
                    .unwrap(),
                false,
            );
            deny_process_and_token(
                std::env::var("AAG_ISOLATION_PEER")
                    .unwrap()
                    .parse()
                    .unwrap(),
                selected_objects().0 == FixtureObjectProfile::ChildQuery,
            );
            for access in [0x2, 0x4, 0x20, 0x80, 0x100] {
                let mut token = null_mut();
                assert_eq!(
                    unsafe { OpenProcessToken(GetCurrentProcess(), access, &mut token) },
                    0,
                    "role token granted an adjustable or duplicable self handle"
                );
            }
        }
        "nested" => {
            assert!(append(b).is_err());
        }
        "privileges" => {
            assert!(token_privileges((-4isize) as Handle).unwrap().is_empty());
            let mut luid = Luid { low: 0, high: 0 };
            assert_ne!(
                unsafe {
                    LookupPrivilegeValueW(
                        null(),
                        wide("SeChangeNotifyPrivilege").unwrap().as_ptr(),
                        &mut luid,
                    )
                },
                0
            );
            let state = TokenPrivileges {
                count: 1,
                privileges: [LuidAndAttributes {
                    luid,
                    attributes: 2,
                }],
            };
            unsafe {
                SetLastError(0);
            }
            let result = unsafe {
                AdjustTokenPrivileges(
                    (-4isize) as Handle,
                    0,
                    (&state as *const TokenPrivileges).cast(),
                    0,
                    null_mut(),
                    null_mut(),
                )
            };
            let error = unsafe { GetLastError() };
            assert!(
                (result == 0 && error == 5) || error == 1300,
                "deleted privilege was re-enabled or the adjustment probe was invalid"
            );
        }
        _ => panic!("unknown synthetic role"),
    }
    println!("AAG_ROLE_RESULT_OK");
}
