//! Read-only OS identity and protected-image checks for one local owner query.
//! QueryServiceConfig describes configured/next-start state. The A host must
//! verify its actual ServiceContext before binding and answering this method.

use super::{
    protocol::{self, OwnerClientError as Error, ServiceObservation},
    VerifiedClientAuthority,
};
use crate::{
    config::private_fs::{inventory_object_metadata_from_file, Managed_directory_identity},
    services::session_broker::{
        local_transport::{
            EnvelopeKind, LocalConnection, SendOutcome, ServicePeer, TransportCancellation,
            TransportClosed, TransportFrame, TransportLimits,
        },
        service_host::ServiceRole,
        windows_isolation::Sid,
    },
};
use std::{
    ffi::c_void,
    fs::{File, OpenOptions},
    os::windows::{ffi::OsStringExt, fs::OpenOptionsExt, io::AsRawHandle},
    path::{Path, PathBuf},
    ptr::{null, null_mut},
    time::{Duration, Instant},
};

const QUERY_TIMEOUT: Duration = Duration::from_secs(5);
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_NATIVE_BYTES: u32 = 64 * 1024;
const MAX_TEXT_UNITS: usize = 32768;
const IMAGE_MUTATION_MASK: u32 = 0x500d_0156;
const ANCESTOR_MUTATION_MASK: u32 = 0x500d_0142;
const REPARSE_ATTRIBUTE: u32 = 0x400;
#[derive(Debug, Clone, Copy)]
enum ImageStage {
    AncestorOpen,
    AncestorObject,
    AncestorSecurity,
    RelativePath,
    Open,
    Object,
    Canonical,
    Security,
    Reopen,
    Recheck,
}
#[derive(Debug, Clone, Copy)]
enum ImageCategory {
    Drive,
    Ancestor,
    ProgramFiles,
    WindowsApps,
    Package,
    Directory,
    File,
}
#[derive(Debug, Clone, Copy)]
enum ImageReason {
    OpenFailed(Option<i32>),
    MetadataUnavailable,
    FileInformationFailed(u32),
    Reparse,
    ObjectKind,
    Hardlink,
    ObjectIdentity,
    DescriptorQuery(u32),
    DescriptorMissing,
    OwnerSidUnavailable,
    NullDacl,
    UntrustedOwner,
    AclQuery(u32),
    TooManyAces,
    AceQuery(u32),
    MalformedAce,
    UnsupportedAce(u8),
    AceSidUnavailable,
    UntrustedMutation,
    CanonicalQuery(u32),
    CanonicalLength,
    CanonicalEncoding,
    CanonicalPrefix,
    CanonicalMismatch,
    RelativePath,
}
impl ImageReason {
    fn public_error(self) -> Error {
        match self {
            Self::OwnerSidUnavailable | Self::AceSidUnavailable => Error::Unavailable,
            _ => Error::ImagesUnqualified,
        }
    }
}
#[derive(Debug)]
struct ImageFailure {
    stage: ImageStage,
    category: ImageCategory,
    index: usize,
    reason: ImageReason,
}
fn image_context<T>(
    value: Result<T, ImageReason>,
    stage: ImageStage,
    category: ImageCategory,
    index: usize,
) -> Result<T, ImageFailure> {
    value.map_err(|reason| ImageFailure {
        stage,
        category,
        index,
        reason,
    })
}
type Handle = *mut c_void;

#[repr(C)]
struct Guid {
    first: u32,
    second: u16,
    third: u16,
    rest: [u8; 8],
}
const PROGRAM_FILES_X64: Guid = Guid {
    first: 0x6d809377,
    second: 0x6af0,
    third: 0x444b,
    rest: [0x89, 0x57, 0xa3, 0x77, 0x3f, 0x02, 0x20, 0x0e],
};
const PROGRAM_DATA: Guid = Guid {
    first: 0x62ab5d82,
    second: 0xfdc1,
    third: 0x4dc3,
    rest: [0xa9, 0xdd, 0x07, 0x0d, 0x1d, 0x49, 0x5d, 0x97],
};
#[repr(C)]
struct ServiceStatus {
    service_type: u32,
    state: u32,
    controls: u32,
    exit_code: u32,
    service_exit: u32,
    checkpoint: u32,
    wait_hint: u32,
    process_id: u32,
    flags: u32,
}
#[repr(C)]
struct ServiceConfig {
    service_type: u32,
    start_type: u32,
    error_control: u32,
    binary: *mut u16,
    group: *mut u16,
    tag: u32,
    dependencies: *mut u16,
    account: *mut u16,
    display: *mut u16,
}
#[repr(C)]
struct FileInformation {
    attributes: u32,
    creation: [u32; 2],
    access: [u32; 2],
    write: [u32; 2],
    volume: u32,
    size_high: u32,
    size_low: u32,
    links: u32,
    index_high: u32,
    index_low: u32,
}
#[repr(C)]
struct AclSize {
    count: u32,
    in_use: u32,
    free: u32,
}
struct AccessAce<'a> {
    kind: u8,
    flags: u8,
    mask: u32,
    sid: &'a [u8],
}

