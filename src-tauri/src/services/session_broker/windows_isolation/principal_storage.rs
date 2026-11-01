//! Fresh storage owned by policy A, readable by trusted C and optionally
//! writable by one never-reused B-generation principal. No path adoption.

use super::super::service_host::protected_process::PolicyProcessProtection;
use super::native::{
    ffi::*,
    sid::{
        descriptor_acl, descriptor_owner, security_descriptor, set_file_acl, sid_from_pointer,
        wide, Descriptor,
    },
    token::{own, token_user},
};
use super::{IsolationError, Sid};
use crate::config::private_fs::{
    inspect_directory_object, inventory_object_metadata_from_file, Managed_directory_identity,
    NamespaceLease,
};
use std::{
    collections::BTreeMap,
    ffi::c_void,
    fs::{File, OpenOptions},
    os::windows::{fs::OpenOptionsExt, io::AsRawHandle},
    path::{Component, Path, PathBuf},
    ptr::null_mut,
};

const FULL_FILE_ACCESS: u32 = 0x001f_01ff;
const READ_EXECUTE: u32 = 0x0012_00a9;
const DIRECTORY_DATA_ACCESS: u32 = 0x0013_00bf;
const FILE_DATA_ACCESS: u32 = 0x0013_01bf;
const READ_CONTROL: u32 = 0x0002_0000;
const WRITE_DAC: u32 = 0x0004_0000;
const READ_DATA_OR_LIST_DIRECTORY: u32 = 0x1;
const READ_ATTRIBUTES: u32 = 0x80;
const DIRECTORY_ATTRIBUTE: u32 = 0x10;
const REPARSE_ATTRIBUTE: u32 = 0x400;
const NO_FOLLOW_BACKUP: u32 = 0x0220_0000;
const PROTECTED_DACL: u16 = 0x1000;
const ERROR_NO_TOKEN: u32 = 1008;

#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentThread() -> Handle;
}
#[link(name = "advapi32")]
extern "system" {
    fn OpenThreadToken(thread: Handle, access: u32, open_as_self: i32, token: *mut Handle) -> i32;
    fn GetSecurityDescriptorControl(
        descriptor: *const c_void,
        control: *mut u16,
        revision: *mut u32,
    ) -> i32;
}

#[derive(Clone, PartialEq, Eq)]
struct PolicyBinding {
    owner: Sid,
    reader: Sid,
    writer: Option<Sid>,
}

#[cfg(test)]
#[derive(Clone, PartialEq, Eq)]
struct FixtureBinding {
    parent: PathBuf,
    identity: Managed_directory_identity,
}

pub(crate) struct PrincipalStoragePolicy {
    owner: Sid,
    reader: Sid,
    writer: Option<Sid>,
    #[cfg(test)]
    fixture: Option<FixtureBinding>,
}

pub(crate) struct FreshPrincipalRoot {
    path: PathBuf,
    identity: Managed_directory_identity,
    lease: NamespaceLease,
    policy: PolicyBinding,
    #[cfg(test)]
    fixture: Option<FixtureBinding>,
}

