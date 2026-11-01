//! Immutable deployment inventory admitted from a protected configuration file.
//! Parsing establishes scalar consistency only: source mode, native ownership,
//! ACLs, installation commit and readiness remain independent owner checks.

use super::protected_config::VerifiedConfigBytes;
use crate::{
    config::private_fs::Managed_directory_identity,
    services::session_broker::{
        service_host::ServiceRole, source_catalog::SourceKey, windows_isolation::Sid,
        CanonicalSessionId, NativeSessionId, NativeThreadId,
    },
};
use serde::Deserialize;
use std::{
    collections::HashSet,
    num::{NonZeroU16, NonZeroU64},
    path::{Path, PathBuf},
};

const MAX_CONFIG_BYTES: usize = 1024 * 1024;
const MAX_IDENTIFIER_BYTES: usize = 256;
const MIN_SOURCES: usize = 2;
const MAX_SOURCES: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RuntimeConfigError {
    InvalidJson,
    InvalidSchema,
    InvalidBounds,
    InvalidIdentifier,
    InvalidPath,
    InvalidImage,
    InvalidSid,
    InvalidPort,
    Duplicate,
    OwnerMismatch,
    RoleMismatch,
    KnownFolderUnavailable,
}
impl RuntimeConfigError {
    pub(super) fn code(self) -> &'static str {
        match self {
            Self::InvalidJson => "session_runtime_config_json_invalid",
            Self::InvalidSchema => "session_runtime_config_schema_invalid",
            Self::InvalidBounds => "session_runtime_config_bounds_invalid",
            Self::InvalidIdentifier => "session_runtime_config_identifier_invalid",
            Self::InvalidPath => "session_runtime_config_path_invalid",
            Self::InvalidImage => "session_runtime_config_image_invalid",
            Self::InvalidSid => "session_runtime_config_sid_invalid",
            Self::InvalidPort => "session_runtime_config_port_invalid",
            Self::Duplicate => "session_runtime_config_duplicate",
            Self::OwnerMismatch => "session_runtime_config_owner_mismatch",
            Self::RoleMismatch => "session_runtime_config_role_mismatch",
            Self::KnownFolderUnavailable => "session_runtime_config_known_folder_unavailable",
        }
    }
}
impl std::fmt::Display for RuntimeConfigError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.code())
    }
}
impl std::error::Error for RuntimeConfigError {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawImagePin {
    path: String,
    object: Managed_directory_identity,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSource {
    source_key: String,
    canonical_id: String,
    native_session_id: String,
    native_thread_ids: Vec<String>,
    native_home: String,
    reader_index: String,
    runtime_index: String,
    generation: u64,
    runtime_sid: String,
    root_identity: Managed_directory_identity,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRuntimeConfig {
    schema_version: u32,
    installation_id: String,
    operator_home: String,
    client_root: String,
    protected_shim: RawImagePin,
    pinned_native: RawImagePin,
    policy_sid: String,
    reader_sid: String,
    operator_sid: String,
    gateway_port: u64,
    sources: Vec<RawSource>,
}

pub(super) struct InstallationId(String);
impl InstallationId {
    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}
pub(super) struct UserDiskPath(PathBuf);
impl UserDiskPath {
    pub(super) fn as_path(&self) -> &Path {
        &self.0
    }
}
pub(super) struct RuntimeImagePin {
    path: PathBuf,
    object: Managed_directory_identity,
}
impl RuntimeImagePin {
    pub(super) fn path(&self) -> &Path {
        &self.path
    }
    pub(super) fn object(&self) -> Managed_directory_identity {
        self.object
    }
}
pub(super) struct RuntimeSource {
    pub(super) source_key: SourceKey,
    pub(super) canonical_id: CanonicalSessionId,
    pub(super) native_session_id: NativeSessionId,
    pub(super) native_thread_ids: Vec<NativeThreadId>,
    pub(super) native_home: UserDiskPath,
    pub(super) reader_index: UserDiskPath,
    pub(super) runtime_index: UserDiskPath,
    pub(super) generation: NonZeroU64,
    pub(super) runtime_sid: Sid,
    pub(super) root_identity: Managed_directory_identity,
}
pub(super) struct RuntimeData {
    pub(super) installation_id: InstallationId,
    pub(super) operator_home: UserDiskPath,
    pub(super) client_root: UserDiskPath,
    pub(super) protected_shim: RuntimeImagePin,
    pub(super) pinned_native: RuntimeImagePin,
    pub(super) policy_sid: Sid,
    pub(super) reader_sid: Sid,
    pub(super) operator_sid: Sid,
    pub(super) gateway_port: NonZeroU16,
    pub(super) sources: Vec<RuntimeSource>,
}

/// No Clone/Debug/Serde: the protected file and ancestor guards stay alive.
/// This is inventory admission, never a committed-installation capability.
pub(super) struct ValidatedRuntimeConfig {
    protected: VerifiedConfigBytes,
    data: RuntimeData,
}
impl ValidatedRuntimeConfig {
    pub(super) fn from_verified(
        protected: VerifiedConfigBytes,
    ) -> Result<Self, RuntimeConfigError> {
        let program_files = super::owner_client::program_files_folder()
            .map_err(|_| RuntimeConfigError::KnownFolderUnavailable)?;
        let data = parse_data(
            protected.contents(),
            protected.role(),
            protected.owner_sid(),
            &program_files,
        )?;
        Ok(Self { protected, data })
    }
    pub(super) fn data(&self) -> &RuntimeData {
        &self.data
    }
    pub(super) fn role(&self) -> ServiceRole {
        self.protected.role()
    }
    pub(super) fn config_object(&self) -> Managed_directory_identity {
        self.protected.object()
    }
}

#[cfg(test)]
struct FixtureRuntimeData(RuntimeData);
#[cfg(test)]
fn parse_fixture(
    bytes: &[u8],
    role: ServiceRole,
    owner_sid: &str,
    program_files: &Path,
) -> Result<FixtureRuntimeData, RuntimeConfigError> {
    parse_data(bytes, role, owner_sid, program_files).map(FixtureRuntimeData)
}

fn opaque(value: &str) -> Result<(), RuntimeConfigError> {
    if value.is_empty()
        || value.len() > MAX_IDENTIFIER_BYTES
        || value.trim().is_empty()
        || value.chars().any(char::is_control)
    {
        return Err(RuntimeConfigError::InvalidIdentifier);
    }
    Ok(())
}

fn disk_path(value: &str) -> Result<UserDiskPath, RuntimeConfigError> {
    let path = PathBuf::from(value);
    super::owner_client::validate_disk_path(&path).map_err(|_| RuntimeConfigError::InvalidPath)?;
    Ok(UserDiskPath(path))
}

fn canonical_numbers(value: &str, count: usize) -> bool {
    let parts: Vec<_> = value.split('-').collect();
    parts.len() == count
        && parts.iter().all(|part| {
            part.parse::<u32>()
                .is_ok_and(|number| number.to_string() == *part)
        })
}

fn service_sid(value: &str) -> Result<Sid, RuntimeConfigError> {
    if !value
        .strip_prefix("S-1-5-80-")
        .is_some_and(|suffix| canonical_numbers(suffix, 5))
    {
        return Err(RuntimeConfigError::InvalidSid);
    }
    Sid::parse(value).map_err(|_| RuntimeConfigError::InvalidSid)
}

fn operator_sid(value: &str) -> Result<Sid, RuntimeConfigError> {
    // These are account-shaped namespaces, not a claim that the SID denotes an
    // existing human. The installer separately binds the exact operator token.
    if !["S-1-5-21-", "S-1-12-1-"].iter().any(|prefix| {
        value
            .strip_prefix(*prefix)
            .is_some_and(|suffix| canonical_numbers(suffix, 4))
    }) {
        return Err(RuntimeConfigError::InvalidSid);
    }
    Sid::parse(value).map_err(|_| RuntimeConfigError::InvalidSid)
}

fn unique(values: &mut HashSet<String>, value: &str) -> Result<(), RuntimeConfigError> {
    if values.insert(value.to_owned()) {
        Ok(())
    } else {
        Err(RuntimeConfigError::Duplicate)
    }
}

fn parse_data(
    bytes: &[u8],
    role: ServiceRole,
    owner_sid: &str,
    program_files: &Path,
) -> Result<RuntimeData, RuntimeConfigError> {
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err(RuntimeConfigError::InvalidBounds);
    }
    let raw: RawRuntimeConfig =
        serde_json::from_slice(bytes).map_err(|_| RuntimeConfigError::InvalidJson)?;
    if raw.schema_version != 1 {
        return Err(RuntimeConfigError::InvalidSchema);
    }
    if !(MIN_SOURCES..=MAX_SOURCES).contains(&raw.sources.len()) {
        return Err(RuntimeConfigError::InvalidBounds);
    }
    SourceKey::parse(&raw.installation_id).map_err(|_| RuntimeConfigError::InvalidIdentifier)?;
    let installation_id = InstallationId(raw.installation_id);
    let operator_home = disk_path(&raw.operator_home)?;
    let client_root = disk_path(&raw.client_root)?;
    if client_root.0 != operator_home.0.join(".codex-cli") {
        return Err(RuntimeConfigError::InvalidPath);
    }
    let protected_shim = RuntimeImagePin {
        path: raw.protected_shim.path.into(),
        object: raw.protected_shim.object,
    };
    let pinned_native = RuntimeImagePin {
        path: raw.pinned_native.path.into(),
        object: raw.pinned_native.object,
    };
    if protected_shim.object.object == 0 || pinned_native.object.object == 0 {
        return Err(RuntimeConfigError::InvalidImage);
    }
    super::owner_client::validate_runtime_image_paths(
        &client_root.0,
        &protected_shim.path,
        &pinned_native.path,
        program_files,
    )
    .map_err(|_| RuntimeConfigError::InvalidImage)?;
    let policy_sid = service_sid(&raw.policy_sid)?;
    let reader_sid = service_sid(&raw.reader_sid)?;
    let operator_sid = operator_sid(&raw.operator_sid)?;
    let mut sids = HashSet::new();
    for sid in [&policy_sid, &reader_sid, &operator_sid] {
        unique(&mut sids, sid.value())?;
    }
    if owner_sid != policy_sid.value() {
        return Err(RuntimeConfigError::OwnerMismatch);
    }
    let gateway_port = u16::try_from(raw.gateway_port)
        .ok()
        .and_then(NonZeroU16::new)
        .ok_or(RuntimeConfigError::InvalidPort)?;
    // Native session IDs share this stable installation_id / Owner A namespace.
    // Source keys and runtime generations never create independent owners.
    let (mut keys, mut canonicals, mut sessions, mut threads) = (
        HashSet::new(),
        HashSet::new(),
        HashSet::new(),
        HashSet::new(),
    );
    let (mut roots, mut paths) = (HashSet::new(), HashSet::new());
    let mut sources = Vec::with_capacity(raw.sources.len());
    for source in raw.sources {
        let source_key = SourceKey::parse(&source.source_key)
            .map_err(|_| RuntimeConfigError::InvalidIdentifier)?;
        unique(&mut keys, source_key.as_str())?;
        opaque(&source.canonical_id)?;
        opaque(&source.native_session_id)?;
        unique(&mut canonicals, &source.canonical_id)?;
        unique(&mut sessions, &source.native_session_id)?;
        let canonical_id = CanonicalSessionId::from_protected_catalog(source.canonical_id)
            .map_err(|_| RuntimeConfigError::InvalidIdentifier)?;
        let native_session_id = NativeSessionId::new(source.native_session_id)
            .map_err(|_| RuntimeConfigError::InvalidIdentifier)?;
        if source.native_thread_ids.is_empty() {
            return Err(RuntimeConfigError::InvalidBounds);
        }
        let mut native_thread_ids = Vec::with_capacity(source.native_thread_ids.len());
        for thread in source.native_thread_ids {
            opaque(&thread)?;
            unique(&mut threads, &thread)?;
            native_thread_ids.push(
                NativeThreadId::new(thread).map_err(|_| RuntimeConfigError::InvalidIdentifier)?,
            );
        }
        let generation =
            NonZeroU64::new(source.generation).ok_or(RuntimeConfigError::InvalidBounds)?;
        let runtime_sid = service_sid(&source.runtime_sid)?;
        unique(&mut sids, runtime_sid.value())?;
        if source.root_identity.object == 0 {
            return Err(RuntimeConfigError::InvalidBounds);
        }
        if !roots.insert((source.root_identity.volume, source.root_identity.object)) {
            return Err(RuntimeConfigError::Duplicate);
        }
        let native_home = disk_path(&source.native_home)?;
        let reader_index = disk_path(&source.reader_index)?;
        let runtime_index = disk_path(&source.runtime_index)?;
        let authority = client_root.0.join(".authority");
        if native_home.0
            != authority
                .join("sources")
                .join(source_key.as_str())
                .join("native_home")
            || reader_index.0
                != authority
                    .join("indexes")
                    .join("reader")
                    .join(source_key.as_str())
            || runtime_index.0
                != authority
                    .join("indexes")
                    .join("runtime")
                    .join(source_key.as_str())
                    .join(generation.to_string())
        {
            return Err(RuntimeConfigError::InvalidPath);
        }
        for path in [&native_home.0, &reader_index.0, &runtime_index.0] {
            if !paths.insert(path.clone()) {
                return Err(RuntimeConfigError::Duplicate);
            }
        }
        sources.push(RuntimeSource {
            source_key,
            canonical_id,
            native_session_id,
            native_thread_ids,
            native_home,
            reader_index,
            runtime_index,
            generation,
            runtime_sid,
            root_identity: source.root_identity,
        });
    }
    if let ServiceRole::Runtime {
        source_key,
        generation,
    } = role
    {
        if !sources.iter().any(|source| {
            source.source_key.as_str() == source_key.as_str() && source.generation == generation
        }) {
            return Err(RuntimeConfigError::RoleMismatch);
        }
    }
    Ok(RuntimeData {
        installation_id,
        operator_home,
        client_root,
        protected_shim,
        pinned_native,
        policy_sid,
        reader_sid,
        operator_sid,
        gateway_port,
        sources,
    })
}

#[cfg(test)]
mod tests;
