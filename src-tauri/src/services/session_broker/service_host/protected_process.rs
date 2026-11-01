//! Policy-A self protection, called before business readiness and untrusted I/O.
//! No restricted token, deny-only logon SID, privilege adjustment, or foreign PID
//! is used. Failure can leave stricter partial protection; the caller must fail
//! startup. Readback is a point-in-time descriptor proof, not handle revocation.

use super::{
    ffi::{GetCurrentProcess, Handle, LocalFree, OpenProcessToken},
    identity, ServiceContext, ServiceHostError, ServicePhase, ServiceRole,
};
use std::{
    collections::BTreeMap,
    ffi::c_void,
    fmt,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    ptr::null_mut,
};

const MAX_DESCRIPTOR_BYTES: u32 = 64 * 1024;
const MAX_THREAD_ENTRIES: usize = 65536;
const MAX_POLICY_THREADS: usize = 4096;
const OWNER_DACL_INFORMATION: u32 = 0x1 | 0x4;
const PROTECTED_DACL_INFORMATION: u32 = 0x8000_0000;
const READ_CONTROL: u32 = 0x0002_0000;
const WRITE_DAC: u32 = 0x0004_0000;
const WRITE_OWNER: u32 = 0x0008_0000;
const GENERIC_ALL: u32 = 0x1000_0000;
const KERNEL_ALL: u32 = 0x001f_ffff;
const TOKEN_ALL: u32 = 0x000f_01ff;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProtectionStage {
    OpenToken,
    Descriptor,
    DefaultToken,
    KernelApply,
    KernelReadback,
    ThreadSnapshot,
    ThreadOpen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PolicyProtectionError {
    ContextRequired,
    Identity(ServiceHostError),
    Native { stage: ProtectionStage, win32: u32 },
    ReadbackFailed,
    ThreadOwnershipChanged,
}

impl fmt::Display for PolicyProtectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("session_service_host_policy_protection_failed")
    }
}
impl std::error::Error for PolicyProtectionError {}
impl From<PolicyProtectionError> for ServiceHostError {
    fn from(_: PolicyProtectionError) -> Self {
        Self::PolicyProtectionFailed
    }
}

/// An observation bound to the current process. Deliberately no Clone/Serde.
pub(crate) struct PolicyProcessProtection {
    process_id: u32,
    user_sid: String,
    protected_threads: usize,
}
impl PolicyProcessProtection {
    pub fn process_id(&self) -> u32 {
        self.process_id
    }
    pub fn protected_thread_count(&self) -> usize {
        self.protected_threads
    }
    pub fn user_sid(&self) -> &str {
        &self.user_sid
    }
}

#[repr(C)]
struct TokenPointer {
    pointer: *mut c_void,
}
#[repr(C)]
struct AclSize {
    count: u32,
    bytes_in_use: u32,
    bytes_free: u32,
}
#[repr(C)]
struct AllowedAce {
    kind: u8,
    flags: u8,
    size: u16,
    mask: u32,
    sid_start: u32,
}
#[repr(C)]
struct ThreadEntry {
    size: u32,
    usage: u32,
    thread_id: u32,
    process_id: u32,
    base_priority: i32,
    delta_priority: i32,
    flags: u32,
}

#[link(name = "advapi32")]
extern "system" {
    fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
        value: *const u16,
        revision: u32,
        output: *mut *mut c_void,
        size: *mut u32,
    ) -> i32;
    fn GetSecurityDescriptorOwner(
        descriptor: *const c_void,
        owner: *mut *mut c_void,
        defaulted: *mut i32,
    ) -> i32;
    fn GetSecurityDescriptorDacl(
        descriptor: *const c_void,
        present: *mut i32,
        dacl: *mut *mut c_void,
        defaulted: *mut i32,
    ) -> i32;
    fn GetSecurityDescriptorControl(
        descriptor: *const c_void,
        control: *mut u16,
        revision: *mut u32,
    ) -> i32;
    fn GetAclInformation(acl: *const c_void, output: *mut c_void, size: u32, class: u32) -> i32;
    fn GetAce(acl: *const c_void, index: u32, output: *mut *mut c_void) -> i32;
    fn SetSecurityInfo(
        object: Handle,
        kind: u32,
        information: u32,
        owner: *mut c_void,
        group: *mut c_void,
        dacl: *mut c_void,
        sacl: *mut c_void,
    ) -> u32;
    fn GetKernelObjectSecurity(
        object: Handle,
        information: u32,
        output: *mut c_void,
        length: u32,
        needed: *mut u32,
    ) -> i32;
    fn SetTokenInformation(
        token: Handle,
        class: u32,
        information: *const c_void,
        length: u32,
    ) -> i32;
}
#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentProcessId() -> u32;
    fn GetCurrentThreadId() -> u32;
    fn GetLastError() -> u32;
    fn CreateToolhelp32Snapshot(flags: u32, process: u32) -> Handle;
    fn Thread32First(snapshot: Handle, entry: *mut ThreadEntry) -> i32;
    fn Thread32Next(snapshot: Handle, entry: *mut ThreadEntry) -> i32;
    fn OpenThread(access: u32, inherit: i32, thread: u32) -> Handle;
    fn GetProcessIdOfThread(thread: Handle) -> u32;
}

