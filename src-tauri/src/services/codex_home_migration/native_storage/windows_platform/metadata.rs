use super::*;
use crate::{
    config::private_fs,
    services::owned_process::{run_capture, SpawnSpec},
};
use std::{
    ffi::c_void,
    os::windows::{ffi::OsStringExt, io::AsRawHandle},
    ptr,
    time::Duration,
};

const MAX_METADATA_BYTES: usize = 1024 * 1024;
const MAX_STREAMS: usize = 1024;
const HELP_OUTPUT_BYTES: usize = 512 * 1024;
const BACKUP_SECURITY_INFORMATION: u32 = 0x0001_0000;
// Winnt.h components: owner/group/DACL/SACL, integrity label, resource
// attributes, scope, process trust label and access filter. GetSecurityInfo
// requires explicit components alongside BACKUP_SECURITY_INFORMATION.
const COMPLETE_SECURITY_INFORMATION: u32 = BACKUP_SECURITY_INFORMATION | 0x0000_01FF;

#[link(name = "kernel32")]
extern "system" {
    fn GetSystemDirectoryW(buffer: *mut u16, chars: u32) -> u32;
    fn LocalFree(memory: *mut c_void) -> *mut c_void;
    fn GetFileInformationByHandleEx(
        file: *mut c_void,
        class: u32,
        information: *mut c_void,
        bytes: u32,
    ) -> i32;
}
#[link(name = "advapi32")]
extern "system" {
    fn GetSecurityInfo(
        file: *mut c_void,
        object_type: u32,
        information: u32,
        owner: *mut *mut c_void,
        group: *mut *mut c_void,
        dacl: *mut *mut c_void,
        sacl: *mut *mut c_void,
        descriptor: *mut *mut c_void,
    ) -> u32;
    fn IsValidSecurityDescriptor(descriptor: *const c_void) -> i32;
    fn GetSecurityDescriptorLength(descriptor: *const c_void) -> u32;
    fn GetSecurityDescriptorControl(
        descriptor: *const c_void,
        control: *mut u16,
        revision: *mut u32,
    ) -> i32;
}

pub(super) fn system_robocopy() -> Result<PathBuf> {
    let mut buffer = vec![0u16; 32768];
    let count = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
    if count == 0 || count >= buffer.len() {
        return Err(MigrationError::PlatformFailure);
    }
    let directory = PathBuf::from(OsString::from_wide(&buffer[..count]));
    if directory
        .file_name()
        .is_none_or(|name| !name.to_string_lossy().eq_ignore_ascii_case("System32"))
    {
        return Err(MigrationError::PlatformFailure);
    }
    let path = directory.join("robocopy.exe");
    let canonical = super::super::inventory::canonical(&path)?;
    if !super::super::inventory::same(&canonical, &path)
        || !std::fs::symlink_metadata(&path)
            .map_err(|_| MigrationError::PlatformFailure)?
            .is_file()
    {
        return Err(MigrationError::PlatformFailure);
    }
    Ok(path)
}

pub(super) fn qualified_robocopy(cancelled: &AtomicBool, deadline: Instant) -> Result<PathBuf> {
    let path = system_robocopy()?;
    let args = vec!["/?".to_owned()];
    let output = run_capture(
        &SpawnSpec::new(&path, &args),
        cancelled,
        deadline.min(Instant::now() + Duration::from_secs(5)),
        HELP_OUTPUT_BYTES,
    )
    .map_err(|_| MigrationError::PlatformFailure)?;
    // /? returns 16 on the qualified Windows binary because it has no copy
    // operands. This exception is confined to help; copy >=8 remains failure.
    if !matches!(output.exit_code, 0 | 16) {
        return Err(MigrationError::PlatformFailure);
    }
    let ascii: String = output
        .stdout
        .iter()
        .filter(|byte| **byte != 0)
        .map(|byte| char::from(*byte).to_ascii_uppercase())
        .collect();
    if !ascii.contains("ROBOCOPY") || !ascii.contains("/DCOPY:") || !ascii.contains("E=") {
        return Err(MigrationError::PlatformFailure);
    }
    Ok(path)
}

fn u32_at(bytes: &[u8], offset: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        bytes
            .get(offset..offset + 4)
            .ok_or(MigrationError::PlatformFailure)?
            .try_into()
            .map_err(|_| MigrationError::PlatformFailure)?,
    ))
}
fn i64_at(bytes: &[u8], offset: usize) -> Result<i64> {
    Ok(i64::from_le_bytes(
        bytes
            .get(offset..offset + 8)
            .ok_or(MigrationError::PlatformFailure)?
            .try_into()
            .map_err(|_| MigrationError::PlatformFailure)?,
    ))
}

