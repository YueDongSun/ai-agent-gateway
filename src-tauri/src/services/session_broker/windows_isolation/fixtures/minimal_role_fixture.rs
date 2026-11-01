//! Standalone std-only Windows role fixture. The parent compiles this exact
//! source with its qualified rustc; it is not a production executable.
#![cfg(windows)]

use std::{
    ffi::c_void,
    fs,
    io::{Read, Write},
    os::windows::{ffi::OsStrExt, io::{AsRawHandle, FromRawHandle, OwnedHandle}},
    path::Path,
    ptr::{null, null_mut},
    time::Duration,
};

type Handle = *mut c_void;
const MAX_TOKEN_BYTES: u32 = 1024 * 1024;

#[repr(C)]
struct Luid { low: u32, high: i32 }
#[repr(C)]
struct LuidAndAttributes { luid: Luid, attributes: u32 }
#[repr(C)]
struct TokenPrivileges { count: u32, privileges: [LuidAndAttributes; 1] }

#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentProcess() -> Handle;
    fn OpenProcess(access: u32, inherit: i32, id: u32) -> Handle;
    fn GetLastError() -> u32;
    fn SetLastError(value: u32);
    fn GetStdHandle(which: u32) -> Handle;
    fn GetNamedPipeInfo(pipe: Handle, flags: *mut u32, out_size: *mut u32,
        in_size: *mut u32, max_instances: *mut u32) -> i32;
    fn CreateFileW(name: *const u16, access: u32, share: u32, attributes: *const c_void,
        disposition: u32, flags: u32, template: Handle) -> Handle;
    fn LocalFree(value: *mut c_void) -> *mut c_void;
}
#[link(name = "advapi32")]
extern "system" {
    fn OpenProcessToken(process: Handle, access: u32, token: *mut Handle) -> i32;
    fn GetTokenInformation(token: Handle, class: u32, information: *mut c_void,
        length: u32, needed: *mut u32) -> i32;
    fn ImpersonateNamedPipeClient(pipe: Handle) -> i32;
    fn RevertToSelf() -> i32;
    fn LookupPrivilegeValueW(system: *const u16, name: *const u16, luid: *mut Luid) -> i32;
    fn AdjustTokenPrivileges(token: Handle, disable_all: i32, new_state: *const c_void,
        length: u32, previous: *mut c_void, returned: *mut u32) -> i32;
    fn ConvertStringSecurityDescriptorToSecurityDescriptorW(value: *const u16, revision: u32,
        descriptor: *mut *mut c_void, size: *mut u32) -> i32;
    fn GetSecurityDescriptorDacl(descriptor: *mut c_void, present: *mut i32,
        acl: *mut *mut c_void, defaulted: *mut i32) -> i32;
    fn SetSecurityInfo(object: Handle, kind: u32, info: u32, owner: *mut c_void,
        group: *mut c_void, dacl: *mut c_void, sacl: *mut c_void) -> u32;
}