fn native_error(stage: ProtectionStage) -> PolicyProtectionError {
    PolicyProtectionError::Native {
        stage,
        win32: unsafe { GetLastError() },
    }
}

struct Descriptor(*mut c_void);
impl Drop for Descriptor {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0);
        }
    }
}
impl Descriptor {
    fn new(policy_sid: &str) -> Result<Self, PolicyProtectionError> {
        let text = identity::wide(&format!(
            "O:{policy_sid}D:P(A;;GA;;;{policy_sid})(A;;GA;;;SY)(A;;GA;;;BA)(A;;RC;;;OW)"
        ));
        let mut pointer = null_mut();
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                text.as_ptr(),
                1,
                &mut pointer,
                null_mut(),
            )
        } == 0
            || pointer.is_null()
        {
            return Err(native_error(ProtectionStage::Descriptor));
        }
        Ok(Self(pointer))
    }
    fn owner_and_acl(&self) -> Result<(*mut c_void, *mut c_void), PolicyProtectionError> {
        descriptor_owner_and_acl(self.0)
    }
}

fn descriptor_owner_and_acl(
    descriptor: *const c_void,
) -> Result<(*mut c_void, *mut c_void), PolicyProtectionError> {
    let (mut owner, mut acl, mut present, mut defaulted) = (null_mut(), null_mut(), 0, 0);
    if unsafe { GetSecurityDescriptorOwner(descriptor, &mut owner, &mut defaulted) } == 0
        || unsafe { GetSecurityDescriptorDacl(descriptor, &mut present, &mut acl, &mut defaulted) }
            == 0
        || owner.is_null()
        || present == 0
        || acl.is_null()
    {
        return Err(PolicyProtectionError::ReadbackFailed);
    }
    Ok((owner, acl))
}

#[derive(Clone)]
struct Grant {
    sid: String,
    kind: u8,
    flags: u8,
    mask: u32,
}

fn grants_match(grants: &[Grant], policy_sid: &str, full_mask: u32) -> bool {
    if grants.len() != 4 {
        return false;
    }
    let expected = [
        (policy_sid, full_mask),
        ("S-1-5-18", full_mask),
        ("S-1-5-32-544", full_mask),
        ("S-1-3-4", READ_CONTROL),
    ];
    let mut observed = BTreeMap::new();
    for grant in grants {
        if grant.kind != 0 || grant.flags != 0 || observed.contains_key(grant.sid.as_str()) {
            return false;
        }
        let mask = if grant.mask == GENERIC_ALL && grant.sid != "S-1-3-4" {
            full_mask
        } else {
            grant.mask
        };
        observed.insert(grant.sid.as_str(), mask);
    }
    expected
        .into_iter()
        .all(|(sid, mask)| observed.get(sid) == Some(&mask))
}