impl PrincipalStoragePolicy {
    pub fn new(owner: &Sid, reader: &Sid, writer: Option<&Sid>) -> Result<Self, IsolationError> {
        if !virtual_service_sid(owner)
            || !virtual_service_sid(reader)
            || owner == reader
            || writer.is_some_and(|writer| {
                !virtual_service_sid(writer) || writer == owner || writer == reader
            })
        {
            return Err(IsolationError::StorageBoundaryInvalid);
        }
        let policy = Self {
            owner: owner.clone(),
            reader: reader.clone(),
            writer: writer.cloned(),
            #[cfg(test)]
            fixture: None,
        };
        policy.directory_sddl()?;
        policy.file_sddl()?;
        Ok(policy)
    }
    pub fn directory_sddl(&self) -> Result<String, IsolationError> {
        self.sddl(true)
    }
    pub fn file_sddl(&self) -> Result<String, IsolationError> {
        self.sddl(false)
    }
    pub fn create_fresh_directory(
        &self,
        protection: &PolicyProcessProtection,
        parent: &NamespaceLease,
        leaf_name: &str,
    ) -> Result<FreshPrincipalRoot, IsolationError> {
        self.validate_authority(protection)?;
        self.create_directory(parent, leaf_name)
    }
    pub fn inspect_owned_root_actual(
        &self,
        root: &FreshPrincipalRoot,
    ) -> Result<(), IsolationError> {
        self.validate_root_binding(root)?;
        root.lease
            .verify()
            .map_err(|_| IsolationError::StorageBoundaryInvalid)?;
        let object = open_object(&root.path, false)?;
        verify_object(&object, root.identity, true)?;
        self.verify_exact_acl(&object, true)?;
        root.lease
            .verify()
            .map_err(|_| IsolationError::StorageBoundaryInvalid)
    }
    pub fn apply_fresh_object(
        &self,
        protection: &PolicyProcessProtection,
        root: &FreshPrincipalRoot,
        relative_path: &Path,
        expected: Managed_directory_identity,
    ) -> Result<(), IsolationError> {
        self.validate_authority(protection)?;
        self.apply_object(root, relative_path, expected)
    }

