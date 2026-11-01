//! Read-only configuration admission for the protected Windows service roles.
//! Production roots come only from the OS ProgramData known-folder lookup.
//! No configuration body, path, or credential is included in diagnostics.

use crate::{
    config::private_fs::Managed_directory_identity,
    services::session_broker::{service_host::ServiceRole, windows_isolation::Sid},
};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom},
    os::windows::{fs::OpenOptionsExt, io::AsRawHandle},
    path::{Component, Path, PathBuf, Prefix},
};

const MAX_CONFIG_BYTES: usize = 1024 * 1024;
const ANCESTOR_MUTATION: u32 = 0x500d_0150;
const PROTECTED_MUTATION: u32 = 0x500d_0156;
// OS ancestors rely on admitted ACLs and canonical/object rechecks. A global
// listing lease there would block unrelated applications' atomic child writes.
const READ_ANCESTOR_SECURITY: u32 = 0x0002_0080;
// Protected directories include listing access so their strict sharing guards
// participate in Windows sharing checks; attribute-only opens are exempt.
const READ_DIRECTORY_SECURITY: u32 = 0x0012_0081;
const READ_FILE_SECURITY_DATA: u32 = 0x0012_0089;
const NOFOLLOW_BACKUP_SEMANTICS: u32 = 0x0220_0000;
const DIRECTORY_ATTRIBUTE: u32 = 0x10;
const REPARSE_ATTRIBUTE: u32 = 0x400;
const SYSTEM_SID: &str = "S-1-5-18";
const ADMINISTRATORS_SID: &str = "S-1-5-32-544";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConfigError {
    PolicyOwnerInvalid,
    KnownFolderUnavailable,
    PathInvalid,
    OpenFailed,
    ObjectInvalid,
    SecurityInvalid,
    FileTooLarge,
    ReadFailed,
    ObjectChanged,
}
impl std::fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::PolicyOwnerInvalid => "protected_config_policy_owner_invalid",
            Self::KnownFolderUnavailable => "protected_config_known_folder_unavailable",
            Self::PathInvalid => "protected_config_path_invalid",
            Self::OpenFailed => "protected_config_open_failed",
            Self::ObjectInvalid => "protected_config_object_invalid",
            Self::SecurityInvalid => "protected_config_security_invalid",
            Self::FileTooLarge => "protected_config_too_large",
            Self::ReadFailed => "protected_config_read_failed",
            Self::ObjectChanged => "protected_config_object_changed",
        })
    }
}
impl std::error::Error for ConfigError {}

/// The parent constructs this only from its trusted Policy-role SCM account
/// lookup. This type validates the role/SID shape; it does not attest an RPC.
pub(crate) struct ExpectedPolicyOwner {
    sid: Sid,
}
impl ExpectedPolicyOwner {
    pub fn from_scm_role_lookup(role: ServiceRole, sid: Sid) -> Result<Self, ConfigError> {
        let suffix = sid
            .value()
            .strip_prefix("S-1-5-80-")
            .ok_or(ConfigError::PolicyOwnerInvalid)?;
        let components: Vec<_> = suffix.split('-').collect();
        if role != ServiceRole::Policy
            || components.len() != 5
            || components.iter().any(|part| {
                part.parse::<u32>()
                    .map_or(true, |value| value.to_string() != *part)
            })
        {
            return Err(ConfigError::PolicyOwnerInvalid);
        }
        Ok(Self { sid })
    }
    pub fn sid(&self) -> &Sid {
        &self.sid
    }
}

struct ConfigRead {
    contents: Vec<u8>,
    object: Managed_directory_identity,
    owner_sid: String,
    _guards: Vec<File>,
}

/// Not Debug, Clone, or Serde. Read guards remain owned with the admitted bytes.
pub(crate) struct VerifiedConfigBytes {
    role: ServiceRole,
    read: ConfigRead,
}
impl VerifiedConfigBytes {
    pub fn contents(&self) -> &[u8] {
        &self.read.contents
    }
    pub fn object(&self) -> Managed_directory_identity {
        self.read.object
    }
    pub fn owner_sid(&self) -> &str {
        &self.read.owner_sid
    }
    pub fn role(&self) -> ServiceRole {
        self.role
    }
}

pub(crate) fn load(
    role: ServiceRole,
    owner: &ExpectedPolicyOwner,
) -> Result<VerifiedConfigBytes, ConfigError> {
    let program_data = super::owner_client::program_data_folder()
        .map_err(|_| ConfigError::KnownFolderUnavailable)?;
    validate_disk_path(&program_data)?;
    let product_root = program_data.join("AI Agent Gateway");
    let authority_root = product_root.join("SessionAuthority");
    // The complete prefix is opened root first. No environment, user-home,
    // working-directory, or arbitrary-path fallback exists at this boundary.
    let mut paths: Vec<_> = program_data.ancestors().map(Path::to_owned).collect();
    paths.reverse();
    paths.extend([product_root, authority_root.clone()]);
    let read = read_config(paths, &authority_root, role, owner.sid.value())?;
    Ok(VerifiedConfigBytes { role, read })
}

