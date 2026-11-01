//! Local Windows pipe creation and same-handle kernel owner attestation.
//! Production uses explicit service identities; fixture identities stay test-only.

use super::*;
use std::{
    os::windows::io::{AsRawHandle, IntoRawHandle},
    time::Duration,
};
use tokio::net::windows::named_pipe::{NamedPipeClient, NamedPipeServer, ServerOptions};

// Specific rights deliberately exclude FILE_CREATE_PIPE_INSTANCE (0x4).
pub(super) const CLIENT_ACCESS: u32 = 0x0012_0183;
// OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION.
pub(super) const CLIENT_FLAGS: u32 = 0x4000_0000 | 0x0010_0000 | 0x0001_0000;
const CONNECT_ATTEMPTS: usize = 64;
const CONNECT_INTERVAL: Duration = Duration::from_millis(25);
const MAX_CLIENT_IDENTITIES: usize = 64;
const PIPE_BUFFER_BYTES: u32 = 64 * 1024;
const LISTENER_CLEANUP_TIMEOUT: Duration = Duration::from_secs(2);

struct ListenerSpec {
    name: PipeName,
    owner: Sid,
    role: Option<ServiceRole>,
    clients: Vec<Sid>,
    context: Option<ServiceContext>,
}

pub(super) fn bind(
    context: &ServiceContext,
    peer: ServicePeer,
    clients: &[Sid],
    limits: TransportLimits,
) -> Result<LocalListener, TransportError> {
    if context.role() != peer.role {
        return Err(TransportError::ServiceContextRequired);
    }
    let current = context
        .verified_user_sid()
        .map_err(|_| TransportError::ServiceContextRequired)?;
    if current != peer.owner_sid.value() {
        return Err(TransportError::CreatorIdentityMismatch);
    }
    start_listener(
        ListenerSpec {
            name: peer.name,
            owner: peer.owner_sid,
            role: Some(peer.role),
            clients: clients.to_vec(),
            context: Some(context.clone()),
        },
        limits,
    )
}

fn create_server(
    spec: &ListenerSpec,
    limits: TransportLimits,
    first: bool,
) -> Result<NamedPipeServer, TransportError> {
    if let Some(context) = &spec.context {
        let current = context
            .verified_user_sid()
            .map_err(|_| TransportError::ServiceContextRequired)?;
        if current != spec.owner.value() {
            return Err(TransportError::CreatorIdentityMismatch);
        }
    } else if ffi::current_user_sid()? != spec.owner {
        return Err(TransportError::CreatorIdentityMismatch);
    }
    let descriptor = ffi::descriptor(&pipe_sddl(&spec.owner, &spec.clients)?)?;
    let mut attributes = ffi::SecurityAttributes {
        length: std::mem::size_of::<ffi::SecurityAttributes>() as u32,
        descriptor: descriptor.0,
        inherit: 0,
    };
    let mut options = ServerOptions::new();
    options
        .first_pipe_instance(first)
        .reject_remote_clients(true)
        .max_instances(limits.connections + 1)
        .in_buffer_size(PIPE_BUFFER_BYTES)
        .out_buffer_size(PIPE_BUFFER_BYTES);
    let server = unsafe {
        options.create_with_security_attributes_raw(
            spec.name.as_str(),
            (&mut attributes as *mut ffi::SecurityAttributes).cast(),
        )
    }
    .map_err(|error| {
        if first && error.raw_os_error() == Some(5) {
            TransportError::NamespaceOccupied
        } else {
            lifecycle::io_error(error, KernelOperation::Create)
        }
    })?;
    if ffi::object_owner(server.as_raw_handle())? != spec.owner {
        return Err(TransportError::CreatorIdentityMismatch);
    }
    Ok(server)
}

pub(super) fn pipe_sddl(owner: &Sid, clients: &[Sid]) -> Result<String, TransportError> {
    if clients.is_empty() || clients.len() > MAX_CLIENT_IDENTITIES {
        return Err(TransportError::InvalidEndpoint);
    }
    let mut value = format!(
        "O:{}G:{}D:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;{})(A;;RC;;;OW)",
        owner.value(),
        owner.value(),
        owner.value()
    );
    for client in clients {
        value.push_str(&format!("(A;;0x{CLIENT_ACCESS:08x};;;{})", client.value()));
    }
    Ok(value)
}

