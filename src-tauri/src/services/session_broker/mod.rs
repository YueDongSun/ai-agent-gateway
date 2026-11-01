//! Policy and native-reference index for the isolated session broker.
//!
//! This core never stores conversation messages or attachment bytes. The caller
//! supplies an already-open, service-owned database connection and independently
//! verifies OS principals, the native owner, and native quiescence. Passing these
//! tests does not qualify the OS, tool/MCP, transport, or native-client boundary.

#[cfg(windows)]
pub(crate) mod client_dispatch;
#[cfg(windows)]
pub(crate) mod local_transport;
#[cfg(windows)]
pub(crate) mod native_core;
mod policy;
mod registry;
#[cfg(windows)]
pub(crate) mod service_host;
#[cfg(windows)]
pub(crate) mod source_catalog;
#[cfg(windows)]
pub(crate) mod windows_isolation;

pub use registry::SessionRegistry;

use serde_json::Value;

pub const CORE_QUALIFICATION: &str = "policy_core_only_native_enforcement_pending";
const MAX_IDENTIFIER_BYTES: usize = 1024;

#[derive(Debug, thiserror::Error)]
pub enum BrokerError {
    #[error("session_broker_invalid_identifier")]
    InvalidIdentifier,
    #[error("session_broker_invalid_request")]
    InvalidRequest,
    #[error("session_broker_method_forbidden")]
    ForbiddenMethod,
    #[error("session_broker_override_forbidden")]
    ForbiddenOverride,
    #[error("session_broker_unknown_thread")]
    UnknownThread,
    #[error("session_broker_alias_conflict")]
    AliasConflict,
    #[error("session_broker_payload_conflict")]
    PayloadConflict,
    #[error("session_broker_owner_mismatch")]
    OwnerMismatch,
    #[error("session_broker_read_only")]
    ReadOnly,
    #[error("session_broker_busy")]
    Busy,
    #[error("session_broker_stale_fence")]
    StaleFence,
    #[error("session_broker_epoch_exhausted")]
    EpochExhausted,
    #[error("session_broker_registry_lock_failed")]
    LockFailed,
    #[error("session_broker_storage_failed")]
    Storage(#[from] rusqlite::Error),
    #[error("session_broker_random_identity_failed")]
    RandomIdentity,
    #[error("session_broker_schema_unsupported")]
    UnsupportedSchema,
}

/// Exact opaque identifiers: no path interpretation or case folding is applied.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NativeSessionId(String);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NativeThreadId(String);

/// An owner-issued payload object identity, never a filesystem path lookup.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PayloadObjectId(String);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CanonicalSessionId(String);

macro_rules! opaque_identifier {
    ($name:ident) => {
        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, BrokerError> {
                let value = value.into();
                validate_identifier(&value)?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

opaque_identifier!(NativeSessionId);
opaque_identifier!(NativeThreadId);
opaque_identifier!(PayloadObjectId);

impl CanonicalSessionId {
    /// Parse an identifier read from the owner-protected source catalog. This
    /// validates syntax only; transport and source ownership remain separate.
    pub(crate) fn from_protected_catalog(value: impl Into<String>) -> Result<Self, BrokerError> {
        let value = value.into();
        validate_identifier(&value)?;
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Established by the native-owner transport, never deserialized from an RPC.
#[derive(Debug, Clone)]
pub struct VerifiedOwner {
    owner_instance_id: String,
}

impl VerifiedOwner {
    /// The service boundary must verify this stable owner identity first.
    pub(crate) fn from_verified_transport(
        owner_instance_id: impl Into<String>,
    ) -> Result<Self, BrokerError> {
        let owner_instance_id = owner_instance_id.into();
        validate_identifier(&owner_instance_id)?;
        Ok(Self { owner_instance_id })
    }

    pub fn owner_instance_id(&self) -> &str {
        &self.owner_instance_id
    }

    /// Call only after the owner has independently confirmed no native work.
    /// A snapshot or zero broker permits alone does not establish native idle.
    pub(crate) fn attest_native_idle(
        &self,
        snapshot: &SessionSnapshot,
    ) -> Result<IdleAttestation, BrokerError> {
        if self.owner_instance_id != snapshot.owner_instance_id {
            return Err(BrokerError::OwnerMismatch);
        }
        Ok(IdleAttestation {
            owner_instance_id: self.owner_instance_id.clone(),
            canonical_session_id: snapshot.canonical_session_id.clone(),
            epoch: snapshot.epoch,
        })
    }
}

/// A capability created after an OS authorization check; deliberately no Serde.
#[derive(Debug)]
pub struct OperatorAuthorization {
    principal_id: String,
}

impl OperatorAuthorization {
    pub(crate) fn from_verified_os_principal(
        principal_id: impl Into<String>,
    ) -> Result<Self, BrokerError> {
        let principal_id = principal_id.into();
        validate_identifier(&principal_id)?;
        Ok(Self { principal_id })
    }

    pub fn principal_id(&self) -> &str {
        &self.principal_id
    }
}

#[derive(Debug)]
pub struct IdleAttestation {
    owner_instance_id: String,
    canonical_session_id: CanonicalSessionId,
    epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadReference {
    pub canonical_session_id: CanonicalSessionId,
    pub owner_instance_id: String,
    pub native_session_id: NativeSessionId,
    pub native_thread_id: NativeThreadId,
    pub payload_object_id: PayloadObjectId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSnapshot {
    pub canonical_session_id: CanonicalSessionId,
    pub owner_instance_id: String,
    pub native_session_id: NativeSessionId,
    pub read_only: bool,
    pub epoch: u64,
    pub admitted_work: bool,
}

/// Not a native write itself: the owner must enforce this before dispatch and
/// retain the admission until terminal execution evidence has been reconciled.
#[derive(Debug)]
pub struct WritePermit {
    pub reference: ThreadReference,
    pub epoch: u64,
    permit_id: String,
}

#[derive(Debug)]
pub enum BrokerDecision {
    Read {
        reference: Option<ThreadReference>,
    },
    /// Compose a stored-history read; do not forward native thread/resume.
    ReadProjection {
        reference: ThreadReference,
    },
    Write(WritePermit),
}

/// An untrusted RPC envelope. Authority is supplied separately as a typed value.
pub struct ClientRequest<'a> {
    pub method: &'a str,
    pub params: &'a Value,
}

fn validate_identifier(value: &str) -> Result<(), BrokerError> {
    if value.is_empty()
        || value.len() > MAX_IDENTIFIER_BYTES
        || value.trim().is_empty()
        || value.chars().any(char::is_control)
    {
        return Err(BrokerError::InvalidIdentifier);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