    fn binding(&self) -> PolicyBinding {
        PolicyBinding {
            owner: self.owner.clone(),
            reader: self.reader.clone(),
            writer: self.writer.clone(),
        }
    }
    fn grants(&self, directory: bool) -> BTreeMap<String, (u32, u8)> {
        let flags = if directory { 3 } else { 0 };
        let mut grants = BTreeMap::from([
            (self.owner.value().to_owned(), (FULL_FILE_ACCESS, flags)),
            ("S-1-5-18".to_owned(), (FULL_FILE_ACCESS, flags)),
            ("S-1-5-32-544".to_owned(), (FULL_FILE_ACCESS, flags)),
            ("S-1-3-4".to_owned(), (READ_CONTROL, flags)),
            (self.reader.value().to_owned(), (READ_EXECUTE, flags)),
        ]);
        if let Some(writer) = &self.writer {
            grants.insert(
                writer.value().to_owned(),
                (
                    if directory {
                        DIRECTORY_DATA_ACCESS
                    } else {
                        FILE_DATA_ACCESS
                    },
                    flags,
                ),
            );
        }
        grants
    }
    fn sddl(&self, directory: bool) -> Result<String, IsolationError> {
        let flags = if directory { "OICI" } else { "" };
        let mut sddl = format!("O:{}D:P", self.owner.value());
        for (sid, (mask, _)) in self.grants(directory) {
            sddl.push_str(&format!("(A;{flags};0x{mask:08x};;;{sid})"));
        }
        Descriptor::new(&sddl)?;
        Ok(sddl)
    }
    fn validate_authority(
        &self,
        protection: &PolicyProcessProtection,
    ) -> Result<(), IsolationError> {
        #[cfg(test)]
        if self.fixture.is_some() {
            return Err(IsolationError::AuthorityContextRequired);
        }
        if protection.process_id() != unsafe { GetCurrentProcessId() }
            || protection.user_sid() != self.owner.value()
            || current_user()? != self.owner
        {
            return Err(IsolationError::AuthorityContextRequired);
        }
        Ok(())
    }
    fn validate_root_binding(&self, root: &FreshPrincipalRoot) -> Result<(), IsolationError> {
        if root.policy != self.binding() {
            return Err(IsolationError::StorageBoundaryInvalid);
        }
        #[cfg(test)]
        if root.fixture != self.fixture {
            return Err(IsolationError::StorageBoundaryInvalid);
        }
        Ok(())
    }
    fn verify_exact_acl(&self, object: &File, directory: bool) -> Result<(), IsolationError> {
        let (owner, grants) = read_acl(object)?;
        if owner != self.owner || grants != self.grants(directory) {
            return Err(IsolationError::StorageBoundaryInvalid);
        }
        Ok(())
    }
    fn verify_namespace_parent(&self, parent: &NamespaceLease) -> Result<(), IsolationError> {
        parent
            .verify()
            .map_err(|_| IsolationError::StorageBoundaryInvalid)?;
        for (path, expected) in parent.directory_objects() {
            let object = open_object(path, false)?;
            verify_object(&object, expected, true)?;
            let (owner, grants) = read_acl(&object)?;
            if owner != self.owner
                || !grants
                    .get(self.owner.value())
                    .is_some_and(|(mask, _)| *mask == FULL_FILE_ACCESS)
            {
                return Err(IsolationError::StorageBoundaryInvalid);
            }
            for (sid, (mask, flags)) in grants {
                let allowed =
                    if sid == self.owner.value() || sid == "S-1-5-18" || sid == "S-1-5-32-544" {
                        mask == FULL_FILE_ACCESS
                    } else if sid == "S-1-3-4" {
                        mask == READ_CONTROL
                    } else if sid == self.reader.value() {
                        mask == READ_EXECUTE
                    } else {
                        false
                    };
                if !allowed || flags & !3 != 0 {
                    return Err(IsolationError::AncestorWriteGrant);
                }
            }
        }
        parent
            .verify()
            .map_err(|_| IsolationError::StorageBoundaryInvalid)
    }
    fn create_directory(
        &self,
        parent: &NamespaceLease,
        leaf_name: &str,
    ) -> Result<FreshPrincipalRoot, IsolationError> {
        let relative = Path::new(leaf_name);
        if relative.components().count() != 1
            || !matches!(relative.components().next(), Some(Component::Normal(_)))
        {
            return Err(IsolationError::InvalidInput);
        }
        let path = parent
            .child_path(relative)
            .map_err(|_| IsolationError::InvalidInput)?;
        self.verify_namespace_parent(parent)?;
        let descriptor = Descriptor::new(&self.directory_sddl()?)?;
        #[repr(C)]
        struct SecurityAttributes {
            length: u32,
            descriptor: *mut c_void,
            inherit: i32,
        }
        let attributes = SecurityAttributes {
            length: std::mem::size_of::<SecurityAttributes>() as u32,
            descriptor: descriptor.pointer(),
            inherit: 0,
        };
        let native_path = path.to_str().ok_or(IsolationError::InvalidInput)?;
        // No existing object is opened for adoption. Failure may leave a fresh
        // quarantined directory; the owner reconciles it before another attempt.
        if unsafe {
            CreateDirectoryW(
                wide(native_path)?.as_ptr(),
                (&attributes as *const SecurityAttributes).cast(),
            )
        } == 0
        {
            return Err(IsolationError::StorageAccessFailed);
        }
        let identity =
            inspect_directory_object(&path).map_err(|_| IsolationError::StorageBoundaryInvalid)?;
        let lease = NamespaceLease::acquire(
            parent.admitted_root(),
            &path,
            parent.root_identity(),
            identity,
        )
        .map_err(|_| IsolationError::StorageBoundaryInvalid)?;
        let root = FreshPrincipalRoot {
            path,
            identity,
            lease,
            policy: self.binding(),
            #[cfg(test)]
            fixture: self.fixture.clone(),
        };
        self.inspect_owned_root_actual(&root)?;
        parent
            .verify()
            .map_err(|_| IsolationError::StorageBoundaryInvalid)?;
        Ok(root)
    }
    fn apply_object(
        &self,
        root: &FreshPrincipalRoot,
        relative: &Path,
        expected: Managed_directory_identity,
    ) -> Result<(), IsolationError> {
        self.inspect_owned_root_actual(root)?;
        let path = root
            .lease
            .child_path(relative)
            .map_err(|_| IsolationError::InvalidInput)?;
        let parent_path = path.parent().ok_or(IsolationError::InvalidInput)?;
        let parent_identity = inspect_directory_object(parent_path)
            .map_err(|_| IsolationError::StorageBoundaryInvalid)?;
        let parent =
            NamespaceLease::acquire(&root.path, parent_path, root.identity, parent_identity)
                .map_err(|_| IsolationError::StorageBoundaryInvalid)?;
        let object = open_object(&path, true)?;
        let metadata = inventory_object_metadata_from_file(&object)
            .map_err(|_| IsolationError::StorageAccessFailed)?;
        let directory = metadata.attributes & DIRECTORY_ATTRIBUTE != 0;
        verify_object(&object, expected, directory)?;
        // Fresh copy targets may still have inherited ACLs. Their existing
        // owner must already be A; only the DACL is translated below.
        let mut owner_descriptor = security_descriptor(object.as_raw_handle())?;
        let actual_owner = descriptor_owner(owner_descriptor.as_mut_ptr().cast())?;
        if expected.volume != root.identity.volume || actual_owner != self.owner {
            return Err(IsolationError::StorageBoundaryInvalid);
        }
        let descriptor = Descriptor::new(&self.sddl(directory)?)?;
        set_file_acl(object.as_raw_handle(), &descriptor)?;
        self.verify_exact_acl(&object, directory)?;
        verify_object(&object, expected, directory)?;
        parent
            .verify()
            .map_err(|_| IsolationError::StorageBoundaryInvalid)?;
        self.inspect_owned_root_actual(root)
    }
}
impl FreshPrincipalRoot {
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn identity(&self) -> Managed_directory_identity {
        self.identity
    }
}

