//! Windows role tokens and fresh storage ACLs. Installation, source sealing,
//! quiescence, and native compatibility qualification belong to the caller.

mod native;
mod principal_storage;
pub(crate) use principal_storage::{FreshPrincipalRoot, PrincipalStoragePolicy};
mod service_principal;
pub(crate) use service_principal::{NormalServiceRole, VerifiedNormalServicePrincipal};
#[cfg(test)]
mod tests;

use super::CanonicalSessionId;
use crate::services::owned_process::OwnedToken;
use std::{fmt, path::Path};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IsolationError {
    Unsupported,
    InvalidInput,
    AuthorityContextRequired,
    AuthorityGroupUnavailable,
    AuthorityProtectionFailed,
    TokenCreationFailed,
    TokenVerificationFailed,
    StorageBoundaryInvalid,
    StorageAccessFailed,
    AncestorWriteGrant,
    FixtureFailed,
}
impl IsolationError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Unsupported => "session_isolation_unsupported",
            Self::InvalidInput => "session_isolation_invalid_input",
            Self::AuthorityContextRequired => "session_isolation_authority_context_required",
            Self::AuthorityGroupUnavailable => "session_isolation_authority_group_unavailable",
            Self::AuthorityProtectionFailed => "session_isolation_authority_protection_failed",
            Self::TokenCreationFailed => "session_isolation_token_creation_failed",
            Self::TokenVerificationFailed => "session_isolation_token_verification_failed",
            Self::StorageBoundaryInvalid => "session_isolation_storage_boundary_invalid",
            Self::StorageAccessFailed => "session_isolation_storage_access_failed",
            Self::AncestorWriteGrant => "session_isolation_ancestor_write_grant",
            Self::FixtureFailed => "session_isolation_fixture_failed",
        }
    }
}
impl fmt::Display for IsolationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}
impl std::error::Error for IsolationError {}

/// Validated, canonical native SID text. It is deliberately not a wire DTO.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Sid {
    value: String,
}
impl Sid {
    pub fn parse(value: &str) -> Result<Self, IsolationError> {
        native::validate_sid(value)
    }
    pub fn value(&self) -> &str {
        &self.value
    }
}