fn verify_acl(
    acl: *const c_void,
    policy_sid: &str,
    full_mask: u32,
) -> Result<(), PolicyProtectionError> {
    if acl.is_null() {
        return Err(PolicyProtectionError::ReadbackFailed);
    }
    let mut size = AclSize {
        count: 0,
        bytes_in_use: 0,
        bytes_free: 0,
    };
    if unsafe {
        GetAclInformation(
            acl,
            (&mut size as *mut AclSize).cast(),
            std::mem::size_of::<AclSize>() as u32,
            2,
        )
    } == 0
        || size.count != 4
        || size.bytes_in_use > MAX_DESCRIPTOR_BYTES
    {
        return Err(PolicyProtectionError::ReadbackFailed);
    }
    let mut grants = Vec::new();
    for index in 0..size.count {
        let mut pointer = null_mut();
        if unsafe { GetAce(acl, index, &mut pointer) } == 0 || pointer.is_null() {
            return Err(PolicyProtectionError::ReadbackFailed);
        }
        let entry = unsafe { &*pointer.cast::<AllowedAce>() };
        if entry.kind != 0
            || entry.flags != 0
            || (entry.size as usize) < std::mem::offset_of!(AllowedAce, sid_start) + 8
        {
            return Err(PolicyProtectionError::ReadbackFailed);
        }
        let sid = unsafe { identity::sid_text((&entry.sid_start as *const u32).cast_mut().cast()) }
            .map_err(PolicyProtectionError::Identity)?;
        grants.push(Grant {
            sid,
            kind: entry.kind,
            flags: entry.flags,
            mask: entry.mask,
        });
    }
    if grants_match(&grants, policy_sid, full_mask) {
        Ok(())
    } else {
        Err(PolicyProtectionError::ReadbackFailed)
    }
}

fn verify_kernel(
    object: Handle,
    policy_sid: &str,
    full_mask: u32,
    require_protected: bool,
) -> Result<(), PolicyProtectionError> {
    let mut needed = 0;
    unsafe {
        GetKernelObjectSecurity(object, OWNER_DACL_INFORMATION, null_mut(), 0, &mut needed);
    }
    if needed == 0 || needed > MAX_DESCRIPTOR_BYTES {
        return Err(native_error(ProtectionStage::KernelReadback));
    }
    let capacity = needed;
    let mut words = vec![0usize; (capacity as usize).div_ceil(std::mem::size_of::<usize>())];
    if unsafe {
        GetKernelObjectSecurity(
            object,
            OWNER_DACL_INFORMATION,
            words.as_mut_ptr().cast(),
            capacity,
            &mut needed,
        )
    } == 0
        || needed > capacity
    {
        return Err(native_error(ProtectionStage::KernelReadback));
    }
    let (owner, acl) = descriptor_owner_and_acl(words.as_ptr().cast())?;
    let (mut control, mut revision) = (0, 0);
    if unsafe { GetSecurityDescriptorControl(words.as_ptr().cast(), &mut control, &mut revision) }
        == 0
        || (require_protected && control & 0x1000 == 0)
        || unsafe { identity::sid_text(owner) }.map_err(PolicyProtectionError::Identity)?
            != policy_sid
    {
        return Err(PolicyProtectionError::ReadbackFailed);
    }
    verify_acl(acl, policy_sid, full_mask)
}

fn protect_kernel(
    object: Handle,
    descriptor: &Descriptor,
    policy_sid: &str,
    full_mask: u32,
) -> Result<(), PolicyProtectionError> {
    let (owner, acl) = descriptor.owner_and_acl()?;
    let error = unsafe {
        SetSecurityInfo(
            object,
            6,
            OWNER_DACL_INFORMATION | PROTECTED_DACL_INFORMATION,
            owner,
            null_mut(),
            acl,
            null_mut(),
        )
    };
    if error != 0 {
        return Err(PolicyProtectionError::Native {
            stage: ProtectionStage::KernelApply,
            win32: error,
        });
    }
    verify_kernel(object, policy_sid, full_mask, true)
}

fn protect_defaults(
    token: Handle,
    descriptor: &Descriptor,
    policy_sid: &str,
) -> Result<(), PolicyProtectionError> {
    let (owner, acl) = descriptor.owner_and_acl()?;
    for (class, pointer) in [(4, owner), (6, acl)] {
        let info = TokenPointer { pointer };
        if unsafe {
            SetTokenInformation(
                token,
                class,
                (&info as *const TokenPointer).cast(),
                std::mem::size_of::<TokenPointer>() as u32,
            )
        } == 0
        {
            return Err(native_error(ProtectionStage::DefaultToken));
        }
    }
    let owner_buffer =
        identity::token_information(token, 4).map_err(PolicyProtectionError::Identity)?;
    let acl_buffer =
        identity::token_information(token, 6).map_err(PolicyProtectionError::Identity)?;
    if owner_buffer.bytes < std::mem::size_of::<TokenPointer>()
        || acl_buffer.bytes < std::mem::size_of::<TokenPointer>()
    {
        return Err(PolicyProtectionError::ReadbackFailed);
    }
    let owner = unsafe { (*owner_buffer.words.as_ptr().cast::<TokenPointer>()).pointer };
    if unsafe { identity::sid_text(owner) }.map_err(PolicyProtectionError::Identity)? != policy_sid
    {
        return Err(PolicyProtectionError::ReadbackFailed);
    }
    let acl = unsafe { (*acl_buffer.words.as_ptr().cast::<TokenPointer>()).pointer };
    verify_acl(acl, policy_sid, GENERIC_ALL)
}