/// Standard and callback allow/deny ACEs share Header/Mask/SidStart. Callback
/// application/conditional data follows the SID and remains opaque here.
/// See Microsoft's ACCESS_ALLOWED_CALLBACK_ACE and ACCESS_DENIED_CALLBACK_ACE.
fn parse_access_ace(bytes: &[u8]) -> Result<AccessAce<'_>, ImageReason> {
    if bytes.len() < 16
        || bytes.len() % 4 != 0
        || usize::from(u16::from_le_bytes([bytes[2], bytes[3]])) != bytes.len()
    {
        return Err(ImageReason::MalformedAce);
    }
    let kind = bytes[0];
    if !matches!(kind, 0 | 1 | 9 | 10) {
        return Err(ImageReason::UnsupportedAce(kind));
    }
    let sid_header = &bytes[8..16];
    if sid_header[0] != 1 || sid_header[1] > 15 {
        return Err(ImageReason::MalformedAce);
    }
    let sid_length = 8 + usize::from(sid_header[1]) * 4;
    let sid = bytes
        .get(8..8 + sid_length)
        .ok_or(ImageReason::MalformedAce)?;
    Ok(AccessAce {
        kind,
        flags: bytes[1],
        mask: u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
        sid,
    })
}
fn check_ace_permissions(
    ace: &AccessAce<'_>,
    trusted_sid: bool,
    mutation: u32,
) -> Result<(), ImageReason> {
    // Deny ACEs do not grant rights. Inherit-only ACEs do not affect this object.
    if matches!(ace.kind, 1 | 10) || ace.flags & 8 != 0 {
        return Ok(());
    }
    // Never evaluate a callback condition as true or false. Any potential
    // mutable allow to an untrusted SID is rejected, conditional or otherwise.
    if ace.mask & mutation != 0 && !trusted_sid {
        return Err(ImageReason::UntrustedMutation);
    }
    Ok(())
}

#[cfg(test)]
pub(in crate::services::session_authority) fn check_config_access_ace(
    bytes: &[u8],
    trusted_sid: bool,
    mutation: u32,
) -> Result<(), &'static str> {
    let ace = parse_access_ace(bytes).map_err(|_| "session_config_ace_invalid")?;
    check_ace_permissions(&ace, trusted_sid, mutation).map_err(|_| "session_config_ace_unqualified")
}

#[link(name = "advapi32")]
extern "system" {
    fn OpenSCManagerW(machine: *const u16, database: *const u16, access: u32) -> Handle;
    fn OpenServiceW(manager: Handle, name: *const u16, access: u32) -> Handle;
    fn CloseServiceHandle(handle: Handle) -> i32;
    fn QueryServiceConfigW(
        service: Handle,
        config: *mut ServiceConfig,
        bytes: u32,
        needed: *mut u32,
    ) -> i32;
    fn QueryServiceStatusEx(
        service: Handle,
        level: u32,
        buffer: *mut u8,
        bytes: u32,
        needed: *mut u32,
    ) -> i32;
    fn LookupAccountNameW(
        system: *const u16,
        account: *const u16,
        sid: *mut c_void,
        sid_bytes: *mut u32,
        domain: *mut u16,
        domain_units: *mut u32,
        kind: *mut u32,
    ) -> i32;
    fn ConvertSidToStringSidW(sid: *mut c_void, text: *mut *mut u16) -> i32;
    fn IsValidSid(sid: *mut c_void) -> i32;
    fn GetSecurityInfo(
        object: Handle,
        kind: u32,
        information: u32,
        owner: *mut *mut c_void,
        group: *mut *mut c_void,
        dacl: *mut *mut c_void,
        sacl: *mut *mut c_void,
        descriptor: *mut *mut c_void,
    ) -> u32;
    fn GetAclInformation(acl: *mut c_void, buffer: *mut c_void, bytes: u32, class: u32) -> i32;
    fn GetSecurityDescriptorControl(
        descriptor: *const c_void,
        control: *mut u16,
        revision: *mut u32,
    ) -> i32;
    fn GetAce(acl: *mut c_void, index: u32, ace: *mut *mut c_void) -> i32;
}
#[link(name = "kernel32")]
extern "system" {
    fn LocalFree(pointer: *mut c_void) -> *mut c_void;
    fn GetFileInformationByHandle(file: Handle, info: *mut FileInformation) -> i32;
    fn GetLastError() -> u32;
    fn GetFinalPathNameByHandleW(file: Handle, path: *mut u16, units: u32, flags: u32) -> u32;
}
#[link(name = "shell32")]
extern "system" {
    fn SHGetKnownFolderPath(id: *const Guid, flags: u32, token: Handle, path: *mut *mut u16)
        -> i32;
    fn CommandLineToArgvW(command: *const u16, count: *mut i32) -> *mut *mut u16;
}
#[link(name = "ole32")]
extern "system" {
    fn CoTaskMemFree(pointer: *mut c_void);
}