pub(super) fn parse_streams(bytes: &[u8]) -> Result<Vec<NamedStream>> {
    let mut result = Vec::new();
    let mut offset: usize = 0;
    let mut count = 0;
    loop {
        count += 1;
        if count > MAX_STREAMS + 1 {
            return Err(MigrationError::PlatformFailure);
        }
        let record = bytes.get(offset..).ok_or(MigrationError::PlatformFailure)?;
        let next = u32_at(record, 0)? as usize;
        let name_bytes = u32_at(record, 4)? as usize;
        let size = i64_at(record, 8)?;
        if name_bytes == 0
            || name_bytes % 2 != 0
            || name_bytes > 65534
            || size < 0
            || i64_at(record, 16)? < 0
        {
            return Err(MigrationError::PlatformFailure);
        }
        let end = 24usize
            .checked_add(name_bytes)
            .ok_or(MigrationError::PlatformFailure)?;
        let name: Vec<u16> = record
            .get(24..end)
            .ok_or(MigrationError::PlatformFailure)?
            .chunks_exact(2)
            .map(|part| u16::from_le_bytes([part[0], part[1]]))
            .collect();
        let name = String::from_utf16(&name).map_err(|_| MigrationError::PlatformFailure)?;
        if name != "::$DATA" {
            if !name.starts_with(':')
                || !name.ends_with(":$DATA")
                || name.len() <= 7
                || name[1..name.len() - 6]
                    .chars()
                    .any(|value| value.is_control() || matches!(value, ':' | '/' | '\\'))
            {
                return Err(MigrationError::PlatformFailure);
            }
            result.push(NamedStream {
                name,
                bytes: size as u64,
            });
        }
        if next == 0 {
            break;
        }
        if next % 8 != 0 || next < end {
            return Err(MigrationError::PlatformFailure);
        }
        offset = offset
            .checked_add(next)
            .ok_or(MigrationError::PlatformFailure)?;
    }
    result.sort_by(|left, right| left.name.cmp(&right.name));
    if result.windows(2).any(|pair| pair[0].name == pair[1].name) {
        return Err(MigrationError::PlatformFailure);
    }
    Ok(result)
}

fn streams(
    platform: &WindowsStoragePlatform<'_>,
    file: &File,
    directory: bool,
) -> Result<Vec<NamedStream>> {
    let mut length = 4096;
    loop {
        platform.check_authority()?;
        let mut words = vec![0u64; length / 8];
        if unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                7,
                words.as_mut_ptr().cast(),
                length as u32,
            )
        } != 0
        {
            let bytes = unsafe { std::slice::from_raw_parts(words.as_ptr().cast::<u8>(), length) };
            return parse_streams(bytes);
        }
        let status = std::io::Error::last_os_error().raw_os_error();
        match status {
            Some(38) if directory => return Ok(Vec::new()),
            Some(122 | 234) if length < MAX_METADATA_BYTES => length *= 2,
            _ => return Err(MigrationError::PlatformFailure),
        }
    }
}

struct SecurityBuffer(*mut c_void);
impl Drop for SecurityBuffer {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                LocalFree(self.0);
            }
        }
    }
}

fn security(file: &File) -> Result<Vec<u8>> {
    let mut raw = ptr::null_mut();
    // Request every SDK component explicitly, retaining backup semantics and
    // label/resource/audit SACL entries rather than only traditional ACLs.
    let status = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            1,
            COMPLETE_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut raw,
        )
    };
    let buffer = SecurityBuffer(raw);
    if status != 0 || raw.is_null() || unsafe { IsValidSecurityDescriptor(raw) } == 0 {
        return Err(MigrationError::PlatformFailure);
    }
    let mut control = 0u16;
    let mut revision = 0u32;
    if unsafe { GetSecurityDescriptorControl(raw, &mut control, &mut revision) } == 0
        || control & 0x8000 == 0
    {
        return Err(MigrationError::PlatformFailure);
    }
    let length = unsafe { GetSecurityDescriptorLength(raw) } as usize;
    if !(20..=MAX_METADATA_BYTES).contains(&length) {
        return Err(MigrationError::PlatformFailure);
    }
    // SAFETY: the validated self-relative descriptor owns this exact contiguous
    // LocalAlloc buffer until SecurityBuffer drops after the private copy.
    let result = unsafe { std::slice::from_raw_parts(buffer.0.cast::<u8>(), length) }.to_vec();
    Ok(result)
}

pub(super) fn capture_file(
    platform: &WindowsStoragePlatform<'_>,
    file: &File,
) -> Result<EntryMetadata> {
    platform.check_authority()?;
    let before = private_fs::inventory_object_metadata_from_file(file)
        .map_err(|_| MigrationError::PlatformFailure)?;
    #[cfg(test)]
    capture_stage("before_security", &before, file)?;
    let security = security(file)?;
    #[cfg(test)]
    capture_stage("after_security", &before, file)?;
    let extended_attributes = extended_attributes::capture(platform, file)?;
    #[cfg(test)]
    capture_stage("after_ea", &before, file)?;
    let streams = streams(platform, file, before.attributes & 0x10 != 0)?;
    #[cfg(test)]
    capture_stage("after_streams", &before, file)?;
    let after = private_fs::inventory_object_metadata_from_file(file)
        .map_err(|_| MigrationError::PlatformFailure)?;
    if before != after {
        #[cfg(test)]
        eprintln!("inventory_internal stage=same_handle_capture directory={} reparse={} object={} attrs={} created={} accessed={} written={} size={}", before.attributes & 0x10 != 0, before.attributes & 0x400 != 0, before.object != after.object, before.attributes != after.attributes, before.created != after.created, before.accessed != after.accessed, before.written != after.written, before.unnamed_bytes != after.unnamed_bytes);
        return Err(MigrationError::SourceChanged);
    }
    Ok(EntryMetadata {
        object: before.object,
        attributes: before.attributes,
        created: before.created,
        accessed: before.accessed,
        written: before.written,
        unnamed_bytes: before.unnamed_bytes,
        security,
        extended_attributes,
        streams,
    })
}