pub(crate) struct SourceCapabilities {
    source_id: CanonicalSessionId,
    generation: u64,
    read: Sid,
    write: Sid,
}
impl SourceCapabilities {
    /// Generation must come from the persisted storage-owner fence. Never
    /// reuse an old generation when granting write access again.
    pub fn derive(source_id: &CanonicalSessionId, generation: u64) -> Result<Self, IsolationError> {
        // Windows capability names may normalize case. Encode the opaque ID
        // bytes so distinct canonical identities cannot alias through casing.
        let source_key: String = source_id
            .as_str()
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Ok(Self {
            source_id: source_id.clone(),
            generation,
            read: native::derive_sid(&format!(
                "aag.session.{source_key}.generation.{generation}.read"
            ))?,
            write: native::derive_sid(&format!(
                "aag.session.{source_key}.generation.{generation}.write"
            ))?,
        })
    }
    pub fn read_sid(&self) -> &Sid {
        &self.read
    }
    pub fn write_sid(&self) -> &Sid {
        &self.write
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CoreRole {
    ReadOnly,
    ReadWrite,
}

/// Privilege-free caller-derived primary token, never serialized or cloned.
pub(crate) struct RoleToken {
    token: OwnedToken,
    role: CoreRole,
    source_id: CanonicalSessionId,
    generation: u64,
}
impl RoleToken {
    pub fn token(&self) -> &OwnedToken {
        &self.token
    }
    pub fn role(&self) -> CoreRole {
        self.role
    }
    pub fn source_id(&self) -> &CanonicalSessionId {
        &self.source_id
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

pub(crate) struct CallerAuthority {
    inner: native::Authority,
}
impl CallerAuthority {
    /// Capture only the exact expected virtual service user in Session 0.
    /// This does not establish RPC/operator authorization.
    pub fn capture_service(expected_owner_sid: &Sid) -> Result<Self, IsolationError> {
        Ok(Self {
            inner: native::Authority::capture_service(expected_owner_sid)?,
        })
    }
    pub fn owner_sid(&self) -> &Sid {
        self.inner.owner_sid()
    }
    /// Call during isolated service startup, before accepting untrusted work.
    /// Never call on a manager process or the global test harness.
    pub fn protect_current_objects(&mut self) -> Result<(), IsolationError> {
        self.inner.protect_current_objects()
    }
    pub fn restrict(
        &self,
        source: &SourceCapabilities,
        role: CoreRole,
    ) -> Result<RoleToken, IsolationError> {
        Ok(RoleToken {
            token: self.inner.restrict(source, role)?,
            role,
            source_id: source.source_id.clone(),
            generation: source.generation,
        })
    }
}

/// ACL for fresh owner-A objects with an explicitly admitted runtime-B user.
/// Applying it cannot revoke already-open handles or prove native quiescence.
pub(crate) struct RoleStorageAcl {
    owner: Sid,
    runtime: Sid,
    write_cap: Sid,
    sddl: String,
    fixture_same_owner: bool,
}
impl RoleStorageAcl {
    pub fn new(
        owner: &Sid,
        runtime: &Sid,
        source: &SourceCapabilities,
        writable: bool,
    ) -> Result<Self, IsolationError> {
        // Production storage identities are distinct virtual-service users.
        // In particular, World, logon, and WriteRestrictedCode may never be
        // admitted as a normal source owner/runtime grant.
        let virtual_service = |sid: &Sid| {
            sid.value().strip_prefix("S-1-5-80-").is_some_and(|suffix| {
                let parts: Vec<_> = suffix.split('-').collect();
                parts.len() == 5 && parts.iter().all(|part| part.parse::<u32>().is_ok())
            })
        };
        if owner == runtime || !virtual_service(owner) || !virtual_service(runtime) {
            return Err(IsolationError::StorageBoundaryInvalid);
        }
        Self::build(owner, runtime, source, writable, false)
    }
    fn build(
        owner: &Sid,
        runtime: &Sid,
        source: &SourceCapabilities,
        writable: bool,
        fixture_same_owner: bool,
    ) -> Result<Self, IsolationError> {
        // These masks exclude WRITE_DAC and WRITE_OWNER. OWNER_RIGHTS also
        // suppresses implicit owner WRITE_DAC in the same-user fixture.
        let runtime_mask = if writable { "0x001301ff" } else { "GRGX" };
        let mut sddl = format!(
            "O:{}D:P(A;OICI;FA;;;{})(A;OICI;RC;;;OW)",
            owner.value(),
            owner.value()
        );
        if owner != runtime {
            sddl.push_str(&format!("(A;OICI;{};;;{})", runtime_mask, runtime.value()));
        }
        sddl.push_str(&format!("(A;OICI;GRGX;;;{})", source.read_sid().value()));
        if writable {
            sddl.push_str(&format!(
                "(A;OICI;0x001301ff;;;{})",
                source.write_sid().value()
            ));
        }
        native::validate_descriptor(&sddl)?;
        Ok(Self {
            owner: owner.clone(),
            runtime: runtime.clone(),
            write_cap: source.write_sid().clone(),
            sddl,
            fixture_same_owner,
        })
    }
    pub fn sddl(&self) -> &str {
        &self.sddl
    }
    /// Call only for fresh A-owned objects while native writers are fenced.
    /// Copy/seal and promotion remain the storage owner's responsibility.
    pub fn apply_tree(
        &self,
        authority: &CallerAuthority,
        root: &Path,
    ) -> Result<usize, IsolationError> {
        native::apply_tree(self, &authority.inner, root)
    }
}
