use super::super::{IsolationError, RoleStorageAcl};
use super::{
    ffi::*,
    sid::{
        descriptor_acl, descriptor_owner, security_descriptor, set_file_acl, sid_from_pointer,
        Descriptor,
    },
    token::{own, Authority},
};
use std::{
    ffi::c_void,
    fs,
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, OwnedHandle},
    },
    path::{Component, Path, PathBuf},
    ptr::{null, null_mut},
};

const MAX_TREE_OBJECTS: usize = 100_000;
const MAX_TREE_DEPTH: usize = 128;
const REPARSE_ATTRIBUTE: u32 = 0x400;
const ANCESTOR_MUTATION_MASK: u32 = 0x500d_0156;

struct Object {
    handle: OwnedHandle,
    directory: bool,
}

fn open_object(path: &Path, writable_acl: bool) -> Result<Object, IsolationError> {
    let text: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    if text[..text.len() - 1].contains(&0) {
        return Err(IsolationError::StorageBoundaryInvalid);
    }
    // OPEN_REPARSE_POINT inspects the object itself. Omitting SHARE_DELETE
    // keeps every admitted path component pinned throughout ACL installation.
    let handle = own(
        unsafe {
            CreateFileW(
                text.as_ptr(),
                0x0002_0080 | if writable_acl { 0x0004_0000 } else { 0 },
                3,
                null(),
                3,
                0x0220_0000,
                null_mut(),
            )
        },
        IsolationError::StorageAccessFailed,
    )?;
    let mut info: FileInformation = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(handle.as_raw_handle(), &mut info) } == 0
        || info.attributes & REPARSE_ATTRIBUTE != 0
    {
        return Err(IsolationError::StorageBoundaryInvalid);
    }
    let directory = info.attributes & 0x10 != 0;
    if !directory && info.links != 1 {
        return Err(IsolationError::StorageBoundaryInvalid);
    }
    Ok(Object { handle, directory })
}

fn safe_path(path: &Path) -> Result<(), IsolationError> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err(IsolationError::StorageBoundaryInvalid);
    }
    let text = path.to_string_lossy();
    if text.starts_with(r"\\") && !text.starts_with(r"\\?\") {
        return Err(IsolationError::StorageBoundaryInvalid);
    }
    if text.starts_with(r"\\?\UNC\") || text.starts_with(r"\\.\") {
        return Err(IsolationError::StorageBoundaryInvalid);
    }
    Ok(())
}

fn check_ancestor(object: &Object, acl: &RoleStorageAcl) -> Result<(), IsolationError> {
    let mut bytes = security_descriptor(object.handle.as_raw_handle())?;
    let descriptor = bytes.as_mut_ptr().cast::<c_void>();
    let owner = descriptor_owner(descriptor)?;
    if owner == acl.runtime && !acl.fixture_same_owner {
        return Err(IsolationError::AncestorWriteGrant);
    }
    let dacl = unsafe { descriptor_acl(descriptor) }?;
    let mut info = AclSize {
        count: 0,
        in_use: 0,
        free: 0,
    };
    if unsafe {
        GetAclInformation(
            dacl,
            (&mut info as *mut AclSize).cast(),
            std::mem::size_of::<AclSize>() as u32,
            2,
        )
    } == 0
        || info.count > 4096
    {
        return Err(IsolationError::StorageBoundaryInvalid);
    }
    let mut owner_rights_guard = false;
    for index in 0..info.count {
        let mut ace = null_mut();
        if unsafe { GetAce(dacl, index, &mut ace) } == 0 || ace.is_null() {
            return Err(IsolationError::StorageBoundaryInvalid);
        }
        let header = unsafe { &*ace.cast::<AceHeader>() };
        if header.flags & 0x8 != 0 {
            continue;
        }
        if header.kind == 1 {
            continue;
        } // Denies do not grant mutation.
        if header.kind != 0 || (header.size as usize) < std::mem::size_of::<AllowedAce>() {
            return Err(IsolationError::StorageBoundaryInvalid);
        }
        let allowed = unsafe { &*ace.cast::<AllowedAce>() };
        let sid =
            unsafe { sid_from_pointer(std::ptr::addr_of!(allowed.sid_start).cast_mut().cast()) }?;
        if sid.value() == "S-1-3-4" {
            if allowed.mask & 0x100c_0000 != 0 {
                return Err(IsolationError::AncestorWriteGrant);
            }
            owner_rights_guard = true;
        }
        if allowed.mask & ANCESTOR_MUTATION_MASK == 0 {
            continue;
        }
        if sid != acl.owner && sid.value() != "S-1-5-18" && sid.value() != "S-1-5-32-544" {
            return Err(IsolationError::AncestorWriteGrant);
        }
        if sid == acl.write_cap {
            return Err(IsolationError::AncestorWriteGrant);
        }
    }
    if acl.fixture_same_owner && owner == acl.owner && !owner_rights_guard {
        return Err(IsolationError::AncestorWriteGrant);
    }
    Ok(())
}

