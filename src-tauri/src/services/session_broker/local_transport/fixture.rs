//! Task-owned current-user pipe fixtures, never service-principal qualification.
//! Impersonation is confined to a dedicated test thread and reverted by RAII.
use super::ffi::*;
use super::*;
use std::{
    mem::{offset_of, size_of},
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    ptr::{null, null_mut},
};
use tokio::net::windows::named_pipe::ServerOptions;

pub(super) struct FixtureEndpoint {
    pub name: PipeName,
    pub owner: Sid,
}
impl FixtureEndpoint {
    pub fn new() -> Result<Self, TransportError> {
        let mut nonce = [0u8; 16];
        getrandom::getrandom(&mut nonce).map_err(|_| TransportError::InvalidEndpoint)?;
        let suffix: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
        Ok(Self {
            name: PipeName {
                value: format!(r"\\.\pipe\aag_transport_fixture_{suffix}"),
            },
            owner: current_user_sid()?,
        })
    }
    pub fn owner(&self) -> &Sid {
        &self.owner
    }
}
/// Synchronous by design: a current-thread test can close this handle before
/// Tokio first polls the listener's already-spawned accept task.
pub(super) fn fixture_disconnect_before_accept(
    endpoint: &FixtureEndpoint,
) -> Result<(), TransportError> {
    let handle = open(
        endpoint.name.as_str(),
        pipe::CLIENT_ACCESS,
        pipe::CLIENT_FLAGS,
    )
    .map_err(|code| TransportError::Kernel {
        operation: KernelOperation::Open,
        code,
    })?;
    if object_owner(handle.as_raw_handle())? != endpoint.owner {
        return Err(TransportError::PeerOwnerMismatch);
    }
    drop(handle);
    Ok(())
}
pub(super) fn fixture_duplex(
    stream: tokio::io::DuplexStream,
    limits: TransportLimits,
) -> Result<LocalConnection, TransportError> {
    lifecycle::start(
        stream,
        PeerAttestation {
            owner_sid: current_user_sid()?,
            role: None,
            peer_process_id: None,
        },
        limits,
        None,
    )
}
pub(super) struct AccessProbe {
    pub opened_rw: bool,
    pub generic_write_denied: bool,
    pub instance_creation_denied: bool,
    pub granted_access: u32,
}

fn groups(buffer: &TokenBuffer) -> Result<&[SidAndAttributes], TransportError> {
    let offset = offset_of!(TokenGroups, groups);
    if buffer.bytes < offset {
        return Err(TransportError::InvalidEndpoint);
    }
    let count = unsafe { *buffer.words.as_ptr().cast::<u32>() } as usize;
    if count
        .checked_mul(size_of::<SidAndAttributes>())
        .and_then(|size| size.checked_add(offset))
        .is_none_or(|end| end > buffer.bytes)
    {
        return Err(TransportError::InvalidEndpoint);
    }
    Ok(unsafe {
        std::slice::from_raw_parts(buffer.words.as_ptr().cast::<u8>().add(offset).cast(), count)
    })
}

pub(super) async fn fixture_access_probe(
    limits: TransportLimits,
) -> Result<AccessProbe, TransportError> {
    limits.validate()?;
    let endpoint = FixtureEndpoint::new()?;
    let token = current_token(0x8 | 0x2 | 0x1)?;
    let token_groups = token_information(&token, 2)?;
    let logon = groups(&token_groups)?
        .iter()
        .find(|entry| entry.attributes & 0xc000_0000 == 0xc000_0000 && entry.attributes & 4 != 0)
        .ok_or(TransportError::InvalidEndpoint)?;
    let logon_sid = unsafe { sid_text(logon.sid) }?;
    let descriptor = descriptor(&pipe::pipe_sddl(&endpoint.owner, &[logon_sid.clone()])?)?;
    let mut attributes = SecurityAttributes {
        length: size_of::<SecurityAttributes>() as u32,
        descriptor: descriptor.0,
        inherit: 0,
    };
    let server = unsafe {
        ServerOptions::new()
            .first_pipe_instance(true)
            .reject_remote_clients(true)
            .max_instances(3)
            .create_with_security_attributes_raw(
                endpoint.name.as_str(),
                (&mut attributes as *mut SecurityAttributes).cast(),
            )
    }
    .map_err(|error| lifecycle::io_error(error, KernelOperation::Create))?;
    // No process-wide token mutation, account creation, filesystem ACL, or wait
    // API. The dedicated thread performs a finite sequence of local kernel calls.
    let name = endpoint.name.value;
    let probe = std::thread::spawn(move || restricted_access_probe(name, logon_sid));
    let result = probe.join().map_err(|_| TransportError::Kernel {
        operation: KernelOperation::Join,
        code: 0,
    })?;
    drop(server);
    result
}