struct ServiceHandle(Handle);
impl Drop for ServiceHandle {
    fn drop(&mut self) {
        unsafe {
            CloseServiceHandle(self.0);
        }
    }
}
struct LocalAllocation(*mut c_void);
impl Drop for LocalAllocation {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0);
        }
    }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}
unsafe fn text(pointer: *const u16) -> Result<String, Error> {
    if pointer.is_null() {
        return Err(Error::Unavailable);
    }
    let mut length = 0;
    while length < MAX_TEXT_UNITS && *pointer.add(length) != 0 {
        length += 1;
    }
    if length == MAX_TEXT_UNITS {
        return Err(Error::Unavailable);
    }
    String::from_utf16(std::slice::from_raw_parts(pointer, length)).map_err(|_| Error::Unavailable)
}
fn program_files() -> Result<PathBuf, Error> {
    known_folder(&PROGRAM_FILES_X64)
}

pub(in crate::services::session_authority) fn program_files_folder() -> Result<PathBuf, &'static str>
{
    program_files().map_err(|_| "session_config_directory_unavailable")
}
fn known_folder(folder: &Guid) -> Result<PathBuf, Error> {
    let mut pointer = null_mut();
    let result = unsafe { SHGetKnownFolderPath(folder, 0, null_mut(), &mut pointer) };
    if result < 0 || pointer.is_null() {
        return Err(Error::Unavailable);
    }
    let value = unsafe { text(pointer) };
    unsafe {
        CoTaskMemFree(pointer.cast());
    }
    let path = PathBuf::from(value?);
    protocol::validate_disk_path(&path)?;
    Ok(path)
}

pub(in crate::services::session_authority) fn program_data_folder() -> Result<PathBuf, &'static str>
{
    known_folder(&PROGRAM_DATA).map_err(|_| "session_config_directory_unavailable")
}

pub(in crate::services::session_authority) fn protected_object_security(
    file: &File,
    trusted_writers: &[String],
    mutation_mask: u32,
    required_owner: Option<&str>,
    require_protected_dacl: bool,
) -> Result<String, &'static str> {
    inspect_security(
        file,
        trusted_writers,
        mutation_mask,
        required_owner,
        require_protected_dacl,
    )
    .map_err(|_| "session_config_security_unqualified")
}

pub(in crate::services::session_authority) fn canonical_config_path(
    file: &File,
    path: &Path,
) -> Result<(), &'static str> {
    check_canonical_path(file, path).map_err(|_| "session_config_path_unqualified")
}

pub(in crate::services::session_authority) fn gateway_policy_peer(
) -> Result<ServicePeer, &'static str> {
    let shim = program_files()
        .map_err(|_| "session_client_authority_unavailable")?
        .join(protocol::SHIM_RELATIVE);
    let service = service_observation().map_err(|_| "session_client_authority_unavailable")?;
    protocol::service(&service, service.process_id, &shim)
        .map_err(|_| "session_client_authority_unavailable")?;
    let sid = account_sid(protocol::ACCOUNT).map_err(|_| "session_client_authority_unavailable")?;
    ServicePeer::new(ServiceRole::Policy, sid).map_err(|_| "session_client_authority_unavailable")
}

