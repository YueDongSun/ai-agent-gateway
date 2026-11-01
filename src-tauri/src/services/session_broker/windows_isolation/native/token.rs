use super::super::{CoreRole, IsolationError, Sid, SourceCapabilities};
use super::{
    ffi::*,
    sid::{sid_from_pointer, BinarySid, Descriptor},
};
use crate::services::owned_process::OwnedToken;
use std::{
    ffi::c_void,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    path::{Path, PathBuf},
    ptr::null_mut,
};

const MAX_TOKEN_BYTES: u32 = 1024 * 1024;
const TOKEN_ALL_ACCESS: u32 = 0x000f_01ff;
const WRITE_RESTRICTED: u32 = 0x8;
const GROUP_ENABLED: u32 = 0x4;
const GROUP_DENY_ONLY: u32 = 0x10;
const GROUP_LOGON_ID: u32 = 0xc000_0000;

#[cfg(test)]
#[derive(Clone, Copy, Debug)]
pub(in super::super) enum FixtureSystemProfile {
    SourceOnly,
    WriteRestrictedCode,
    ServiceRuntime,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in super::super) enum FixtureObjectProfile {
    Protected,
    ChildQuery,
    DefaultObjects,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in super::super) enum AuthorityGroupMode {
    DenyOnly,
    #[cfg(test)]
    KeepEnabled,
}

fn token_creation_error(_stage: &'static str) -> IsolationError {
    #[cfg(test)]
    println!(
        "AAG_ISOLATION_TOKEN_CALL_FAILED stage={} win32={}",
        _stage,
        unsafe { GetLastError() }
    );
    IsolationError::TokenCreationFailed
}

