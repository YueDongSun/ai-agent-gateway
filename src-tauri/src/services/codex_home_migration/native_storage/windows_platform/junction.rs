//! In-place mutation restricted to an already admitted Chrome junction object.

use super::*;
use crate::config::private_fs;
use std::{
    ffi::c_void,
    os::windows::{ffi::OsStrExt, io::AsRawHandle},
    ptr,
};

const CHROME_ALIAS: &str = "plugins/cache/openai-bundled/chrome/latest";
const CHROME_PACKAGE: &str = "plugins/cache/openai-bundled/chrome";
const MOUNT_POINT_TAG: u32 = 0xA000_0003;
const MAX_REPARSE_BYTES: usize = 16 * 1024;

#[link(name = "kernel32")]
extern "system" {
    fn DeviceIoControl(
        file: *mut c_void,
        code: u32,
        input: *const c_void,
        input_bytes: u32,
        output: *mut c_void,
        output_bytes: u32,
        returned: *mut u32,
        overlapped: *mut c_void,
    ) -> i32;
}

fn same(left: &Path, right: &Path) -> bool {
    super::super::inventory::same(left, right)
}
fn u16_at(bytes: &[u8], offset: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(
        bytes
            .get(offset..offset + 2)
            .ok_or(MigrationError::ForbiddenLink)?
            .try_into()
            .map_err(|_| MigrationError::ForbiddenLink)?,
    ))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct MountPoint {
    pub target: PathBuf,
    print_name: String,
}