pub(in crate::services::session_authority) fn verify_gateway_peer(
    connection: &LocalConnection,
) -> Result<(), &'static str> {
    let peer = gateway_policy_peer()?;
    if connection.peer().role() != Some(ServiceRole::Policy)
        || connection.peer().owner_sid() != peer.owner_sid()
    {
        return Err("session_client_authority_unavailable");
    }
    let service = service_observation().map_err(|_| "session_client_authority_unavailable")?;
    let shim = program_files()
        .map_err(|_| "session_client_authority_unavailable")?
        .join(protocol::SHIM_RELATIVE);
    protocol::service(
        &service,
        connection
            .peer()
            .peer_process_id()
            .ok_or("session_client_authority_unavailable")?,
        &shim,
    )
    .map_err(|_| "session_client_authority_unavailable")
}
fn account_sid(account: &str) -> Result<Sid, Error> {
    let account = wide(account);
    let (mut bytes, mut units, mut kind) = (0, 0, 0);
    unsafe {
        LookupAccountNameW(
            null(),
            account.as_ptr(),
            null_mut(),
            &mut bytes,
            null_mut(),
            &mut units,
            &mut kind,
        );
    }
    if bytes == 0 || bytes > MAX_NATIVE_BYTES || units > 256 {
        return Err(Error::Unavailable);
    }
    let mut sid = vec![0usize; (bytes as usize).div_ceil(std::mem::size_of::<usize>())];
    let mut domain = vec![0u16; units.max(1) as usize];
    if unsafe {
        LookupAccountNameW(
            null(),
            account.as_ptr(),
            sid.as_mut_ptr().cast(),
            &mut bytes,
            domain.as_mut_ptr(),
            &mut units,
            &mut kind,
        )
    } == 0
    {
        return Err(Error::Unavailable);
    }
    sid_value(sid.as_mut_ptr().cast())
}
fn sid_value(pointer: *mut c_void) -> Result<Sid, Error> {
    if pointer.is_null() || unsafe { IsValidSid(pointer) } == 0 {
        return Err(Error::Unavailable);
    }
    let mut output = null_mut();
    if unsafe { ConvertSidToStringSidW(pointer, &mut output) } == 0 {
        return Err(Error::Unavailable);
    }
    let _allocation = LocalAllocation(output.cast());
    Sid::parse(&unsafe { text(output) }?).map_err(|_| Error::Unavailable)
}
fn service_observation() -> Result<ServiceObservation, Error> {
    let manager = ServiceHandle(unsafe { OpenSCManagerW(null(), null(), 1) });
    if manager.0.is_null() {
        return Err(Error::Unavailable);
    }
    // QUERY_CONFIG | QUERY_STATUS only; no create/change/start/stop rights.
    let service = ServiceHandle(unsafe {
        OpenServiceW(manager.0, wide("aag_session_authority").as_ptr(), 5)
    });
    if service.0.is_null() {
        return Err(Error::Unavailable);
    }
    let mut status: ServiceStatus = unsafe { std::mem::zeroed() };
    let mut needed = 0;
    if unsafe {
        QueryServiceStatusEx(
            service.0,
            0,
            (&mut status as *mut ServiceStatus).cast(),
            std::mem::size_of::<ServiceStatus>() as u32,
            &mut needed,
        )
    } == 0
    {
        return Err(Error::Unavailable);
    }
    unsafe {
        QueryServiceConfigW(service.0, null_mut(), 0, &mut needed);
    }
    if needed < std::mem::size_of::<ServiceConfig>() as u32 || needed > MAX_NATIVE_BYTES {
        return Err(Error::Unavailable);
    }
    let mut buffer = vec![0usize; (needed as usize).div_ceil(std::mem::size_of::<usize>())];
    let capacity = needed;
    if unsafe { QueryServiceConfigW(service.0, buffer.as_mut_ptr().cast(), capacity, &mut needed) }
        == 0
    {
        return Err(Error::Unavailable);
    }
    let config = unsafe { &*buffer.as_ptr().cast::<ServiceConfig>() };
    let account = unsafe { text(config.account) }?;
    let binary = unsafe { text(config.binary) }?;
    let mut count = 0;
    let arguments = unsafe { CommandLineToArgvW(wide(&binary).as_ptr(), &mut count) };
    if arguments.is_null() || count < 1 {
        return Err(Error::Unavailable);
    }
    let _arguments = LocalAllocation(arguments.cast());
    let executable = PathBuf::from(unsafe { text(*arguments) }?);
    if status.service_type != config.service_type {
        return Err(Error::ServiceMismatch);
    }
    Ok(ServiceObservation {
        service_type: status.service_type,
        current_state: status.state,
        process_id: status.process_id,
        account,
        executable,
    })
}

/// This candidate is constructed only after a real same-handle kernel pipe
/// owner observation, SCM PID consistency and a fresh matching owner response.
struct AttestedCandidate {
    receipt: protocol::InstallationReceipt,
    peer_process_id: u32,
}