fn current_threads(process_id: u32) -> Result<Vec<u32>, PolicyProtectionError> {
    let raw = unsafe { CreateToolhelp32Snapshot(0x4, 0) };
    if raw.is_null() || raw as isize == -1 {
        return Err(native_error(ProtectionStage::ThreadSnapshot));
    }
    let snapshot = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut entry = ThreadEntry {
        size: std::mem::size_of::<ThreadEntry>() as u32,
        usage: 0,
        thread_id: 0,
        process_id: 0,
        base_priority: 0,
        delta_priority: 0,
        flags: 0,
    };
    if unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) } == 0 {
        return Err(native_error(ProtectionStage::ThreadSnapshot));
    }
    let mut own = Vec::new();
    for _ in 0..MAX_THREAD_ENTRIES {
        if entry.process_id == process_id {
            own.push(entry.thread_id);
            if own.len() > MAX_POLICY_THREADS {
                return Err(PolicyProtectionError::ReadbackFailed);
            }
        }
        entry.size = std::mem::size_of::<ThreadEntry>() as u32;
        if unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) } == 0 {
            return if unsafe { GetLastError() } == 18 {
                Ok(own)
            } else {
                Err(native_error(ProtectionStage::ThreadSnapshot))
            };
        }
    }
    Err(PolicyProtectionError::ReadbackFailed)
}

fn protect_threads(
    descriptor: &Descriptor,
    policy_sid: &str,
    process_id: u32,
    apply: bool,
    context: &ServiceContext,
) -> Result<usize, PolicyProtectionError> {
    let mut current_seen = false;
    let mut protected = 0;
    for thread_id in current_threads(process_id)? {
        if context.is_cancelled() {
            return Err(PolicyProtectionError::ContextRequired);
        }
        let access = READ_CONTROL | 0x800 | if apply { WRITE_DAC | WRITE_OWNER } else { 0 };
        let raw = unsafe { OpenThread(access, 0, thread_id) };
        if raw.is_null() {
            if unsafe { GetLastError() } == 87 {
                continue;
            }
            return Err(native_error(ProtectionStage::ThreadOpen));
        }
        let thread = unsafe { OwnedHandle::from_raw_handle(raw) };
        if unsafe { GetProcessIdOfThread(thread.as_raw_handle()) } != process_id {
            return Err(PolicyProtectionError::ThreadOwnershipChanged);
        }
        if apply {
            protect_kernel(thread.as_raw_handle(), descriptor, policy_sid, KERNEL_ALL)?;
        }
        // TokenDefaultDacl carries ACLs, not SE_DACL_PROTECTED. A newly created
        // ordinary kernel thread can therefore have the exact private grants
        // without that descriptor-control bit. Kernel threads are not file
        // containers; the exact no-inheritance ACE readback is still required.
        else {
            verify_kernel(thread.as_raw_handle(), policy_sid, KERNEL_ALL, false)?;
        }
        current_seen |= thread_id == unsafe { GetCurrentThreadId() };
        protected += 1;
    }
    if current_seen {
        Ok(protected)
    } else {
        Err(PolicyProtectionError::ReadbackFailed)
    }
}