/// Only an independently created PrivateTempDir can enter the native fixture
/// reader. This value deliberately cannot stand in for VerifiedConfigBytes.
#[cfg(test)]
struct FixtureConfigBytes(ConfigRead);
#[cfg(test)]
fn load_fixture(
    root: &crate::config::private_fs::PrivateTempDir,
    role: ServiceRole,
) -> Result<FixtureConfigBytes, ConfigError> {
    crate::config::private_fs::verify_private_dir(root.path())
        .map_err(|_| ConfigError::SecurityInvalid)?;
    let owner = fixture_identity::current_user_sid()?;
    read_config(vec![root.path().to_owned()], root.path(), role, &owner).map(FixtureConfigBytes)
}
#[cfg(test)]
fn fixture_ace_check(bytes: &[u8], trusted: bool, ancestor: bool) -> Result<(), ConfigError> {
    super::owner_client::check_config_access_ace(
        bytes,
        trusted,
        if ancestor {
            ANCESTOR_MUTATION
        } else {
            PROTECTED_MUTATION
        },
    )
    .map_err(|_| ConfigError::SecurityInvalid)
}

struct ReadGuard {
    file: File,
    path: PathBuf,
    object: Managed_directory_identity,
    protected: bool,
}

/// Internal execution path shared by the fixed production root and the typed
/// test root. Only load() can construct a production VerifiedConfigBytes.
fn read_config(
    mut paths: Vec<PathBuf>,
    authority_root: &Path,
    role: ServiceRole,
    owner: &str,
) -> Result<ConfigRead, ConfigError> {
    paths.push(authority_root.join("runtime"));
    let file_path = authority_root
        .join("runtime")
        .join(format!("{}.json", role.service_name()));
    let trusted = [
        SYSTEM_SID.to_owned(),
        ADMINISTRATORS_SID.to_owned(),
        owner.to_owned(),
    ];
    let mut guards = Vec::with_capacity(paths.len() + 1);
    for path in paths {
        validate_disk_path(&path)?;
        let protected = path.starts_with(authority_root);
        let file = open_object(&path, true, protected)?;
        let object = object_record(&file, true)?.object;
        check_path_security(&file, &path, &trusted, owner, protected)?;
        guards.push(ReadGuard {
            file,
            path,
            object,
            protected,
        });
    }
    let mut file = open_object(&file_path, false, true)?;
    let before = object_record(&file, false)?;
    let owner_sid = check_path_security(&file, &file_path, &trusted, owner, true)?;
    let contents = read_bounded(&mut file)?;
    if contents.len() as u64 != before.bytes {
        return Err(ConfigError::ObjectChanged);
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|_| ConfigError::ReadFailed)?;
    let readback = read_bounded(&mut file)?;
    if contents != readback || object_record(&file, false)? != before {
        return Err(ConfigError::ObjectChanged);
    }
    if check_path_security(&file, &file_path, &trusted, owner, true)? != owner_sid {
        return Err(ConfigError::ObjectChanged);
    }
    for guard in &guards {
        if object_record(&guard.file, true)?.object != guard.object {
            return Err(ConfigError::ObjectChanged);
        }
        check_path_security(&guard.file, &guard.path, &trusted, owner, guard.protected)?;
    }
    let mut handles: Vec<_> = guards.into_iter().map(|guard| guard.file).collect();
    handles.push(file);
    Ok(ConfigRead {
        contents,
        object: before.object,
        owner_sid,
        _guards: handles,
    })
}

fn open_object(path: &Path, directory: bool, protected: bool) -> Result<File, ConfigError> {
    OpenOptions::new()
        .access_mode(if directory && !protected {
            READ_ANCESTOR_SECURITY
        } else if directory {
            READ_DIRECTORY_SECURITY
        } else {
            READ_FILE_SECURITY_DATA
        })
        // Higher metadata guards do not impose a global write lease. Their
        // protection comes from the validated SY/BA/A ACL boundary. The A-owned
        // subtree and final file retain strict data-write/delete exclusion.
        .share_mode(if directory && !protected { 3 } else { 1 })
        .custom_flags(NOFOLLOW_BACKUP_SEMANTICS)
        .open(path)
        .map_err(|_| ConfigError::OpenFailed)
}

fn check_path_security(
    file: &File,
    path: &Path,
    trusted: &[String],
    owner: &str,
    protected: bool,
) -> Result<String, ConfigError> {
    super::owner_client::canonical_config_path(file, path).map_err(|_| ConfigError::PathInvalid)?;
    super::owner_client::protected_object_security(
        file,
        trusted,
        if protected {
            PROTECTED_MUTATION
        } else {
            ANCESTOR_MUTATION
        },
        if protected { Some(owner) } else { None },
        protected,
    )
    .map_err(|_| ConfigError::SecurityInvalid)
}

fn read_bounded(file: &mut File) -> Result<Vec<u8>, ConfigError> {
    let mut bytes = Vec::new();
    file.take(MAX_CONFIG_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ConfigError::ReadFailed)?;
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err(ConfigError::FileTooLarge);
    }
    Ok(bytes)
}