fn dos_path(value: &str) -> Result<PathBuf> {
    let text = value
        .strip_prefix(r"\??\")
        .or_else(|| value.strip_prefix(r"\\?\"))
        .unwrap_or(value);
    // The NT substitute name needs native separators; Win32's accepted '/'
    // spelling is not copied verbatim into a reparse payload.
    let path: PathBuf = Path::new(text).components().collect();
    if !super::super::inventory::plain(&path)
        || !matches!(path.components().next(), Some(std::path::Component::Prefix(prefix)) if matches!(prefix.kind(), std::path::Prefix::Disk(_)))
    {
        return Err(MigrationError::ForbiddenLink);
    }
    Ok(path)
}

pub(super) fn parse(bytes: &[u8]) -> Result<MountPoint> {
    if bytes.len() < 16
        || bytes.len() > MAX_REPARSE_BYTES
        || u32::from_le_bytes(
            bytes[0..4]
                .try_into()
                .map_err(|_| MigrationError::ForbiddenLink)?,
        ) != MOUNT_POINT_TAG
        || usize::from(u16_at(bytes, 4)?) + 8 != bytes.len()
    {
        return Err(MigrationError::ForbiddenLink);
    }
    let field = |offset_field: usize, length_field: usize| -> Result<String> {
        let offset = usize::from(u16_at(bytes, offset_field)?);
        let length = usize::from(u16_at(bytes, length_field)?);
        if offset % 2 != 0 || length % 2 != 0 {
            return Err(MigrationError::ForbiddenLink);
        }
        let start = 16usize
            .checked_add(offset)
            .ok_or(MigrationError::ForbiddenLink)?;
        let end = start
            .checked_add(length)
            .ok_or(MigrationError::ForbiddenLink)?;
        let value = bytes.get(start..end).ok_or(MigrationError::ForbiddenLink)?;
        let units: Vec<_> = value
            .chunks_exact(2)
            .map(|part| u16::from_le_bytes([part[0], part[1]]))
            .collect();
        let text = String::from_utf16(&units).map_err(|_| MigrationError::ForbiddenLink)?;
        if text.chars().any(char::is_control) {
            return Err(MigrationError::ForbiddenLink);
        }
        Ok(text)
    };
    let substitute = field(8, 10)?;
    if !substitute.starts_with(r"\??\") {
        return Err(MigrationError::ForbiddenLink);
    }
    Ok(MountPoint {
        target: dos_path(&substitute)?,
        print_name: field(12, 14)?,
    })
}

pub(super) fn buffer(target: &Path, print_name: &str) -> Result<Vec<u8>> {
    let dos = dos_path(target.to_str().ok_or(MigrationError::InvalidPaths)?)?;
    let mut substitute: Vec<u16> = r"\??\"
        .encode_utf16()
        .chain(dos.as_os_str().encode_wide())
        .collect();
    let print: Vec<u16> = print_name.encode_utf16().collect();
    if substitute.contains(&0) || print.contains(&0) {
        return Err(MigrationError::InvalidPaths);
    }
    let substitute_bytes = substitute
        .len()
        .checked_mul(2)
        .ok_or(MigrationError::InvalidPaths)?;
    let print_bytes = print
        .len()
        .checked_mul(2)
        .ok_or(MigrationError::InvalidPaths)?;
    let payload = 8usize
        .checked_add(substitute_bytes + 2 + print_bytes + 2)
        .ok_or(MigrationError::InvalidPaths)?;
    if payload + 8 > MAX_REPARSE_BYTES {
        return Err(MigrationError::InvalidPaths);
    }
    let mut bytes = Vec::with_capacity(payload + 8);
    bytes.extend_from_slice(&MOUNT_POINT_TAG.to_le_bytes());
    bytes.extend_from_slice(&(payload as u16).to_le_bytes());
    bytes.extend_from_slice(&0u16.to_le_bytes());
    bytes.extend_from_slice(&0u16.to_le_bytes());
    bytes.extend_from_slice(&(substitute_bytes as u16).to_le_bytes());
    bytes.extend_from_slice(&((substitute_bytes + 2) as u16).to_le_bytes());
    bytes.extend_from_slice(&(print_bytes as u16).to_le_bytes());
    substitute.push(0);
    for unit in substitute {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    for unit in print.into_iter().chain(Some(0)) {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    Ok(bytes)
}

fn raw(file: &File) -> Result<Vec<u8>> {
    let mut words = vec![0u64; MAX_REPARSE_BYTES / 8];
    let mut returned = 0u32;
    // This is an admitted user filesystem directory handle, never a volume,
    // device namespace, storage controller, or arbitrary control-code surface.
    if unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            0x0009_00A8,
            ptr::null(),
            0,
            words.as_mut_ptr().cast(),
            MAX_REPARSE_BYTES as u32,
            &mut returned,
            ptr::null_mut(),
        )
    } == 0
        || returned as usize > MAX_REPARSE_BYTES
    {
        return Err(MigrationError::ForbiddenLink);
    }
    let bytes =
        unsafe { std::slice::from_raw_parts(words.as_ptr().cast::<u8>(), returned as usize) }
            .to_vec();
    parse(&bytes)?;
    Ok(bytes)
}

pub(super) fn target_from_file(file: &File) -> Result<PathBuf> {
    Ok(parse(&raw(file)?)?.target)
}

fn set_raw(file: &File, bytes: &[u8]) -> Result<()> {
    parse(bytes)?;
    let mut returned = 0;
    if unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            0x0009_00A4,
            bytes.as_ptr().cast(),
            bytes.len() as u32,
            ptr::null_mut(),
            0,
            &mut returned,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(MigrationError::PlatformFailure);
    }
    Ok(())
}

pub(super) fn validate_alias_path(
    platform: &WindowsStoragePlatform<'_>,
    path: &Path,
) -> Result<()> {
    if ![
        &platform.paths.source,
        &platform.paths.destination,
        &platform.paths.source_backup,
        &platform.paths.destination_archive,
    ]
    .iter()
    .any(|root| same(path, &root.join(CHROME_ALIAS)))
    {
        return Err(MigrationError::ForbiddenLink);
    }
    Ok(())
}

pub(super) fn retarget(
    platform: &WindowsStoragePlatform<'_>,
    root: &Path,
    expected: DirectoryIdentity,
    alias: &ChromeLatest,
    target: &Path,
    alias_identity: DirectoryIdentity,
) -> Result<()> {
    platform.check()?;
    if !same(&alias.relative_path, Path::new(CHROME_ALIAS))
        || alias
            .target_relative
            .parent()
            .is_none_or(|parent| !same(parent, Path::new(CHROME_PACKAGE)))
        || alias
            .target_relative
            .file_name()
            .is_none_or(|name| name.to_string_lossy().eq_ignore_ascii_case("latest"))
        || ![&platform.paths.source, &platform.paths.destination]
            .iter()
            .any(|allowed| same(root, allowed))
        || !same(target, &root.join(&alias.target_relative))
        || !same(
            &alias.raw_target,
            &platform.paths.source.join(&alias.target_relative),
        )
        || private_fs::inspect_directory_object(root).map_err(|_| MigrationError::ObjectConflict)?
            != expected
        || private_fs::inspect_directory_object(target)
            .map_err(|_| MigrationError::ObjectConflict)?
            != alias.target_identity
    {
        return Err(MigrationError::ForbiddenLink);
    }
    let path = root.join(&alias.relative_path);
    validate_alias_path(platform, &path)?;
    let file = private_fs::open_inventory_reparse_mutation_object(&path)
        .map_err(|_| MigrationError::ObjectConflict)?;
    let original = metadata::capture_file(platform, &file)?;
    if original.object != alias_identity || original.attributes & 0x410 != 0x410 {
        return Err(MigrationError::ObjectConflict);
    }
    let original_raw = raw(&file)?;
    let original_link = parse(&original_raw)?;
    let old = platform.paths.source.join(&alias.target_relative);
    let new = platform.paths.destination.join(&alias.target_relative);
    if !same(&original_link.target, &old) && !same(&original_link.target, &new) {
        return Err(MigrationError::ForbiddenLink);
    }
    if same(&original_link.target, target) {
        return Ok(());
    }
    let target_text = dos_path(target.to_str().ok_or(MigrationError::InvalidPaths)?)?
        .to_string_lossy()
        .into_owned();
    let print_name = if dos_path(&original_link.print_name)
        .is_ok_and(|print| same(&print, &old) || same(&print, &new))
    {
        target_text
    } else {
        original_link.print_name
    };
    let updated = buffer(target, &print_name)?;
    let mut attempted = false;
    let result = (|| {
        platform.check()?;
        attempted = true;
        set_raw(&file, &updated)?;
        file_times::restore_all(&file, &original)?;
        if !same(&target_from_file(&file)?, target)
            || metadata::capture_file(platform, &file)? != original
            || private_fs::inspect_directory_object(root)
                .map_err(|_| MigrationError::ObjectConflict)?
                != expected
        {
            return Err(MigrationError::SourceChanged);
        }
        Ok(())
    })();
    if let Err(error) = result {
        if !attempted {
            return Err(error);
        }
        // Reconcile on the same admitted link object. Retain the original error
        // and report rollback failure without ever deleting/recreating a link.
        let rollback = match raw(&file) {
            Ok(current) if current == original_raw => file_times::restore_all(&file, &original),
            Ok(current) if current == updated => set_raw(&file, &original_raw)
                .and_then(|_| file_times::restore_all(&file, &original)),
            _ => Err(MigrationError::SourceChanged),
        };
        if rollback.is_err() {
            log::error!("migration_junction_restore_failed");
        }
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn create_fixture(path: &Path, target: &Path) -> Result<DirectoryIdentity> {
    let file = private_fs::open_inventory_fixture_empty_directory_for_reparse(path)
        .map_err(|_| MigrationError::PlatformFailure)?;
    let identity = private_fs::inventory_object_metadata_from_file(&file)
        .map_err(|_| MigrationError::PlatformFailure)?
        .object;
    set_raw(
        &file,
        &buffer(target, target.to_str().ok_or(MigrationError::InvalidPaths)?)?,
    )?;
    if private_fs::inventory_object_metadata_from_file(&file)
        .map_err(|_| MigrationError::PlatformFailure)?
        .object
        != identity
    {
        return Err(MigrationError::ObjectConflict);
    }
    Ok(identity)
}