pub(super) fn verify(root: &Path) -> Result<VerifiedClientAuthority, Error> {
    protocol::validate_root(root)?;
    let program_files = program_files()?;
    let shim = program_files.join(protocol::SHIM_RELATIVE);
    let sid = account_sid(protocol::ACCOUNT)?;
    let before = service_observation()?;
    protocol::service(&before, before.process_id, &shim)?;
    let peer =
        ServicePeer::new(ServiceRole::Policy, sid.clone()).map_err(|_| Error::Unavailable)?;
    let mut random = [0u8; 32];
    getrandom::getrandom(&mut random).map_err(|_| Error::Unavailable)?;
    let challenge: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    let requested_root = root.to_owned();
    let expected_shim = shim.clone();
    // A dedicated short-lived IO runtime avoids nested block_on on the UI or
    // an async executor thread. The caller remains the existing blocking pool.
    let candidate = std::thread::Builder::new()
        .name("aag_owner_attestation".into())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|_| Error::Unavailable)?;
            let result = runtime.block_on(exchange(
                peer,
                before,
                &expected_shim,
                &requested_root,
                &challenge,
            ));
            runtime.shutdown_timeout(CLEANUP_TIMEOUT);
            result
        })
        .map_err(|_| Error::Unavailable)?
        .join()
        .map_err(|_| Error::Unavailable)??;
    let after = service_observation()?;
    protocol::service(&after, candidate.peer_process_id, &shim)?;
    let paths = protocol::paths(&candidate.receipt, &program_files)?;
    validate_canonical_root(&paths.root)?;
    let trusted_installer = account_sid(r"NT SERVICE\TrustedInstaller")?;
    let trusted = [
        "S-1-5-18".to_owned(),
        "S-1-5-32-544".to_owned(),
        sid.value().to_owned(),
        trusted_installer.value().to_owned(),
    ];
    let mut guards = Vec::new();
    qualify_image(
        &program_files,
        &paths.protected_shim,
        candidate.receipt.protected_shim.object,
        &trusted,
        &mut guards,
    )?;
    qualify_image(
        &program_files,
        &paths.pinned_native,
        candidate.receipt.pinned_native.object,
        &trusted,
        &mut guards,
    )?;
    // These are immutable-to-untrusted-callers protected images. A trusted
    // owner update must fence admissions; this is an observation, not deployment.
    Ok(VerifiedClientAuthority {
        root: paths.root,
        protected_shim: paths.protected_shim,
        pinned_native: paths.pinned_native,
    })
}

async fn exchange(
    peer: ServicePeer,
    before: ServiceObservation,
    shim: &Path,
    root: &Path,
    challenge: &str,
) -> Result<AttestedCandidate, Error> {
    let deadline = Instant::now() + QUERY_TIMEOUT;
    let limits = TransportLimits {
        frame_bytes: protocol::FRAME_LIMIT,
        queued_bytes: 2 * (protocol::FRAME_LIMIT + 4),
        queued_frames: 2,
        connections: 1,
    };
    let connection =
        LocalConnection::connect(&peer, limits, deadline, &TransportCancellation::new())
            .await
            .map_err(|_| Error::Unavailable)?;
    let result = async {
        if connection.peer().owner_sid() != peer.owner_sid()
            || connection.peer().role() != Some(ServiceRole::Policy)
        {
            return Err(Error::ServiceMismatch);
        }
        let pid = connection
            .peer()
            .peer_process_id()
            .ok_or(Error::ServiceMismatch)?;
        protocol::service(&before, pid, shim)?;
        let frame =
            TransportFrame::parse(protocol::request(root, challenge)?, protocol::FRAME_LIMIT)
                .map_err(|_| Error::InvalidRequest)?;
        if connection.send(frame, deadline).await != SendOutcome::Written {
            return Err(Error::Unavailable);
        }
        let response = connection
            .receive(deadline)
            .await
            .map_err(|_| Error::Unavailable)?;
        if response.kind() != EnvelopeKind::Response {
            return Err(Error::InvalidReceipt);
        }
        Ok(AttestedCandidate {
            receipt: protocol::receipt(response.bytes(), root, challenge)?,
            peer_process_id: pid,
        })
    }
    .await;
    if connection.close(Instant::now() + CLEANUP_TIMEOUT).await != TransportClosed::Closed {
        return Err(Error::Unavailable);
    }
    result
}