fn validate_disk_path(path: &Path) -> Result<(), ConfigError> {
    if !path.is_absolute()
        || path
            .to_str()
            .is_none_or(|text| text.chars().any(char::is_control))
    {
        return Err(ConfigError::PathInvalid);
    }
    let mut parts = path.components();
    if !matches!(parts.next(), Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::Disk(_)))
        || !matches!(parts.next(), Some(Component::RootDir))
    {
        return Err(ConfigError::PathInvalid);
    }
    for part in parts {
        match part {
            Component::Normal(text)
                if text
                    .to_str()
                    .is_some_and(|text| !text.contains(':') && !text.ends_with([' ', '.'])) => {}
            _ => return Err(ConfigError::PathInvalid),
        }
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct ObjectRecord {
    object: Managed_directory_identity,
    attributes: u32,
    created: [u32; 2],
    written: [u32; 2],
    bytes: u64,
    links: u32,
}
#[repr(C)]
struct FileInformation {
    attributes: u32,
    created: [u32; 2],
    accessed: [u32; 2],
    written: [u32; 2],
    volume: u32,
    size_high: u32,
    size_low: u32,
    links: u32,
    index_high: u32,
    index_low: u32,
}
#[link(name = "kernel32")]
extern "system" {
    fn GetFileInformationByHandle(
        file: *mut std::ffi::c_void,
        information: *mut FileInformation,
    ) -> i32;
}
fn object_record(file: &File, directory: bool) -> Result<ObjectRecord, ConfigError> {
    let mut information: FileInformation = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) } == 0 {
        return Err(ConfigError::ObjectInvalid);
    }
    if information.attributes & REPARSE_ATTRIBUTE != 0
        || (information.attributes & DIRECTORY_ATTRIBUTE != 0) != directory
        || (!directory && information.links != 1)
    {
        return Err(ConfigError::ObjectInvalid);
    }
    let bytes = (u64::from(information.size_high) << 32) | u64::from(information.size_low);
    if !directory && bytes > MAX_CONFIG_BYTES as u64 {
        return Err(ConfigError::FileTooLarge);
    }
    Ok(ObjectRecord {
        object: Managed_directory_identity {
            volume: u64::from(information.volume),
            object: (u64::from(information.index_high) << 32) | u64::from(information.index_low),
        },
        attributes: information.attributes,
        created: information.created,
        written: information.written,
        bytes,
        links: information.links,
    })
}

/// Process TokenUser is used only by the PrivateTempDir fixture lane. Production
/// ownership never comes from the current user's token or environment.
#[cfg(test)]
mod fixture_identity {
    use super::ConfigError;
    use std::{
        ffi::c_void,
        os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
        ptr::null_mut,
    };
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn LocalFree(pointer: *mut c_void) -> *mut c_void;
    }
    #[link(name = "advapi32")]
    extern "system" {
        fn OpenProcessToken(process: *mut c_void, access: u32, token: *mut *mut c_void) -> i32;
        fn GetTokenInformation(
            token: *mut c_void,
            class: u32,
            output: *mut c_void,
            bytes: u32,
            needed: *mut u32,
        ) -> i32;
        fn ConvertSidToStringSidW(sid: *mut c_void, text: *mut *mut u16) -> i32;
    }
    pub(super) fn current_user_sid() -> Result<String, ConfigError> {
        let mut handle = null_mut();
        if unsafe { OpenProcessToken(GetCurrentProcess(), 8, &mut handle) } == 0 || handle.is_null()
        {
            return Err(ConfigError::SecurityInvalid);
        }
        let token = unsafe { OwnedHandle::from_raw_handle(handle) };
        let mut needed = 0;
        unsafe {
            GetTokenInformation(token.as_raw_handle(), 1, null_mut(), 0, &mut needed);
        }
        if needed < std::mem::size_of::<usize>() as u32 || needed > 64 * 1024 {
            return Err(ConfigError::SecurityInvalid);
        }
        let mut buffer = vec![0usize; (needed as usize).div_ceil(std::mem::size_of::<usize>())];
        if unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                1,
                buffer.as_mut_ptr().cast(),
                needed,
                &mut needed,
            )
        } == 0
        {
            return Err(ConfigError::SecurityInvalid);
        }
        let sid = unsafe { *buffer.as_ptr().cast::<*mut c_void>() };
        let mut text = null_mut();
        if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 || text.is_null() {
            return Err(ConfigError::SecurityInvalid);
        }
        let mut units = 0;
        while units < 256 && unsafe { *text.add(units) } != 0 {
            units += 1;
        }
        let value = if units < 256 {
            String::from_utf16(unsafe { std::slice::from_raw_parts(text, units) })
                .map_err(|_| ConfigError::SecurityInvalid)
        } else {
            Err(ConfigError::SecurityInvalid)
        };
        unsafe {
            LocalFree(text.cast());
        }
        value
    }
}

#[cfg(test)]
mod tests;