/// Call in Policy-A startup before ready/untrusted I/O. There is no PID, SID,
/// role-name, account, or descriptor supplied by a remote caller.
pub(crate) fn protect_policy_process(
    context: &ServiceContext,
) -> Result<PolicyProcessProtection, PolicyProtectionError> {
    if context.role() != ServiceRole::Policy {
        return Err(PolicyProtectionError::ContextRequired);
    }
    // Serialize startup protection with ready(). No business thread can publish
    // RUNNING between identity admission and the final kernel readback.
    let machine = context
        .control
        .machine
        .lock()
        .map_err(|_| PolicyProtectionError::ContextRequired)?;
    if context.is_cancelled()
        || machine.phase != ServicePhase::StartPending
        || !machine.identity_verified
    {
        return Err(PolicyProtectionError::ContextRequired);
    }
    let expected = context
        .current_role_user_sid()
        .map_err(PolicyProtectionError::Identity)?;
    if context.is_cancelled() {
        return Err(PolicyProtectionError::ContextRequired);
    }
    let descriptor = Descriptor::new(&expected)?;
    let mut raw = null_mut();
    if unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            0x8 | 0x80 | READ_CONTROL | WRITE_DAC | WRITE_OWNER,
            &mut raw,
        )
    } == 0
        || raw.is_null()
    {
        return Err(native_error(ProtectionStage::OpenToken));
    }
    let token = unsafe { OwnedHandle::from_raw_handle(raw) };
    // Set defaults before the thread snapshot, so subsequent ordinary threads
    // inherit the strict A/SYSTEM/Admin descriptor with OWNER_RIGHTS RC.
    protect_defaults(token.as_raw_handle(), &descriptor, &expected)?;
    protect_kernel(
        unsafe { GetCurrentProcess() },
        &descriptor,
        &expected,
        KERNEL_ALL,
    )?;
    protect_kernel(token.as_raw_handle(), &descriptor, &expected, TOKEN_ALL)?;
    let process_id = unsafe { GetCurrentProcessId() };
    protect_threads(&descriptor, &expected, process_id, true, context)?;
    let protected_threads = protect_threads(&descriptor, &expected, process_id, false, context)?;
    Ok(PolicyProcessProtection {
        process_id,
        user_sid: expected,
        protected_threads,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expected() -> Vec<Grant> {
        ["S-1-5-80-1-2-3-4-5", "S-1-5-18", "S-1-5-32-544", "S-1-3-4"]
            .into_iter()
            .map(|sid| Grant {
                sid: sid.into(),
                kind: 0,
                flags: 0,
                mask: if sid == "S-1-3-4" {
                    READ_CONTROL
                } else {
                    GENERIC_ALL
                },
            })
            .collect()
    }

    #[test]
    fn policy_descriptor_keeps_a_enabled_and_grants_no_runtime_or_interactive_owner() {
        let grants = expected();
        assert!(grants_match(&grants, "S-1-5-80-1-2-3-4-5", KERNEL_ALL));
        for foreign in [
            "S-1-5-21-100-200-300-1001",
            "S-1-5-80-6-7-8-9-10",
            "S-1-1-0",
        ] {
            let mut extra = grants.clone();
            extra.push(Grant {
                sid: foreign.into(),
                kind: 0,
                flags: 0,
                mask: READ_CONTROL,
            });
            assert!(!grants_match(&extra, "S-1-5-80-1-2-3-4-5", KERNEL_ALL));
        }
    }

    #[test]
    fn readback_rejects_owner_write_dac_deny_only_and_duplicate_grants() {
        let mut grants = expected();
        grants[3].mask |= WRITE_DAC;
        assert!(!grants_match(&grants, "S-1-5-80-1-2-3-4-5", KERNEL_ALL));
        let mut grants = expected();
        grants[0].kind = 1;
        assert!(!grants_match(&grants, "S-1-5-80-1-2-3-4-5", KERNEL_ALL));
        let mut grants = expected();
        grants[0].flags = 0x10;
        assert!(!grants_match(&grants, "S-1-5-80-1-2-3-4-5", KERNEL_ALL));
        let mut grants = expected();
        grants[1] = grants[0].clone();
        assert!(!grants_match(&grants, "S-1-5-80-1-2-3-4-5", KERNEL_ALL));
    }

    #[test]
    fn reader_and_runtime_cannot_invoke_policy_self_mutation() {
        use super::super::{ServiceControl, ServiceMachine, StatusReporter, StatusSnapshot};
        use std::sync::{
            atomic::{AtomicBool, AtomicU64},
            Arc, Mutex,
        };
        use std::time::Instant;
        struct NeverReport;
        impl StatusReporter for NeverReport {
            fn report(&self, _: StatusSnapshot) -> Result<(), ServiceHostError> {
                panic!("foreign role reached policy mutation");
            }
        }
        for role in [
            ServiceRole::Reader,
            ServiceRole::runtime("0123456789abcdef0123456789abcdef", 1).unwrap(),
        ] {
            let context = ServiceContext {
                control: Arc::new(ServiceControl {
                    role,
                    cancelled: AtomicBool::new(false),
                    machine: Mutex::new(ServiceMachine::new()),
                    started: Instant::now(),
                    stop_elapsed_ms: AtomicU64::new(u64::MAX),
                    callback_panicked: AtomicBool::new(false),
                    reporter: Arc::new(NeverReport),
                }),
            };
            assert!(matches!(
                protect_policy_process(&context),
                Err(PolicyProtectionError::ContextRequired)
            ));
        }
    }
}