pub(in super::super) fn own(
    handle: Handle,
    error: IsolationError,
) -> Result<OwnedHandle, IsolationError> {
    if handle.is_null() || handle as isize == -1 {
        return Err(error);
    }
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

pub(in super::super) fn token_information(
    token: Handle,
    class: u32,
) -> Result<Vec<usize>, IsolationError> {
    let mut needed = 0;
    unsafe {
        GetTokenInformation(token, class, null_mut(), 0, &mut needed);
    }
    if needed == 0 || needed > MAX_TOKEN_BYTES {
        return Err(IsolationError::TokenVerificationFailed);
    }
    let mut buffer = vec![0usize; (needed as usize).div_ceil(std::mem::size_of::<usize>())];
    if unsafe {
        GetTokenInformation(
            token,
            class,
            buffer.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    } == 0
    {
        return Err(IsolationError::TokenVerificationFailed);
    }
    Ok(buffer)
}

pub(in super::super) fn token_user(token: Handle) -> Result<Sid, IsolationError> {
    let buffer = token_information(token, 1)?;
    let user = unsafe { &*buffer.as_ptr().cast::<SidAndAttributes>() };
    unsafe { sid_from_pointer(user.sid) }
}

pub(in super::super) fn token_groups(
    token: Handle,
    class: u32,
) -> Result<Vec<(Sid, u32)>, IsolationError> {
    let buffer = token_information(token, class)?;
    let offset = std::mem::offset_of!(TokenGroups, groups);
    let count = unsafe { *buffer.as_ptr().cast::<u32>() } as usize;
    if offset
        .checked_add(
            count
                .checked_mul(std::mem::size_of::<SidAndAttributes>())
                .ok_or(IsolationError::TokenVerificationFailed)?,
        )
        .is_none_or(|length| length > buffer.len() * std::mem::size_of::<usize>())
    {
        return Err(IsolationError::TokenVerificationFailed);
    }
    let entries = unsafe {
        std::slice::from_raw_parts(
            buffer
                .as_ptr()
                .cast::<u8>()
                .add(offset)
                .cast::<SidAndAttributes>(),
            count,
        )
    };
    entries
        .iter()
        .map(|entry| Ok((unsafe { sid_from_pointer(entry.sid) }?, entry.attributes)))
        .collect()
}

pub(in super::super) fn token_privileges(
    token: Handle,
) -> Result<Vec<LuidAndAttributes>, IsolationError> {
    let buffer = token_information(token, 3)?;
    let offset = std::mem::offset_of!(TokenPrivileges, privileges);
    let count = unsafe { *buffer.as_ptr().cast::<u32>() } as usize;
    if offset
        .checked_add(
            count
                .checked_mul(std::mem::size_of::<LuidAndAttributes>())
                .ok_or(IsolationError::TokenVerificationFailed)?,
        )
        .is_none_or(|length| length > buffer.len() * std::mem::size_of::<usize>())
    {
        return Err(IsolationError::TokenVerificationFailed);
    }
    Ok(unsafe {
        std::slice::from_raw_parts(
            buffer
                .as_ptr()
                .cast::<u8>()
                .add(offset)
                .cast::<LuidAndAttributes>(),
            count,
        )
    }
    .to_vec())
}

fn validate_original_primary(token: Handle) -> Result<(), IsolationError> {
    if unsafe { IsTokenRestricted(token) } != 0 {
        return Err(IsolationError::AuthorityContextRequired);
    }
    // IsTokenRestricted checks restricting SIDs only. A token filtered solely
    // through groups/privileges is also not an original authority.
    let has_restrictions = token_information(token, 21)?;
    let kind = token_information(token, 8)?;
    if unsafe { *has_restrictions.as_ptr().cast::<u32>() } != 0
        || unsafe { *kind.as_ptr().cast::<u32>() } != 1
    {
        return Err(IsolationError::AuthorityContextRequired);
    }
    Ok(())
}

pub(in super::super) struct Authority {
    process: OwnedHandle,
    token: OwnedHandle,
    owner: Sid,
    group: Sid,
    process_id: u32,
    protected: bool,
    fixture_root: Option<PathBuf>,
}
impl Authority {
    pub fn capture_service(expected: &Sid) -> Result<Self, IsolationError> {
        if !expected.value().starts_with("S-1-5-80-") {
            return Err(IsolationError::AuthorityContextRequired);
        }
        let authority = Self::capture(None)?;
        let session = token_information(authority.token.as_raw_handle(), 12)?;
        if authority.owner != *expected || unsafe { *session.as_ptr().cast::<u32>() } != 0 {
            return Err(IsolationError::AuthorityContextRequired);
        }
        Ok(authority)
    }

    fn capture(fixture_root: Option<PathBuf>) -> Result<Self, IsolationError> {
        let process_id = unsafe { GetCurrentProcessId() };
        let process = own(
            unsafe { OpenProcess(0x001f_ffff, 0, process_id) },
            IsolationError::AuthorityContextRequired,
        )?;
        let mut token = null_mut();
        if unsafe { OpenProcessToken(process.as_raw_handle(), TOKEN_ALL_ACCESS, &mut token) } == 0 {
            return Err(IsolationError::AuthorityContextRequired);
        }
        let token = own(token, IsolationError::AuthorityContextRequired)?;
        validate_original_primary(token.as_raw_handle())?;
        let owner = token_user(token.as_raw_handle())?;
        let groups = token_groups(token.as_raw_handle(), 2)?;
        let mut logon = groups.into_iter().filter(|(sid, attrs)| {
            sid.value().starts_with("S-1-5-5-")
                && attrs & GROUP_LOGON_ID == GROUP_LOGON_ID
                && attrs & GROUP_ENABLED != 0
                && attrs & GROUP_DENY_ONLY == 0
        });
        let group = logon
            .next()
            .ok_or(IsolationError::AuthorityGroupUnavailable)?
            .0;
        if logon.next().is_some() {
            return Err(IsolationError::AuthorityGroupUnavailable);
        }
        Ok(Self {
            process,
            token,
            owner,
            group,
            process_id,
            protected: false,
            fixture_root,
        })
    }

    #[cfg(test)]
    pub(in super::super) fn capture_fixture(root: &Path) -> Result<Self, IsolationError> {
        // Admission is specific to our owned ignored subprocess. A normal
        // Cargo harness cannot harden itself by calling this API accidentally.
        let args: Vec<_> = std::env::args().collect();
        if std::env::var("AAG_ISOLATION_MODE").as_deref() != Ok("authority")
            || !args.iter().any(|arg| arg == "--exact")
            || !args.iter().any(|arg| arg == "--ignored")
            || !args
                .iter()
                .any(|arg| arg == super::super::tests::AUTHORITY_FIXTURE)
        {
            return Err(IsolationError::AuthorityContextRequired);
        }
        let nonce = std::env::var("AAG_ISOLATION_NONCE")
            .map_err(|_| IsolationError::AuthorityContextRequired)?;
        let expected = super::super::tests::fixture_base().join(&nonce);
        if nonce.len() != 32
            || !nonce.bytes().all(|byte| byte.is_ascii_hexdigit())
            || root != expected
            || std::fs::canonicalize(root).map_err(|_| IsolationError::AuthorityContextRequired)?
                != std::fs::canonicalize(&expected)
                    .map_err(|_| IsolationError::AuthorityContextRequired)?
        {
            return Err(IsolationError::AuthorityContextRequired);
        }
        Self::capture(Some(
            std::fs::canonicalize(root).map_err(|_| IsolationError::AuthorityContextRequired)?,
        ))
    }

    pub fn owner_sid(&self) -> &Sid {
        &self.owner
    }
    #[cfg(test)]
    pub(in super::super) fn verify_filtered_authority_rejected(
        &self,
    ) -> Result<(), IsolationError> {
        let privileges = token_privileges(self.token.as_raw_handle())?;
        let first = privileges.first().ok_or(IsolationError::FixtureFailed)?;
        let mut filtered = null_mut();
        if unsafe {
            CreateRestrictedToken(
                self.token.as_raw_handle(),
                0,
                0,
                std::ptr::null(),
                1,
                first,
                0,
                std::ptr::null(),
                &mut filtered,
            )
        } == 0
        {
            return Err(IsolationError::FixtureFailed);
        }
        // This negative control is never wrapped in OwnedToken or launched.
        let filtered = own(filtered, IsolationError::FixtureFailed)?;
        let has_restrictions = token_information(filtered.as_raw_handle(), 21)?;
        if unsafe { IsTokenRestricted(filtered.as_raw_handle()) } != 0
            || unsafe { *has_restrictions.as_ptr().cast::<u32>() } == 0
            || validate_original_primary(filtered.as_raw_handle())
                != Err(IsolationError::AuthorityContextRequired)
        {
            return Err(IsolationError::FixtureFailed);
        }
        Ok(())
    }
    pub(in super::super) fn fixture_root(&self) -> Option<&Path> {
        self.fixture_root.as_deref()
    }
    pub(in super::super) fn is_protected(&self) -> bool {
        self.protected && self.process_id == unsafe { GetCurrentProcessId() }
    }
    fn kernel_descriptor(&self) -> Result<Descriptor, IsolationError> {
        // The enabled logon group admits the unrestricted B parent. Every
        // child has this group deny-only; OWNER_RIGHTS removes owner WDAC.
        Descriptor::new(&format!(
            "O:{}D:P(A;;GA;;;{})(A;;RC;;;OW)",
            self.owner.value(),
            self.group.value()
        ))
    }
    pub fn protect_current_objects(&mut self) -> Result<(), IsolationError> {
        if self.protected || self.process_id != unsafe { GetCurrentProcessId() } {
            return Err(IsolationError::AuthorityContextRequired);
        }
        let descriptor = self.kernel_descriptor()?;
        if unsafe { SetKernelObjectSecurity(self.process.as_raw_handle(), 5, descriptor.pointer()) }
            == 0
            || unsafe {
                SetKernelObjectSecurity(self.token.as_raw_handle(), 5, descriptor.pointer())
            } == 0
        {
            return Err(IsolationError::AuthorityProtectionFailed);
        }
        self.protected = true;
        Ok(())
    }

    pub fn restrict(
        &self,
        source: &SourceCapabilities,
        role: CoreRole,
    ) -> Result<OwnedToken, IsolationError> {
        self.restrict_inner(source, role, &[], AuthorityGroupMode::DenyOnly)
    }

    /// Diagnostic candidates only. The production factory keeps SourceOnly.
    /// The authority's ordinary logon group remains deny-only in every child.
    #[cfg(test)]
    pub(in super::super) fn restrict_fixture_system(
        &self,
        source: &SourceCapabilities,
        role: CoreRole,
        profile: FixtureSystemProfile,
        objects: FixtureObjectProfile,
        group_mode: AuthorityGroupMode,
    ) -> Result<OwnedToken, IsolationError> {
        let extra = match profile {
            FixtureSystemProfile::SourceOnly => Vec::new(),
            FixtureSystemProfile::WriteRestrictedCode => vec![Sid::parse("S-1-5-33")?],
            FixtureSystemProfile::ServiceRuntime => vec![
                Sid::parse("S-1-1-0")?,
                self.group.clone(),
                Sid::parse("S-1-5-33")?,
            ],
        };
        let token = self.restrict_inner(source, role, &extra, group_mode)?;
        if objects == FixtureObjectProfile::DefaultObjects {
            // Independent diagnostic: only defaults for subsequently created
            // objects change. Explicit authority/process/thread/token/stdio
            // descriptors remain protected. No child QUERY variant is added.
            let descriptor = Descriptor::new(&format!(
                "O:{}D:P(A;;GA;;;{})(A;;RC;;;OW)(A;;GRGWGXRC;;;{})",
                self.owner.value(),
                self.group.value(),
                self.owner.value()
            ))?;
            let default_dacl = TokenDefaultDacl {
                acl: descriptor.acl()?,
            };
            if unsafe {
                SetTokenInformation(
                    token.as_handle().as_raw_handle(),
                    6,
                    (&default_dacl as *const TokenDefaultDacl).cast(),
                    std::mem::size_of::<TokenDefaultDacl>() as u32,
                )
            } == 0
            {
                return Err(token_creation_error("fixture_default_object_dacl"));
            }
        }
        if objects == FixtureObjectProfile::ChildQuery {
            // Object-specific masks: process QUERY_LIMITED_INFORMATION is
            // 0x1000; thread QUERY_LIMITED_INFORMATION is 0x0800. Neither
            // descriptor grants VM, injection, duplicate, WDAC, or owner rights.
            let process = Descriptor::new(&format!(
                "O:{}D:P(A;;GA;;;{})(A;;RC;;;OW)(A;;0x00101000;;;{})",
                self.owner.value(),
                self.group.value(),
                self.owner.value()
            ))?;
            let thread = Descriptor::new(&format!(
                "O:{}D:P(A;;GA;;;{})(A;;RC;;;OW)(A;;0x00100800;;;{})",
                self.owner.value(),
                self.group.value(),
                self.owner.value()
            ))?;
            return Ok(unsafe {
                token.with_fixture_child_object_descriptors(process.bytes(), thread.bytes())
            });
        }
        Ok(token)
    }

    fn restrict_inner(
        &self,
        source: &SourceCapabilities,
        role: CoreRole,
        extra_restricting: &[Sid],
        group_mode: AuthorityGroupMode,
    ) -> Result<OwnedToken, IsolationError> {
        if !self.is_protected() {
            return Err(IsolationError::AuthorityContextRequired);
        }
        let admin = BinarySid::new(&Sid::parse("S-1-5-32-544")?)?;
        let group = BinarySid::new(&self.group)?;
        let mut disable = vec![SidAndAttributes {
            sid: admin.pointer(),
            attributes: 0,
        }];
        if group_mode == AuthorityGroupMode::DenyOnly {
            disable.push(SidAndAttributes {
                sid: group.pointer(),
                attributes: 0,
            });
        }
        let read = BinarySid::new(source.read_sid())?;
        let write = BinarySid::new(source.write_sid())?;
        let mut restricting = vec![SidAndAttributes {
            sid: read.pointer(),
            attributes: 0,
        }];
        if role == CoreRole::ReadWrite {
            restricting.push(SidAndAttributes {
                sid: write.pointer(),
                attributes: 0,
            });
        }
        let extra_binary = extra_restricting
            .iter()
            .map(BinarySid::new)
            .collect::<Result<Vec<_>, _>>()?;
        restricting.extend(extra_binary.iter().map(|sid| SidAndAttributes {
            sid: sid.pointer(),
            attributes: 0,
        }));
        let mut privileges = token_privileges(self.token.as_raw_handle())?;
        for privilege in &mut privileges {
            privilege.attributes = 0;
        }
        let mut handle = null_mut();
        // DISABLE_MAX_PRIVILEGE would ignore this deletion list. Enumerate
        // and delete every privilege, including SeChangeNotifyPrivilege.
        if unsafe {
            CreateRestrictedToken(
                self.token.as_raw_handle(),
                WRITE_RESTRICTED,
                disable.len() as u32,
                disable.as_ptr(),
                privileges.len() as u32,
                privileges.as_ptr(),
                restricting.len() as u32,
                restricting.as_ptr(),
                &mut handle,
            )
        } == 0
        {
            let error = token_creation_error("create_restricted_token");
            return Err(error);
        }
        let token = own(handle, IsolationError::TokenCreationFailed)?;
        if unsafe { IsTokenRestricted(token.as_raw_handle()) } == 0
            || token_user(token.as_raw_handle())? != self.owner
            || !token_privileges(token.as_raw_handle())?.is_empty()
        {
            return Err(IsolationError::TokenVerificationFailed);
        }
        let groups = token_groups(token.as_raw_handle(), 2)?;
        if !groups.iter().any(|(sid, attrs)| {
            sid == &self.group
                && match group_mode {
                    AuthorityGroupMode::DenyOnly => {
                        attrs & GROUP_DENY_ONLY != 0 && attrs & GROUP_ENABLED == 0
                    }
                    #[cfg(test)]
                    AuthorityGroupMode::KeepEnabled => {
                        attrs & GROUP_DENY_ONLY == 0 && attrs & GROUP_ENABLED != 0
                    }
                }
        }) || groups.iter().any(|(sid, attrs)| {
            sid.value() == "S-1-5-32-544"
                && (attrs & GROUP_DENY_ONLY == 0 || attrs & GROUP_ENABLED != 0)
        }) {
            return Err(IsolationError::TokenVerificationFailed);
        }
        let actual = token_groups(token.as_raw_handle(), 11)?;
        if actual.len() != restricting.len()
            || !actual.iter().any(|(sid, _)| sid == source.read_sid())
            || (role == CoreRole::ReadWrite
                && !actual.iter().any(|(sid, _)| sid == source.write_sid()))
            || extra_restricting
                .iter()
                .any(|expected| !actual.iter().any(|(sid, _)| sid == expected))
        {
            return Err(IsolationError::TokenVerificationFailed);
        }
        let descriptor = self.kernel_descriptor()?;
        let default_dacl = TokenDefaultDacl {
            acl: descriptor.acl()?,
        };
        let default_owner = BinarySid::new(&self.owner)?;
        let owner_pointer = default_owner.pointer();
        if unsafe {
            SetTokenInformation(
                token.as_raw_handle(),
                6,
                (&default_dacl as *const TokenDefaultDacl).cast::<c_void>(),
                std::mem::size_of::<TokenDefaultDacl>() as u32,
            )
        } == 0
        {
            return Err(token_creation_error("set_token_default_dacl"));
        }
        if unsafe {
            SetTokenInformation(
                token.as_raw_handle(),
                4,
                (&owner_pointer as *const Psid).cast::<c_void>(),
                std::mem::size_of::<Psid>() as u32,
            )
        } == 0
        {
            return Err(token_creation_error("set_token_default_owner"));
        }
        if unsafe { SetKernelObjectSecurity(token.as_raw_handle(), 5, descriptor.pointer()) } == 0 {
            return Err(token_creation_error("protect_role_token_object"));
        }
        // Only this verified caller-derived primary token crosses the shared
        // process boundary. The preopened full token handle is never inherited.
        let child_token_descriptor = Descriptor::new(&format!(
            "O:{}D:P(A;;GA;;;{})(A;;RC;;;OW)(A;;0x00000008;;;{})",
            self.owner.value(),
            self.group.value(),
            self.owner.value()
        ))?;
        Ok(unsafe {
            OwnedToken::from_caller_derived_restricted(
                token,
                descriptor.bytes(),
                child_token_descriptor.bytes(),
            )
        })
    }
}