fn open_image_object(path: &Path, participating: bool) -> Result<File, Error> {
    open_image_object_diagnostic(path, participating).map_err(ImageReason::public_error)
}
fn open_image_object_diagnostic(path: &Path, participating: bool) -> Result<File, ImageReason> {
    OpenOptions::new()
        .access_mode(if participating {
            0x0012_0081
        } else {
            0x0002_0080
        })
        .share_mode(if participating { 1 } else { 7 })
        .custom_flags(0x0220_0000)
        .open(path)
        .map_err(|error| ImageReason::OpenFailed(error.raw_os_error()))
}
fn qualify_image(
    root: &Path,
    image: &Path,
    expected: Managed_directory_identity,
    trusted: &[String],
    guards: &mut Vec<File>,
) -> Result<(), Error> {
    let result = qualify_image_diagnostic(root, image, expected, trusted, guards);
    #[cfg(test)]
    if operator_probe::enabled() {
        if let Err(failure) = &result {
            eprintln!(
                "owner_image_probe stage={:?} category={:?} index={} reason={:?}",
                failure.stage, failure.category, failure.index, failure.reason
            );
        }
    }
    result.map_err(|failure| failure.reason.public_error())
}
fn qualify_image_diagnostic(
    root: &Path,
    image: &Path,
    expected: Managed_directory_identity,
    trusted: &[String],
    guards: &mut Vec<File>,
) -> Result<(), ImageFailure> {
    // Check ancestors above Program Files without taking a global drive lease.
    // Creation of unrelated children there is allowed; redirect/delete/ACL
    // authority over the admitted Program Files object is not.
    for (index, ancestor) in root.ancestors().skip(1).enumerate() {
        let category = if ancestor.parent().is_none() {
            ImageCategory::Drive
        } else {
            ImageCategory::Ancestor
        };
        let file = image_context(
            open_image_object_diagnostic(ancestor, false),
            ImageStage::AncestorOpen,
            category,
            index,
        )?;
        image_context(
            check_object(&file, None, true),
            ImageStage::AncestorObject,
            category,
            index,
        )?;
        image_context(
            check_security(&file, trusted, ANCESTOR_MUTATION_MASK),
            ImageStage::AncestorSecurity,
            category,
            index,
        )?;
    }
    let relative = image.strip_prefix(root).map_err(|_| ImageFailure {
        stage: ImageStage::RelativePath,
        category: ImageCategory::ProgramFiles,
        index: 0,
        reason: ImageReason::RelativePath,
    })?;
    let mut path = root.to_owned();
    let mut paths = vec![path.clone()];
    for part in relative.components() {
        path.push(part);
        paths.push(path.clone());
    }
    for (index, path) in paths.into_iter().enumerate() {
        let directory = path != image;
        let category = if !directory {
            ImageCategory::File
        } else if path == root {
            ImageCategory::ProgramFiles
        } else if path == root.join("WindowsApps") {
            ImageCategory::WindowsApps
        } else if path.parent() == Some(root.join("WindowsApps").as_path()) {
            ImageCategory::Package
        } else {
            ImageCategory::Directory
        };
        let file = image_context(
            open_image_object_diagnostic(&path, true),
            ImageStage::Open,
            category,
            index,
        )?;
        let object = image_context(
            check_object(
                &file,
                if directory { None } else { Some(expected) },
                directory,
            ),
            ImageStage::Object,
            category,
            index,
        )?;
        image_context(
            check_canonical_path(&file, &path),
            ImageStage::Canonical,
            category,
            index,
        )?;
        image_context(
            check_security(&file, trusted, IMAGE_MUTATION_MASK),
            ImageStage::Security,
            category,
            index,
        )?;
        // Reopen only while the original handle is retained; it must resolve
        // to the same non-reparse object. Every parent already has a guard.
        let check = image_context(
            open_image_object_diagnostic(&path, true),
            ImageStage::Reopen,
            category,
            index,
        )?;
        image_context(
            check_object(&check, Some(object), directory),
            ImageStage::Recheck,
            category,
            index,
        )?;
        guards.push(file);
    }
    Ok(())
}
fn check_object(
    file: &File,
    expected: Option<Managed_directory_identity>,
    directory: bool,
) -> Result<Managed_directory_identity, ImageReason> {
    let metadata =
        inventory_object_metadata_from_file(file).map_err(|_| ImageReason::MetadataUnavailable)?;
    let mut info: FileInformation = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(ImageReason::FileInformationFailed(unsafe {
            GetLastError()
        }));
    }
    if metadata.attributes & REPARSE_ATTRIBUTE != 0 {
        return Err(ImageReason::Reparse);
    }
    if (metadata.attributes & 0x10 != 0) != directory {
        return Err(ImageReason::ObjectKind);
    }
    if !directory && info.links != 1 {
        return Err(ImageReason::Hardlink);
    }
    if expected.is_some_and(|expected| metadata.object != expected) {
        return Err(ImageReason::ObjectIdentity);
    }
    Ok(metadata.object)
}
fn check_security(file: &File, trusted: &[String], mutation: u32) -> Result<(), ImageReason> {
    inspect_security(file, trusted, mutation, None, false).map(|_| ())
}