#[cfg(test)]
fn capture_stage(
    stage: &str,
    before: &private_fs::Inventory_object_metadata,
    file: &File,
) -> Result<()> {
    let after = private_fs::inventory_object_metadata_from_file(file)
        .map_err(|_| MigrationError::PlatformFailure)?;
    eprintln!("native_metadata_subread stage={stage} directory={} reparse={} object={} attrs={} created={} accessed={} written={} size={}", before.attributes & 0x10 != 0, before.attributes & 0x400 != 0, before.object != after.object, before.attributes != after.attributes, before.created != after.created, before.accessed != after.accessed, before.written != after.written, before.unnamed_bytes != after.unnamed_bytes);
    if before != &after {
        return Err(MigrationError::SourceChanged);
    }
    Ok(())
}

pub(super) fn capture(platform: &WindowsStoragePlatform<'_>, path: &Path) -> Result<EntryMetadata> {
    let file = private_fs::open_inventory_security_object(path)
        .map_err(|_| MigrationError::PlatformFailure)?;
    let result = capture_file(platform, &file)?;
    if result.attributes & 0x400 != 0 {
        junction::validate_alias_path(platform, path)?;
        junction::target_from_file(&file)?;
    }
    Ok(result)
}

pub(super) struct TimeLease<'a, 'b> {
    platform: &'a WindowsStoragePlatform<'b>,
    file: File,
    object: DirectoryIdentity,
    link: Option<PathBuf>,
}
impl<'a, 'b> TimeLease<'a, 'b> {
    pub(super) fn new(platform: &'a WindowsStoragePlatform<'b>, path: &Path) -> Result<Self> {
        let file = private_fs::open_inventory_security_object(path)
            .map_err(|_| MigrationError::PlatformFailure)?;
        let metadata = capture_file(platform, &file)?;
        let link = if metadata.attributes & 0x400 != 0 {
            junction::validate_alias_path(platform, path)?;
            Some(junction::target_from_file(&file)?)
        } else {
            None
        };
        Ok(Self {
            platform,
            file,
            object: metadata.object,
            link,
        })
    }
}
impl AccessTimeLease for TimeLease<'_, '_> {
    fn metadata(&self) -> Result<EntryMetadata> {
        let metadata = capture_file(self.platform, &self.file)?;
        if metadata.object != self.object {
            return Err(MigrationError::ObjectConflict);
        }
        Ok(metadata)
    }
    fn link_target(&self) -> Result<Option<PathBuf>> {
        if self.link.is_some() {
            Ok(Some(junction::target_from_file(&self.file)?))
        } else {
            Ok(None)
        }
    }
    fn restore_accessed(&mut self, expected: &EntryMetadata) -> Result<()> {
        let current = self.metadata()?;
        let mut normalized = current.clone();
        normalized.accessed = expected.accessed;
        if normalized != *expected || self.link_target()? != self.link {
            #[cfg(test)]
            eprintln!("atime_lease_before_mismatch object={} attrs={} created={} accessed={} written={} size={} security={} ea={} streams={} link={}", current.object != expected.object, current.attributes != expected.attributes, current.created != expected.created, current.accessed != expected.accessed, current.written != expected.written, current.unnamed_bytes != expected.unnamed_bytes, current.security != expected.security, current.extended_attributes != expected.extended_attributes, current.streams != expected.streams, self.link_target()? != self.link);
            return Err(MigrationError::SourceChanged);
        }
        // A held handle can still report the recorded value while another
        // handle has a pending NTFS access-time update. Reassert the admitted
        // value even when this handle currently compares equal, then suppress
        // further updates on this same protected handle.
        file_times::restore_accessed(&self.file, expected.accessed)?;
        let after = self.metadata()?;
        if after != *expected {
            #[cfg(test)]
            eprintln!("atime_lease_after_mismatch object={} attrs={} created={} accessed={} written={} size={} security={} ea={} streams={}", after.object != expected.object, after.attributes != expected.attributes, after.created != expected.created, after.accessed != expected.accessed, after.written != expected.written, after.unnamed_bytes != expected.unnamed_bytes, after.security != expected.security, after.extended_attributes != expected.extended_attributes, after.streams != expected.streams);
            return Err(MigrationError::SourceChanged);
        }
        Ok(())
    }
}