fn owned(handle: Handle) -> OwnedHandle {
    assert!(!handle.is_null() && handle as isize != -1, "fixture handle unavailable");
    unsafe { OwnedHandle::from_raw_handle(handle) }
}
fn wide(value: &std::ffi::OsStr) -> Vec<u16> {
    let mut result: Vec<_> = value.encode_wide().collect();
    assert!(!result.contains(&0));
    result.push(0);
    result
}
fn privilege_count(token: Handle) -> u32 {
    let mut needed = 0;
    unsafe { GetTokenInformation(token, 3, null_mut(), 0, &mut needed); }
    assert!(needed >= 4 && needed <= MAX_TOKEN_BYTES, "invalid token information size");
    let mut buffer = vec![0usize; (needed as usize).div_ceil(std::mem::size_of::<usize>())];
    assert_ne!(unsafe { GetTokenInformation(token, 3, buffer.as_mut_ptr().cast(), needed, &mut needed) }, 0);
    unsafe { *buffer.as_ptr().cast::<u32>() }
}
fn verify_own_token() {
    let mut token = null_mut();
    assert_ne!(unsafe { OpenProcessToken(GetCurrentProcess(), 0x8, &mut token) }, 0,
        "restricted role must query its own token");
    let token = owned(token);
    assert_eq!(privilege_count(token.as_raw_handle()), 0);
}
fn verify_stdio() {
    let mut input = [0u8; 12];
    std::io::stdin().read_exact(&mut input).unwrap();
    assert_eq!(&input, b"guard-probe\n");
    for which in [(-10i32) as u32, (-11i32) as u32] {
        let handle = unsafe { GetStdHandle(which) };
        let mut flags = 0;
        assert_ne!(unsafe { GetNamedPipeInfo(handle, &mut flags, null_mut(), null_mut(), null_mut()) }, 0);
        assert_eq!(flags & 1, 0, "restricted child received a pipe server end");
        let impersonated = unsafe { ImpersonateNamedPipeClient(handle) };
        if impersonated != 0 { unsafe { RevertToSelf(); } }
        assert_eq!(impersonated, 0, "stdio exposed client impersonation");
    }
}
fn append(path: &Path) -> std::io::Result<()> {
    fs::OpenOptions::new().append(true).open(path)?.write_all(b"+")
}
fn descriptor_attempt(path: &Path) -> bool {
    let name = wide(path.as_os_str());
    let handle = unsafe { CreateFileW(name.as_ptr(), 0x0006_0000, 3, null(), 3,
        0x0220_0000, null_mut()) };
    if handle.is_null() || handle as isize == -1 { return false; }
    let handle = owned(handle);
    let descriptor_text = wide(std::ffi::OsStr::new("D:P(A;;FA;;;WD)"));
    let mut descriptor = null_mut();
    assert_ne!(unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(
        descriptor_text.as_ptr(), 1, &mut descriptor, null_mut()) }, 0);
    let mut present = 0;
    let mut defaulted = 0;
    let mut acl = null_mut();
    assert_ne!(unsafe { GetSecurityDescriptorDacl(descriptor, &mut present, &mut acl, &mut defaulted) }, 0);
    assert_ne!(present, 0);
    let result = unsafe { SetSecurityInfo(handle.as_raw_handle(), 1, 0x8000_0004,
        null_mut(), null_mut(), acl, null_mut()) };
    unsafe { LocalFree(descriptor); }
    result == 0
}
fn deny_process_and_token(pid: u32, allow_query: bool) {
    assert_ne!(pid, 0);
    for access in [0x10, 0x40, 0x2, 0x8, 0x20, 0x40000, 0x80000, 0x400] {
        let handle = unsafe { OpenProcess(access, 0, pid) };
        let error = unsafe { GetLastError() };
        if !handle.is_null() { drop(owned(handle)); panic!("process access {access:#x} unexpectedly granted"); }
        assert_eq!(error, 5);
    }
    let query = unsafe { OpenProcess(0x1000, 0, pid) };
    if allow_query {
        drop(owned(query));
    } else {
        if !query.is_null() { drop(owned(query)); panic!("authority process query unexpectedly granted"); }
    }
    let process = owned(unsafe { OpenProcess(if allow_query { 0x1000 } else { 0x20000 }, 0, pid) });
    for access in [0x2, 0x4, 0x20, 0x80, 0x100] {
        let mut token = null_mut();
        let result = unsafe { OpenProcessToken(process.as_raw_handle(), access, &mut token) };
        if !token.is_null() { drop(owned(token)); }
        assert_eq!(result, 0, "authority or peer token power unexpectedly granted");
    }
}
fn deny_self_token_power() {
    for access in [0x2, 0x4, 0x20, 0x80, 0x100] {
        let mut token = null_mut();
        let result = unsafe { OpenProcessToken(GetCurrentProcess(), access, &mut token) };
        if !token.is_null() { drop(owned(token)); }
        assert_eq!(result, 0, "role token granted adjustable or duplicable self handle");
    }
}
fn verify_deleted_privileges() {
    assert_eq!(privilege_count((-4isize) as Handle), 0);
    let mut luid = Luid { low: 0, high: 0 };
    let name = wide(std::ffi::OsStr::new("SeChangeNotifyPrivilege"));
    assert_ne!(unsafe { LookupPrivilegeValueW(null(), name.as_ptr(), &mut luid) }, 0);
    let state = TokenPrivileges { count: 1, privileges: [LuidAndAttributes { luid, attributes: 2 }] };
    unsafe { SetLastError(0); }
    let result = unsafe { AdjustTokenPrivileges((-4isize) as Handle, 0,
        (&state as *const TokenPrivileges).cast(), 0, null_mut(), null_mut()) };
    let error = unsafe { GetLastError() };
    assert!((result == 0 && error == 5) || error == 1300,
        "deleted privilege was re-enabled or adjustment probe was invalid");
}
fn verify_fixture_directory() {
    let root = fs::canonicalize(std::env::current_dir().unwrap()).unwrap();
    let nonce = root.file_name().unwrap().to_str().unwrap();
    assert!(nonce.len() == 32 && nonce.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert_eq!(root.parent().unwrap().file_name().unwrap(), "session_isolation_fixtures");
    assert_eq!(root.parent().unwrap().parent().unwrap().file_name().unwrap(), "target");
}
fn run() {
    assert_eq!(std::env::var("AAG_ISOLATION_MODE").unwrap(), "role");
    verify_fixture_directory();
    verify_own_token();
    let mode = std::env::var("AAG_ISOLATION_ROLE").unwrap();
    if mode == "hold" {
        fs::write("b/ready.txt", b"ready").unwrap();
        loop { std::thread::sleep(Duration::from_millis(50)); }
    }
    verify_stdio();
    let a = Path::new("a/payload.txt");
    let b = Path::new("b/payload.txt");
    match mode.as_str() {
        "reader" | "revoked" | "stale_generation" => {
            assert_eq!(fs::read(a).unwrap(), b"fixture");
            assert!(append(a).is_err());
            assert!(fs::remove_file(a).is_err());
            assert!(fs::rename(a, "a/renamed.txt").is_err());
            assert!(fs::rename("a", "renamed_source").is_err());
            assert!(!descriptor_attempt(a));
            assert!(!descriptor_attempt(Path::new("a")));
        }
        "writer" => {
            append(a).unwrap();
            fs::write("a/new_payload.txt", b"new").unwrap();
            assert!(append(b).is_err());
            assert!(fs::remove_file(b).is_err());
            assert!(fs::rename(b, "b/renamed.txt").is_err());
            assert!(fs::rename("b", "renamed_source").is_err());
            assert!(!descriptor_attempt(b));
        }
        "fresh_generation" => {
            append(a).unwrap();
            assert_eq!(fs::read(a).unwrap(), b"fixture+");
        }
        "authority" => {
            let allow_peer_query = match std::env::var("AAG_ISOLATION_FIXTURE_OBJECTS").as_deref() {
                Ok("child_query") => true,
                Ok("protected") | Ok("default_objects") => false,
                _ => panic!("unknown fixture object profile"),
            };
            deny_process_and_token(std::env::var("AAG_ISOLATION_PARENT").unwrap().parse().unwrap(), false);
            deny_process_and_token(std::env::var("AAG_ISOLATION_PEER").unwrap().parse().unwrap(), allow_peer_query);
            deny_self_token_power();
        }
        "nested" => assert!(append(b).is_err()),
        "privileges" => verify_deleted_privileges(),
        _ => panic!("unknown synthetic role"),
    }
    println!("AAG_ROLE_RESULT_OK");
}
fn main() {
    std::panic::set_hook(Box::new(|info| println!("AAG_MINIMAL_FIXTURE_FAILURE: {info}")));
    println!("AAG_MINIMAL_ROLE_ENTRY");
    run();
}