fn inspect_security(
    file: &File,
    trusted: &[String],
    mutation: u32,
    required_owner: Option<&str>,
    require_protected_dacl: bool,
) -> Result<String, ImageReason> {
    let (mut owner, mut acl, mut descriptor) = (null_mut(), null_mut(), null_mut());
    let status = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            1,
            5,
            &mut owner,
            null_mut(),
            &mut acl,
            null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 {
        return Err(ImageReason::DescriptorQuery(status));
    }
    if descriptor.is_null() {
        return Err(ImageReason::DescriptorMissing);
    }
    let _descriptor = LocalAllocation(descriptor);
    let owner = sid_value(owner).map_err(|_| ImageReason::OwnerSidUnavailable)?;
    if acl.is_null() {
        return Err(ImageReason::NullDacl);
    }
    if !trusted.iter().any(|trusted| trusted == owner.value()) {
        return Err(ImageReason::UntrustedOwner);
    }
    if required_owner.is_some_and(|required| owner.value() != required) {
        return Err(ImageReason::UntrustedOwner);
    }
    if require_protected_dacl {
        let (mut control, mut revision) = (0, 0);
        if unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) } == 0
            || control & 0x1000 == 0
        {
            return Err(ImageReason::UntrustedOwner);
        }
    }
    let mut information: AclSize = unsafe { std::mem::zeroed() };
    if unsafe {
        GetAclInformation(
            acl,
            (&mut information as *mut AclSize).cast(),
            std::mem::size_of::<AclSize>() as u32,
            2,
        )
    } == 0
    {
        return Err(ImageReason::AclQuery(unsafe { GetLastError() }));
    }
    if information.count > 256 {
        return Err(ImageReason::TooManyAces);
    }
    let acl_start = acl as usize;
    if information.in_use < 8 || information.in_use > MAX_NATIVE_BYTES {
        return Err(ImageReason::MalformedAce);
    }
    let acl_end = acl_start
        .checked_add(information.in_use as usize)
        .ok_or(ImageReason::MalformedAce)?;
    for index in 0..information.count {
        let mut pointer = null_mut();
        if unsafe { GetAce(acl, index, &mut pointer) } == 0 || pointer.is_null() {
            return Err(ImageReason::AceQuery(unsafe { GetLastError() }));
        }
        let ace_start = pointer as usize;
        if ace_start < acl_start + 8 || ace_start.checked_add(4).is_none_or(|end| end > acl_end) {
            return Err(ImageReason::MalformedAce);
        }
        let header = unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), 4) };
        let size = usize::from(u16::from_le_bytes([header[2], header[3]]));
        if ace_start.checked_add(size).is_none_or(|end| end > acl_end) {
            return Err(ImageReason::MalformedAce);
        }
        let bytes = unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), size) };
        let ace = parse_access_ace(bytes)?;
        let sid = sid_value(ace.sid.as_ptr().cast_mut().cast())
            .map_err(|_| ImageReason::AceSidUnavailable)?;
        check_ace_permissions(
            &ace,
            trusted.iter().any(|trusted| trusted == sid.value()),
            mutation,
        )?;
    }
    Ok(owner.value().to_owned())
}