fn restricted_access_probe(name: String, logon: Sid) -> Result<AccessProbe, TransportError> {
    let token = current_token(0x8 | 0x2 | 0x1)?;
    let user = token_information(&token, 1)?;
    if user.bytes < size_of::<SidAndAttributes>() {
        return Err(TransportError::InvalidEndpoint);
    }
    let user_entry = unsafe { *user.words.as_ptr().cast::<SidAndAttributes>() };
    let group_buffer = token_information(&token, 2)?;
    let mut disabled = vec![SidAndAttributes {
        sid: user_entry.sid,
        attributes: 0,
    }];
    for entry in groups(&group_buffer)? {
        let sid = unsafe { sid_text(entry.sid) }?;
        if sid.value() == "S-1-5-32-544" {
            disabled.push(SidAndAttributes {
                sid: entry.sid,
                attributes: 0,
            });
        }
    }
    // Disable every privilege except SeChangeNotifyPrivilege, including any
    // enabled administrator privilege that could bypass this access check.
    let mut raw = null_mut();
    if unsafe {
        CreateRestrictedToken(
            token.as_raw_handle(),
            1,
            disabled.len() as u32,
            disabled.as_ptr(),
            0,
            null(),
            0,
            null(),
            &mut raw,
        )
    } == 0
    {
        return Err(last_error(KernelOperation::Identity));
    }
    let restricted = unsafe { OwnedHandle::from_raw_handle(raw) };
    let restricted_groups = token_information(&restricted, 2)?;
    let mut logon_enabled = false;
    for entry in groups(&restricted_groups)? {
        if unsafe { sid_text(entry.sid) }? == logon {
            logon_enabled = entry.attributes & 4 != 0 && entry.attributes & 0x10 == 0;
        }
    }
    if !logon_enabled {
        return Err(TransportError::InvalidEndpoint);
    }
    if unsafe { ImpersonateLoggedOnUser(restricted.as_raw_handle()) } == 0 {
        return Err(last_error(KernelOperation::Identity));
    }
    let guard = ImpersonationGuard;
    let generic_write_denied = match open(&name, 0xc000_0000, pipe::CLIENT_FLAGS) {
        Err(5) => true,
        Ok(handle) => {
            drop(handle);
            false
        }
        Err(code) => {
            return Err(TransportError::Kernel {
                operation: KernelOperation::Open,
                code,
            })
        }
    };
    let wide_name = wide(&name);
    let extra = unsafe {
        CreateNamedPipeW(
            wide_name.as_ptr(),
            3 | 0x4000_0000,
            8,
            3,
            65536,
            65536,
            0,
            null_mut(),
        )
    };
    let instance_creation_denied = if extra == INVALID_HANDLE {
        let code = unsafe { GetLastError() };
        if code != 5 {
            return Err(TransportError::Kernel {
                operation: KernelOperation::Create,
                code,
            });
        }
        true
    } else {
        drop(unsafe { OwnedHandle::from_raw_handle(extra) });
        false
    };
    let handle = open(&name, pipe::CLIENT_ACCESS, pipe::CLIENT_FLAGS).map_err(|code| {
        TransportError::Kernel {
            operation: KernelOperation::Open,
            code,
        }
    })?;
    let mut basic = ObjectBasicInformation {
        attributes: 0,
        granted_access: 0,
        handle_count: 0,
        pointer_count: 0,
        reserved: [0; 10],
    };
    let code = unsafe {
        NtQueryObject(
            handle.as_raw_handle(),
            0,
            (&mut basic as *mut ObjectBasicInformation).cast(),
            size_of::<ObjectBasicInformation>() as u32,
            null_mut(),
        )
    };
    if code < 0 {
        return Err(TransportError::Kernel {
            operation: KernelOperation::Attest,
            code: code as u32,
        });
    }
    drop(handle);
    drop(guard);
    Ok(AccessProbe {
        opened_rw: true,
        generic_write_denied,
        instance_creation_denied,
        granted_access: basic.granted_access,
    })
}
struct ImpersonationGuard;
impl Drop for ImpersonationGuard {
    fn drop(&mut self) {
        // Never continue the test worker thread after failed reversion.
        if unsafe { RevertToSelf() } == 0 {
            std::process::abort();
        }
    }
}