pub(in super::super) fn apply_tree(
    acl: &RoleStorageAcl,
    authority: &Authority,
    root: &Path,
) -> Result<usize, IsolationError> {
    if !authority.is_protected() || authority.owner_sid() != &acl.owner {
        return Err(IsolationError::AuthorityContextRequired);
    }
    if acl.fixture_same_owner != authority.fixture_root().is_some()
        || (!acl.fixture_same_owner && acl.owner == acl.runtime)
    {
        return Err(IsolationError::StorageBoundaryInvalid);
    }
    safe_path(root)?;
    let canonical = fs::canonicalize(root).map_err(|_| IsolationError::StorageBoundaryInvalid)?;
    if let Some(boundary) = authority.fixture_root() {
        if canonical == boundary || !canonical.starts_with(boundary) {
            return Err(IsolationError::StorageBoundaryInvalid);
        }
    }
    // Pin and inspect every ancestor. Same-user tests stop at their exclusive
    // fixture boundary; that exception is unavailable in a deployed build.
    let mut ancestors = Vec::new();
    for path in root.ancestors().skip(1) {
        let object = open_object(path, false)?;
        if !object.directory {
            return Err(IsolationError::StorageBoundaryInvalid);
        }
        check_ancestor(&object, acl)?;
        ancestors.push(object);
        if authority.fixture_root().is_some_and(|boundary| {
            fs::canonicalize(path).is_ok_and(|candidate| candidate == boundary)
        }) {
            break;
        }
    }
    let mut objects = Vec::new();
    let mut pending = vec![(root.to_path_buf(), 0usize)];
    while let Some((path, depth)) = pending.pop() {
        if objects.len() >= MAX_TREE_OBJECTS || depth > MAX_TREE_DEPTH {
            return Err(IsolationError::StorageBoundaryInvalid);
        }
        let object = open_object(&path, true)?;
        let mut descriptor = security_descriptor(object.handle.as_raw_handle())?;
        if descriptor_owner(descriptor.as_mut_ptr().cast())? != acl.owner {
            // No adoption of B-owned objects. The caller seals into fresh A
            // objects before this operation; old objects remain quarantined.
            return Err(IsolationError::StorageBoundaryInvalid);
        }
        if object.directory {
            for entry in fs::read_dir(&path).map_err(|_| IsolationError::StorageAccessFailed)? {
                let child = entry
                    .map_err(|_| IsolationError::StorageAccessFailed)?
                    .path();
                if !child.starts_with(root) {
                    return Err(IsolationError::StorageBoundaryInvalid);
                }
                pending.push((child, depth + 1));
            }
        }
        objects.push(object);
    }
    let descriptor = Descriptor::new(acl.sddl())?;
    for object in &objects {
        set_file_acl(object.handle.as_raw_handle(), &descriptor)?;
    }
    Ok(objects.len())
}

#[cfg(test)]
pub(in super::super) fn initialize_fixture_container(
    authority: &Authority,
    root: &Path,
) -> Result<(), IsolationError> {
    if !authority.is_protected()
        || authority.fixture_root()
            != Some(
                fs::canonicalize(root)
                    .map_err(|_| IsolationError::StorageBoundaryInvalid)?
                    .as_path(),
            )
    {
        return Err(IsolationError::AuthorityContextRequired);
    }
    let object = open_object(root, true)?;
    let mut bytes = security_descriptor(object.handle.as_raw_handle())?;
    if !object.directory || descriptor_owner(bytes.as_mut_ptr().cast())? != *authority.owner_sid() {
        return Err(IsolationError::StorageBoundaryInvalid);
    }
    let descriptor = Descriptor::new(&format!(
        "O:{}D:P(A;OICI;FA;;;{})(A;OICI;RC;;;OW)",
        authority.owner_sid().value(),
        authority.owner_sid().value()
    ))?;
    set_file_acl(object.handle.as_raw_handle(), &descriptor)
}

#[cfg(test)]
pub(in super::super) fn fixture_descriptor_attempt(path: &Path) -> bool {
    let Ok(object) = open_object(path, true) else {
        return false;
    };
    let Ok(descriptor) = Descriptor::new("D:P(A;;FA;;;WD)") else {
        return false;
    };
    set_file_acl(object.handle.as_raw_handle(), &descriptor).is_ok()
}