fn check_canonical_path(file: &File, expected: &Path) -> Result<(), ImageReason> {
    let mut buffer = vec![0u16; MAX_TEXT_UNITS];
    let units = unsafe {
        GetFinalPathNameByHandleW(
            file.as_raw_handle(),
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            0,
        )
    } as usize;
    if units == 0 {
        return Err(ImageReason::CanonicalQuery(unsafe { GetLastError() }));
    }
    if units >= buffer.len() {
        return Err(ImageReason::CanonicalLength);
    }
    let value = String::from_utf16(&buffer[..units]).map_err(|_| ImageReason::CanonicalEncoding)?;
    let canonical = value
        .strip_prefix(r"\\?\")
        .ok_or(ImageReason::CanonicalPrefix)?;
    if !canonical.eq_ignore_ascii_case(expected.to_str().ok_or(ImageReason::CanonicalEncoding)?) {
        return Err(ImageReason::CanonicalMismatch);
    }
    Ok(())
}
fn validate_canonical_root(root: &Path) -> Result<(), Error> {
    for path in root.ancestors() {
        let file = open_image_object(path, false)?;
        check_object(&file, None, true).map_err(ImageReason::public_error)?;
        check_canonical_path(&file, path).map_err(ImageReason::public_error)?;
    }
    Ok(())
}

#[cfg(test)]
mod operator_probe {
    use super::*;
    const IMAGE: &str = r"C:\Program Files\WindowsApps\OpenAI.Codex_26.1021.3705.0_x64__2p2nqsd0c76g0\app\resources\codex.exe";
    #[derive(Debug)]
    enum Stage {
        Gate,
        KnownFolder,
        Bind,
        Identity,
        TrustedInstaller,
        Qualify,
    }
    pub(super) fn enabled() -> bool {
        std::env::var_os("AAG_NATIVE_IMAGE_PROBE").as_deref() == Some(std::ffi::OsStr::new(IMAGE))
    }
    fn checked<T, E: std::fmt::Debug>(stage: Stage, value: Result<T, E>) -> T {
        value.unwrap_or_else(|error| panic!("owner_image_probe stage={stage:?} code={error:?}"))
    }
    #[test]
    #[ignore = "explicit read-only operator image qualification; requires exact gate"]
    fn installed_vendor_image_chain_probe() {
        checked(
            Stage::Gate,
            if std::env::var_os("AAG_NATIVE_IMAGE_PROBE").as_deref()
                == Some(std::ffi::OsStr::new(IMAGE))
            {
                Ok(())
            } else {
                Err(Error::InvalidRequest)
            },
        );
        let root = checked(Stage::KnownFolder, program_files());
        checked(
            Stage::Gate,
            if root == Path::new(r"C:\Program Files") {
                Ok(())
            } else {
                Err(Error::InstallationMismatch)
            },
        );
        let image = Path::new(IMAGE);
        let relative = checked(
            Stage::Gate,
            image
                .strip_prefix(&root)
                .map_err(|_| Error::InstallationMismatch),
        );
        let mut paths = vec![root.clone()];
        for part in relative.components() {
            paths.push(paths.last().unwrap().join(part));
        }
        let mut bound = Vec::new();
        let mut expected = None;
        for path in paths {
            let file = checked(Stage::Bind, open_image_object(&path, true));
            let object = checked(Stage::Identity, check_object(&file, None, path != image));
            if path == image {
                expected = Some(object);
            }
            bound.push(file);
        }
        let installer = checked(
            Stage::TrustedInstaller,
            account_sid(r"NT SERVICE\TrustedInstaller"),
        );
        let trusted = [
            "S-1-5-18".to_owned(),
            "S-1-5-32-544".to_owned(),
            installer.value().to_owned(),
        ];
        checked(
            Stage::Qualify,
            qualify_image(
                &root,
                image,
                expected.expect("owner_image_probe_identity_missing"),
                &trusted,
                &mut bound,
            ),
        );
        eprintln!("owner_image_probe stage=Qualify code=ok");
    }
}

#[cfg(test)]
mod callback_ace_tests {
    use super::*;
    fn callback(kind: u8, mask: u32) -> Vec<u8> {
        // Synthetic S-1-1-0 followed by opaque, non-evaluated application data.
        let mut bytes = vec![kind, 0, 24, 0];
        bytes.extend(mask.to_le_bytes());
        bytes.extend([1, 1, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0]);
        bytes.extend([0x61, 0x72, 0x74, 0x78]);
        bytes
    }
    #[test]
    fn callback_read_allows_are_bounded_and_mutable_untrusted_allows_stay_rejected() {
        let readonly = callback(9, 0x0012_00a9);
        let parsed = parse_access_ace(&readonly).unwrap();
        assert_eq!(parsed.sid.len(), 12);
        check_ace_permissions(&parsed, false, IMAGE_MUTATION_MASK).unwrap();
        let mutable = callback(9, 0x0004_0000);
        assert!(matches!(
            check_ace_permissions(
                &parse_access_ace(&mutable).unwrap(),
                false,
                IMAGE_MUTATION_MASK
            ),
            Err(ImageReason::UntrustedMutation)
        ));
        let denied = callback(10, 0x001f_01ff);
        check_ace_permissions(
            &parse_access_ace(&denied).unwrap(),
            false,
            IMAGE_MUTATION_MASK,
        )
        .unwrap();
        let mut bad_sid = readonly.clone();
        bad_sid[9] = 15;
        assert!(matches!(
            parse_access_ace(&bad_sid),
            Err(ImageReason::MalformedAce)
        ));
        let mut bad_size = readonly.clone();
        bad_size[2] = 28;
        assert!(matches!(
            parse_access_ace(&bad_size),
            Err(ImageReason::MalformedAce)
        ));
        let mut unsupported = readonly;
        unsupported[0] = 11;
        assert!(matches!(
            parse_access_ace(&unsupported),
            Err(ImageReason::UnsupportedAce(11))
        ));
    }
}