#[cfg(test)]
impl PrincipalStoragePolicy {
    fn for_fixture(
        parent: &crate::config::private_fs::PrivateTempDir,
    ) -> Result<Self, IsolationError> {
        let owner = current_user()?;
        let identity = inspect_directory_object(parent.path())
            .map_err(|_| IsolationError::StorageBoundaryInvalid)?;
        let object = open_object(parent.path(), false)?;
        verify_object(&object, identity, true)?;
        if read_acl(&object)?.0 != owner {
            return Err(IsolationError::StorageBoundaryInvalid);
        }
        let candidates = [
            "S-1-5-80-6-7-8-9-10",
            "S-1-5-80-11-12-13-14-15",
            "S-1-5-80-16-17-18-19-20",
        ];
        let mut roles = candidates
            .iter()
            .map(|value| Sid::parse(value))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|sid| sid != &owner);
        let reader = roles.next().ok_or(IsolationError::FixtureFailed)?;
        let writer = roles.next().ok_or(IsolationError::FixtureFailed)?;
        let policy = Self {
            owner,
            reader,
            writer: Some(writer),
            fixture: Some(FixtureBinding {
                parent: parent.path().to_owned(),
                identity,
            }),
        };
        Ok(policy)
    }
    fn create_fixture_directory(
        &self,
        parent: &crate::config::private_fs::PrivateTempDir,
        leaf_name: &str,
    ) -> Result<FreshPrincipalRoot, IsolationError> {
        let binding = self
            .fixture
            .as_ref()
            .ok_or(IsolationError::AuthorityContextRequired)?;
        if current_user()? != self.owner
            || parent.path() != binding.parent
            || inspect_directory_object(parent.path())
                .map_err(|_| IsolationError::StorageBoundaryInvalid)?
                != binding.identity
        {
            return Err(IsolationError::AuthorityContextRequired);
        }
        let lease = NamespaceLease::acquire(
            parent.path(),
            parent.path(),
            binding.identity,
            binding.identity,
        )
        .map_err(|_| IsolationError::StorageBoundaryInvalid)?;
        self.create_directory(&lease, leaf_name)
    }
    fn apply_fixture_object(
        &self,
        root: &FreshPrincipalRoot,
        relative_path: &Path,
        expected: Managed_directory_identity,
    ) -> Result<(), IsolationError> {
        if self.fixture.is_none() || current_user()? != self.owner {
            return Err(IsolationError::AuthorityContextRequired);
        }
        self.apply_object(root, relative_path, expected)
    }
}

fn virtual_service_sid(sid: &Sid) -> bool {
    sid.value().strip_prefix("S-1-5-80-").is_some_and(|suffix| {
        let parts: Vec<_> = suffix.split('-').collect();
        parts.len() == 5 && parts.iter().all(|part| part.parse::<u32>().is_ok())
    })
}