fn start_listener(
    spec: ListenerSpec,
    limits: TransportLimits,
) -> Result<LocalListener, TransportError> {
    limits.validate()?;
    tokio::runtime::Handle::try_current().map_err(|_| TransportError::RuntimeUnavailable)?;
    let mut pending = create_server(&spec, limits, true)?;
    let control = lifecycle::TaskControl::new();
    let driver_control = control.clone();
    let (sender, receiver) = mpsc::channel(limits.connections);
    let incoming = Arc::new(Mutex::new(receiver));
    let cleanup = incoming.clone();
    let slots = Arc::new(Semaphore::new(limits.connections));
    control.spawn(async move {
        loop {
            let permit = tokio::select! {
                biased;
                _ = driver_control.cancel.cancelled() => break,
                value = slots.clone().acquire_owned() => match value { Ok(value) => value, Err(_) => break },
            };
            let connected = tokio::select! {
                biased;
                _ = driver_control.cancel.cancelled() => break,
                result = pending.connect() => result,
            };
            if let Err(error) = connected { driver_control.fail(lifecycle::io_error(error, KernelOperation::Accept)); break; }
            let pid = match ffi::process_id(pending.as_raw_handle(), false) { Ok(pid) => pid, Err(error) => { driver_control.fail(error); break; } };
            // Keep the namespace continuously owned while rotating instances.
            let next = match create_server(&spec, limits, false) { Ok(value) => value, Err(error) => { driver_control.fail(error); break; } };
            let stream = std::mem::replace(&mut pending, next);
            let peer = PeerAttestation { owner_sid: spec.owner.clone(), role: spec.role, peer_process_id: Some(pid) };
            let connection = match lifecycle::start(stream, peer, limits, Some(permit)) { Ok(value) => value, Err(error) => { driver_control.fail(error); break; } };
            tokio::select! {
                biased;
                _ = driver_control.cancel.cancelled() => break,
                result = sender.send(connection) => if result.is_err() { break; },
            }
        }
        drop(pending);
        drop(sender);
        // Undelivered connections remain listener-owned and close on shutdown.
        let mut incoming = cleanup.lock().await;
        incoming.close();
        let mut owned = Vec::new();
        while let Ok(connection) = incoming.try_recv() { owned.push(connection); }
        drop(incoming);
        let deadline = Instant::now() + LISTENER_CLEANUP_TIMEOUT;
        for connection in owned {
            if let TransportClosed::OutcomeUnknown(error) = connection.close(deadline).await {
                driver_control.cleanup_failed(error);
            }
        }
    })?;
    Ok(LocalListener { control, incoming })
}

pub(super) async fn connect(
    name: &PipeName,
    expected: &Sid,
    role: Option<ServiceRole>,
    limits: TransportLimits,
    deadline: Instant,
    cancel: &TransportCancellation,
) -> Result<LocalConnection, TransportError> {
    limits.validate()?;
    tokio::runtime::Handle::try_current().map_err(|_| TransportError::RuntimeUnavailable)?;
    for attempt in 0..CONNECT_ATTEMPTS {
        if cancel.is_cancelled() {
            return Err(TransportError::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(TransportError::DeadlineExceeded);
        }
        match ffi::open(name.as_str(), CLIENT_ACCESS, CLIENT_FLAGS) {
            Ok(handle) => {
                // Never reopen by name between identity validation and IO.
                let owner = ffi::object_owner(handle.as_raw_handle())?;
                if owner != *expected {
                    return Err(TransportError::PeerOwnerMismatch);
                }
                let pid = ffi::process_id(handle.as_raw_handle(), true)?;
                if cancel.is_cancelled() {
                    return Err(TransportError::Cancelled);
                }
                if Instant::now() >= deadline {
                    return Err(TransportError::DeadlineExceeded);
                }
                let stream = unsafe { NamedPipeClient::from_raw_handle(handle.into_raw_handle()) }
                    .map_err(|error| lifecycle::io_error(error, KernelOperation::Open))?;
                return lifecycle::start(
                    stream,
                    PeerAttestation {
                        owner_sid: owner,
                        role,
                        peer_process_id: Some(pid),
                    },
                    limits,
                    None,
                );
            }
            Err(2 | 3) => return Err(TransportError::EndpointUnavailable),
            Err(231) if attempt + 1 < CONNECT_ATTEMPTS => {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Err(TransportError::Cancelled),
                    _ = tokio::time::sleep_until(deadline.into()) => return Err(TransportError::DeadlineExceeded),
                    _ = tokio::time::sleep(CONNECT_INTERVAL) => {}
                }
            }
            Err(231) => return Err(TransportError::PipeBusy),
            Err(code) => {
                return Err(TransportError::Kernel {
                    operation: KernelOperation::Open,
                    code,
                })
            }
        }
    }
    Err(TransportError::PipeBusy)
}

#[cfg(test)]
pub(super) use super::fixture::{fixture_access_probe, fixture_duplex, FixtureEndpoint};
#[cfg(test)]
pub(super) fn fixture_listener(
    endpoint: &FixtureEndpoint,
    limits: TransportLimits,
) -> Result<LocalListener, TransportError> {
    start_listener(
        ListenerSpec {
            name: endpoint.name.clone(),
            owner: endpoint.owner.clone(),
            role: None,
            clients: vec![endpoint.owner.clone()],
            context: None,
        },
        limits,
    )
}
#[cfg(test)]
pub(super) async fn fixture_connect(
    endpoint: &FixtureEndpoint,
    owner: &Sid,
    limits: TransportLimits,
    deadline: Instant,
) -> Result<LocalConnection, TransportError> {
    connect(
        &endpoint.name,
        owner,
        None,
        limits,
        deadline,
        &TransportCancellation::new(),
    )
    .await
}
