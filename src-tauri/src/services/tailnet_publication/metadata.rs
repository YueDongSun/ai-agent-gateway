//! Read only installed Tailscale self metadata. Peer records never leave here.

use super::PublicationError;
use serde::Deserialize;
use std::{
    net::IpAddr,
    path::PathBuf,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

const STATUS_LIMIT: usize = 1024 * 1024;
const STATUS_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TailnetSelfStatus {
    pub self_ip: Option<IpAddr>,
}

#[derive(Deserialize)]
struct StatusDocument {
    #[serde(rename = "BackendState")]
    backend_state: String,
    #[serde(rename = "Self")]
    own: Option<SelfDocument>,
}

#[derive(Deserialize)]
struct SelfDocument {
    #[serde(rename = "TailscaleIPs", default)]
    addresses: Vec<String>,
}

pub(crate) fn parse_self_status(raw: &[u8]) -> Result<TailnetSelfStatus, PublicationError> {
    if raw.len() > STATUS_LIMIT {
        return Err(PublicationError::OutputLimitExceeded);
    }
    let document: StatusDocument =
        serde_json::from_slice(raw).map_err(|_| PublicationError::StatusInvalid)?;
    if document.backend_state != "Running" {
        return Ok(TailnetSelfStatus { self_ip: None });
    }
    let own = document.own.ok_or(PublicationError::StatusInvalid)?;
    if own.addresses.len() > 16 {
        return Err(PublicationError::StatusInvalid);
    }
    let mut addresses = Vec::new();
    for address in own.addresses {
        let address: IpAddr = address
            .parse()
            .map_err(|_| PublicationError::StatusInvalid)?;
        let tailnet_address = match address {
            IpAddr::V4(address) => u32::from(address) & 0xffc0_0000 == 0x6440_0000,
            IpAddr::V6(address) => address.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0],
        };
        if !tailnet_address {
            return Err(PublicationError::StatusInvalid);
        }
        addresses.push(address);
    }
    addresses.sort_by_key(|address| (address.is_ipv6(), *address));
    Ok(TailnetSelfStatus {
        self_ip: addresses.into_iter().next(),
    })
}

pub(crate) fn read_self_status() -> Result<TailnetSelfStatus, PublicationError> {
    let executable = trusted_executable()?;
    let arguments = vec!["status".into(), "--json".into()];
    let output = crate::services::owned_process::run_capture(
        &crate::services::owned_process::SpawnSpec::new(&executable, &arguments),
        &AtomicBool::new(false),
        Instant::now() + STATUS_TIMEOUT,
        STATUS_LIMIT,
    )
    .map_err(PublicationError::from)?;
    if output.exit_code != 0 {
        return Err(PublicationError::CommandFailed);
    }
    parse_self_status(&output.stdout)
}

#[cfg(windows)]
pub(crate) fn trusted_executable() -> Result<PathBuf, PublicationError> {
    use std::{
        ffi::OsString,
        os::windows::{ffi::OsStringExt, fs::MetadataExt},
    };
    #[repr(C)]
    struct Guid {
        first: u32,
        second: u16,
        third: u16,
        last: [u8; 8],
    }
    #[link(name = "shell32")]
    extern "system" {
        fn SHGetKnownFolderPath(
            folder: *const Guid,
            flags: u32,
            token: *mut std::ffi::c_void,
            path: *mut *mut u16,
        ) -> i32;
    }
    #[link(name = "ole32")]
    extern "system" {
        fn CoTaskMemFree(pointer: *mut std::ffi::c_void);
    }
    // FOLDERID_ProgramFiles: OS-owned install location, never PATH or caller input.
    let folder = Guid {
        first: 0x905e63b6,
        second: 0xc1bf,
        third: 0x494e,
        last: [0xb2, 0x9c, 0x65, 0xb7, 0x32, 0xd3, 0xd2, 0x1a],
    };
    let mut pointer = std::ptr::null_mut();
    let result = unsafe { SHGetKnownFolderPath(&folder, 0, std::ptr::null_mut(), &mut pointer) };
    if result < 0 || pointer.is_null() {
        return Err(PublicationError::ProcessUnavailable);
    }
    let mut length = 0;
    while length < 32768 && unsafe { *pointer.add(length) } != 0 {
        length += 1;
    }
    let root = if length < 32768 {
        Some(PathBuf::from(OsString::from_wide(unsafe {
            std::slice::from_raw_parts(pointer, length)
        })))
    } else {
        None
    };
    unsafe {
        CoTaskMemFree(pointer.cast());
    }
    let executable = root
        .ok_or(PublicationError::ProcessUnavailable)?
        .join("Tailscale")
        .join("tailscale.exe");
    for path in executable.ancestors() {
        let metadata =
            std::fs::symlink_metadata(path).map_err(|_| PublicationError::ProcessUnavailable)?;
        if metadata.file_attributes() & 0x400 != 0 || metadata.file_type().is_symlink() {
            return Err(PublicationError::ProcessUnavailable);
        }
    }
    if !executable.is_absolute() || !executable.is_file() {
        return Err(PublicationError::ProcessUnavailable);
    }
    Ok(executable)
}

#[cfg(not(windows))]
pub(crate) fn trusted_executable() -> Result<PathBuf, PublicationError> {
    Err(PublicationError::PlatformUnsupported)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_running_self_overlay_addresses_can_form_an_endpoint() {
        let status = parse_self_status(br#"{"BackendState":"Running","Self":{"TailscaleIPs":["fd7a:115c:a1e0::1234","100.64.1.2"]},"Peer":{"private":{"TailscaleIPs":["100.64.9.9"]}},"TailscaleIPs":["100.64.8.8"]}"#).unwrap();
        assert_eq!(status.self_ip, Some("100.64.1.2".parse().unwrap()));
        assert_eq!(
            parse_self_status(
                br#"{"BackendState":"Stopped","Self":{"TailscaleIPs":["100.64.1.2"]}}"#
            )
            .unwrap()
            .self_ip,
            None
        );
        assert!(parse_self_status(
            br#"{"BackendState":"Running","Peer":{"private":{"TailscaleIPs":["100.64.9.9"]}}}"#
        )
        .is_err());
    }

    #[test]
    fn self_metadata_rejects_non_overlay_malformed_and_oversized_values() {
        for address in [
            "127.0.0.1",
            "192.168.1.1",
            "100.128.0.1",
            "::1",
            "fd00::1",
            "not-an-address",
        ] {
            let raw =
                serde_json::json!({"BackendState":"Running","Self":{"TailscaleIPs":[address]}})
                    .to_string();
            assert_eq!(
                parse_self_status(raw.as_bytes()).unwrap_err(),
                PublicationError::StatusInvalid
            );
        }
        assert_eq!(
            parse_self_status(&vec![b' '; STATUS_LIMIT + 1]).unwrap_err(),
            PublicationError::OutputLimitExceeded
        );
    }
}