fn current_user() -> Result<Sid, IsolationError> {
    let mut thread_token = null_mut();
    if unsafe { OpenThreadToken(GetCurrentThread(), 8, 1, &mut thread_token) } != 0 {
        let _token = own(thread_token, IsolationError::AuthorityContextRequired)?;
        return Err(IsolationError::AuthorityContextRequired);
    }
    if unsafe { GetLastError() } != ERROR_NO_TOKEN {
        return Err(IsolationError::AuthorityContextRequired);
    }
    let mut token = null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), 8, &mut token) } == 0 {
        return Err(IsolationError::AuthorityContextRequired);
    }
    let token = own(token, IsolationError::AuthorityContextRequired)?;
    token_user(token.as_raw_handle())
}

fn open_object(path: &Path, writable_acl: bool) -> Result<File, IsolationError> {
    OpenOptions::new()
        .access_mode(
            READ_DATA_OR_LIST_DIRECTORY
                | READ_CONTROL
                | READ_ATTRIBUTES
                | if writable_acl { WRITE_DAC } else { 0 },
        )
        .share_mode(1)
        .custom_flags(NO_FOLLOW_BACKUP)
        .open(path)
        .map_err(|_| IsolationError::StorageAccessFailed)
}

fn verify_object(
    object: &File,
    expected: Managed_directory_identity,
    directory: bool,
) -> Result<(), IsolationError> {
    let metadata = inventory_object_metadata_from_file(object)
        .map_err(|_| IsolationError::StorageAccessFailed)?;
    let mut info: FileInformation = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(object.as_raw_handle(), &mut info) } == 0 {
        return Err(IsolationError::StorageAccessFailed);
    }
    if metadata.object != expected
        || metadata.attributes & REPARSE_ATTRIBUTE != 0
        || (metadata.attributes & DIRECTORY_ATTRIBUTE != 0) != directory
        || (!directory && info.links != 1)
    {
        return Err(IsolationError::StorageBoundaryInvalid);
    }
    Ok(())
}

fn read_acl(object: &File) -> Result<(Sid, BTreeMap<String, (u32, u8)>), IsolationError> {
    let mut descriptor = security_descriptor(object.as_raw_handle())?;
    let pointer = descriptor.as_mut_ptr().cast();
    let (mut control, mut revision) = (0, 0);
    if unsafe { GetSecurityDescriptorControl(pointer, &mut control, &mut revision) } == 0
        || control & PROTECTED_DACL == 0
    {
        return Err(IsolationError::StorageBoundaryInvalid);
    }
    let owner = descriptor_owner(pointer)?;
    let acl = unsafe { descriptor_acl(pointer)? };
    let mut information: AclSize = unsafe { std::mem::zeroed() };
    if unsafe {
        GetAclInformation(
            acl,
            (&mut information as *mut AclSize).cast(),
            std::mem::size_of::<AclSize>() as u32,
            2,
        )
    } == 0
        || information.count > 8
    {
        return Err(IsolationError::StorageBoundaryInvalid);
    }
    let mut grants = BTreeMap::new();
    for index in 0..information.count {
        let mut raw = null_mut();
        if unsafe { GetAce(acl, index, &mut raw) } == 0 || raw.is_null() {
            return Err(IsolationError::StorageBoundaryInvalid);
        }
        let ace = unsafe { &*raw.cast::<AllowedAce>() };
        if ace.header.kind != 0 || ace.header.size < 12 {
            return Err(IsolationError::StorageBoundaryInvalid);
        }
        let sid = unsafe { sid_from_pointer((&ace.sid_start as *const u32).cast_mut().cast()) }?;
        if grants
            .insert(sid.value().to_owned(), (ace.mask, ace.header.flags))
            .is_some()
        {
            return Err(IsolationError::StorageBoundaryInvalid);
        }
    }
    Ok((owner, grants))
}

#[cfg(test)]
mod tests;
