//! Normal service identities for the distinct A/B-generation/C architecture.
//! This proves the current Windows caller, not installation, source ownership,
//! namespace ACLs, a SID non-reuse ledger, or the trusted reader's method policy.

use super::native::{
    ffi::{
        GetCurrentProcess, GetCurrentProcessId, GetLastError, Handle, IsTokenRestricted,
        LookupPrivilegeValueW, Luid, OpenProcessToken,
    },
    sid::wide,
    token::{own, token_groups, token_information, token_privileges, token_user},
};
use super::{CanonicalSessionId, IsolationError, Sid};
use std::{
    os::windows::io::{AsRawHandle, OwnedHandle},
    ptr::{null, null_mut},
};

const GROUP_ENABLED: u32 = 0x4;
const GROUP_DENY_ONLY: u32 = 0x10;
const GROUP_LOGON_ID: u32 = 0xc000_0000;
const CHANGE_NOTIFY_PRIVILEGE: &str = "SeChangeNotifyPrivilege";

#[derive(Clone)]
pub(crate) enum NormalServiceRole {
    RuntimeGeneration {
        source_id: CanonicalSessionId,
        physical_generation: u64,
    },
    TrustedReader,
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(C)]
struct TokenLuid {
    low: u32,
    high: i32,
}

#[derive(Clone, Copy)]
#[repr(C)]
struct TokenStatistics {
    token_id: TokenLuid,
    authentication_id: TokenLuid,
    expiration_time: i64,
    token_type: u32,
    impersonation_level: u32,
    dynamic_charged: u32,
    dynamic_available: u32,
    group_count: u32,
    privilege_count: u32,
    modified_id: TokenLuid,
}

#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentThread() -> Handle;
}
#[link(name = "advapi32")]
extern "system" {
    fn OpenThreadToken(thread: Handle, access: u32, open_as_self: i32, token: *mut Handle) -> i32;
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct TokenBinding {
    process_id: u32,
    token_id: TokenLuid,
    authentication_id: TokenLuid,
}

#[derive(Clone)]
struct Observation {
    binding: TokenBinding,
    user: Sid,
    session_id: u32,
    token_type: u32,
    restricted: bool,
    restricting_sids: usize,
    groups: Vec<(Sid, u32)>,
    privileges: Vec<(TokenLuid, u32)>,
    change_notify_luid: TokenLuid,
    thread_impersonating: bool,
}

/// A process-local, token-bound identity proof. It cannot be deserialized from
/// IPC or constructed from a claimed service group, PID, or source-name string.
/// The owner-protected catalog must bind CanonicalSessionId, SourceKey, physical
/// generation, and the never-reused expected service SID before capture.
pub(crate) struct VerifiedNormalServicePrincipal {
    service_sid: Sid,
    storage_owner_sid: Sid,
    role: NormalServiceRole,
    binding: TokenBinding,
    token: OwnedHandle,
}

impl VerifiedNormalServicePrincipal {
    pub fn capture_current(
        expected_service_sid: &Sid,
        storage_owner_sid: &Sid,
        role: NormalServiceRole,
    ) -> Result<Self, IsolationError> {
        validate_service_users(expected_service_sid, storage_owner_sid)?;
        let (token, observed) = observe_current()?;
        validate_observation(expected_service_sid, storage_owner_sid, &observed)?;
        Ok(Self {
            service_sid: expected_service_sid.clone(),
            storage_owner_sid: storage_owner_sid.clone(),
            role,
            binding: observed.binding,
            token,
        })
    }

    pub fn role(&self) -> &NormalServiceRole {
        &self.role
    }
    pub fn service_sid(&self) -> &Sid {
        &self.service_sid
    }
    pub fn source_id(&self) -> Option<&CanonicalSessionId> {
        match &self.role {
            NormalServiceRole::RuntimeGeneration { source_id, .. } => Some(source_id),
            NormalServiceRole::TrustedReader => None,
        }
    }
    pub fn physical_generation(&self) -> Option<u64> {
        match &self.role {
            NormalServiceRole::RuntimeGeneration {
                physical_generation,
                ..
            } => Some(*physical_generation),
            NormalServiceRole::TrustedReader => None,
        }
    }
    pub fn process_id(&self) -> u32 {
        self.binding.process_id
    }

    /// Call immediately before each inherited-identity NativeChild spawn.
    /// No caller may replace a failed proof with an unqualified None-token path.
    pub fn verify_current(&self) -> Result<(), IsolationError> {
        if self.binding.process_id != unsafe { GetCurrentProcessId() } {
            return Err(IsolationError::AuthorityContextRequired);
        }
        let retained = statistics(self.token.as_raw_handle())?;
        validate_binding(
            self.binding,
            TokenBinding {
                process_id: self.binding.process_id,
                token_id: retained.token_id,
                authentication_id: retained.authentication_id,
            },
        )?;
        let (_token, observed) = observe_current()?;
        validate_observation(&self.service_sid, &self.storage_owner_sid, &observed)?;
        validate_binding(self.binding, observed.binding)
    }
}

fn virtual_service_sid(sid: &Sid) -> bool {
    sid.value().strip_prefix("S-1-5-80-").is_some_and(|suffix| {
        let parts: Vec<_> = suffix.split('-').collect();
        parts.len() == 5 && parts.iter().all(|part| part.parse::<u32>().is_ok())
    })
}

fn validate_service_users(expected: &Sid, storage_owner: &Sid) -> Result<(), IsolationError> {
    if expected == storage_owner
        || !virtual_service_sid(expected)
        || !virtual_service_sid(storage_owner)
    {
        return Err(IsolationError::AuthorityContextRequired);
    }
    Ok(())
}

fn validate_observation(
    expected: &Sid,
    storage_owner: &Sid,
    observed: &Observation,
) -> Result<(), IsolationError> {
    validate_service_users(expected, storage_owner)?;
    if observed.binding.process_id == 0
        || observed.user != *expected
        || observed.session_id != 0
        || observed.token_type != 1
        || observed.restricted
        || observed.restricting_sids != 0
        || observed.thread_impersonating
        || observed
            .groups
            .iter()
            .any(|(sid, _)| sid.value() == "S-1-5-32-544")
        || observed
            .groups
            .iter()
            .any(|(sid, _)| virtual_service_sid(sid) && sid != expected)
    {
        return Err(IsolationError::AuthorityContextRequired);
    }

    let mut logons = observed
        .groups
        .iter()
        .filter(|(_, attributes)| attributes & GROUP_LOGON_ID == GROUP_LOGON_ID);
    let Some((logon, attributes)) = logons.next() else {
        return Err(IsolationError::AuthorityContextRequired);
    };
    if logons.next().is_some()
        || !logon.value().starts_with("S-1-5-5-")
        || attributes & GROUP_ENABLED == 0
        || attributes & GROUP_DENY_ONLY != 0
        || !observed.groups.iter().any(|(sid, attributes)| {
            sid.value() == "S-1-5-6"
                && attributes & GROUP_ENABLED != 0
                && attributes & GROUP_DENY_ONLY == 0
        })
        || observed.privileges.len() > 1
        || observed.privileges.iter().any(|(luid, attributes)| {
            *luid != observed.change_notify_luid || attributes & !0x8000_0003 != 0
        })
    {
        return Err(IsolationError::AuthorityContextRequired);
    }
    Ok(())
}

fn validate_binding(captured: TokenBinding, current: TokenBinding) -> Result<(), IsolationError> {
    if captured.process_id == 0 || captured != current {
        return Err(IsolationError::AuthorityContextRequired);
    }
    Ok(())
}

fn statistics(token: Handle) -> Result<TokenStatistics, IsolationError> {
    let buffer = token_information(token, 10)?;
    if buffer.len() * std::mem::size_of::<usize>() < std::mem::size_of::<TokenStatistics>() {
        return Err(IsolationError::TokenVerificationFailed);
    }
    Ok(unsafe { buffer.as_ptr().cast::<TokenStatistics>().read_unaligned() })
}

fn token_dword(token: Handle, class: u32) -> Result<u32, IsolationError> {
    let buffer = token_information(token, class)?;
    if buffer.len() * std::mem::size_of::<usize>() < std::mem::size_of::<u32>() {
        return Err(IsolationError::TokenVerificationFailed);
    }
    Ok(unsafe { buffer.as_ptr().cast::<u32>().read_unaligned() })
}

fn thread_impersonating() -> Result<bool, IsolationError> {
    let mut token = null_mut();
    // OpenAsSelf also detects identification-level tokens. Access denial and
    // anonymous-token errors never become a false "no impersonation" result.
    if unsafe { OpenThreadToken(GetCurrentThread(), 0x8, 1, &mut token) } != 0 {
        drop(own(token, IsolationError::TokenVerificationFailed)?);
        return Ok(true);
    }
    if unsafe { GetLastError() } == 1008 {
        return Ok(false);
    }
    Err(IsolationError::AuthorityContextRequired)
}

fn observe_current() -> Result<(OwnedHandle, Observation), IsolationError> {
    let impersonating = thread_impersonating()?;
    if impersonating {
        return Err(IsolationError::AuthorityContextRequired);
    }
    let mut allowed = Luid { low: 0, high: 0 };
    if unsafe {
        LookupPrivilegeValueW(
            null(),
            wide(CHANGE_NOTIFY_PRIVILEGE)?.as_ptr(),
            &mut allowed,
        )
    } == 0
    {
        return Err(IsolationError::TokenVerificationFailed);
    }
    let mut handle = null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), 0x8, &mut handle) } == 0 {
        return Err(IsolationError::AuthorityContextRequired);
    }
    let token = own(handle, IsolationError::AuthorityContextRequired)?;
    let before = statistics(token.as_raw_handle())?;
    let groups = token_groups(token.as_raw_handle(), 2)?;
    let privileges = token_privileges(token.as_raw_handle())?
        .into_iter()
        .map(|value| {
            (
                TokenLuid {
                    low: value.luid.low,
                    high: value.luid.high,
                },
                value.attributes,
            )
        })
        .collect::<Vec<_>>();
    let user = token_user(token.as_raw_handle())?;
    let session_id = token_dword(token.as_raw_handle(), 12)?;
    let restricting_sids = token_groups(token.as_raw_handle(), 11)?.len();
    let restricted = unsafe { IsTokenRestricted(token.as_raw_handle()) } != 0;
    let after = statistics(token.as_raw_handle())?;
    if before.token_id != after.token_id
        || before.authentication_id != after.authentication_id
        || before.modified_id != after.modified_id
        || before.token_type != after.token_type
        || groups.len() != after.group_count as usize
        || privileges.len() != after.privilege_count as usize
    {
        return Err(IsolationError::TokenVerificationFailed);
    }
    // Do not use TokenHasRestrictions here. SCM may legitimately have removed
    // privileges while keeping the exact normal service user and logon groups.
    let observed = Observation {
        binding: TokenBinding {
            process_id: unsafe { GetCurrentProcessId() },
            token_id: after.token_id,
            authentication_id: after.authentication_id,
        },
        user,
        session_id,
        token_type: after.token_type,
        restricted,
        restricting_sids,
        groups,
        privileges,
        change_notify_luid: TokenLuid {
            low: allowed.low,
            high: allowed.high,
        },
        thread_impersonating: impersonating,
    };
    Ok((token, observed))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sid(value: &str) -> Sid {
        Sid::parse(value).unwrap()
    }
    fn runtime() -> Sid {
        sid("S-1-5-80-1-2-3-4-5")
    }
    fn owner() -> Sid {
        sid("S-1-5-80-6-7-8-9-10")
    }
    fn observed() -> Observation {
        let change_notify = TokenLuid { low: 23, high: 0 };
        Observation {
            binding: TokenBinding {
                process_id: 100,
                token_id: TokenLuid { low: 200, high: 0 },
                authentication_id: TokenLuid { low: 300, high: 0 },
            },
            user: runtime(),
            session_id: 0,
            token_type: 1,
            restricted: false,
            restricting_sids: 0,
            groups: vec![
                (sid("S-1-5-5-10-20"), GROUP_LOGON_ID | GROUP_ENABLED),
                (sid("S-1-5-6"), GROUP_ENABLED),
            ],
            privileges: vec![(change_notify, 3)],
            change_notify_luid: change_notify,
            thread_impersonating: false,
        }
    }
    fn rejected(observed: &Observation) {
        assert!(matches!(
            validate_observation(&runtime(), &owner(), observed),
            Err(IsolationError::AuthorityContextRequired)
        ));
    }

    #[test]
    fn accepts_normal_service_after_scm_privilege_stripping() {
        let mut value = observed();
        assert!(validate_observation(&runtime(), &owner(), &value).is_ok());
        value.privileges.clear();
        // Filtering history alone is deliberately not an admission condition.
        // Actual current groups, privileges, and restricting SIDs are checked.
        assert!(validate_observation(&runtime(), &owner(), &value).is_ok());
    }
    #[test]
    fn requires_exact_service_token_user_not_a_service_group() {
        let mut value = observed();
        value.groups.push((runtime(), GROUP_ENABLED));
        value.user = sid("S-1-5-21-1-2-3-1001");
        rejected(&value);
    }
    #[test]
    fn rejects_foreign_virtual_service_groups_even_when_deny_only() {
        let mut baseline = observed();
        baseline.groups.push((runtime(), GROUP_ENABLED));
        baseline.groups.push((sid("S-1-5-80-0"), GROUP_ENABLED));
        assert!(
            validate_observation(&runtime(), &owner(), &baseline).is_ok(),
            "the current service and ordinary AllServices groups must remain admissible"
        );
        for foreign in [
            owner(),
            sid("S-1-5-80-11-12-13-14-15"),
            sid("S-1-5-80-16-17-18-19-20"),
        ] {
            for attributes in [GROUP_ENABLED, GROUP_DENY_ONLY] {
                let mut value = baseline.clone();
                value.groups.push((foreign.clone(), attributes));
                rejected(&value);
            }
        }
    }
    #[test]
    fn rejects_interactive_admin_impersonating_and_nonprimary_contexts() {
        let baseline = observed();
        let mut value = baseline.clone();
        value.session_id = 1;
        rejected(&value);
        let mut value = baseline.clone();
        value.token_type = 2;
        rejected(&value);
        let mut value = baseline.clone();
        value.thread_impersonating = true;
        rejected(&value);
        for attributes in [GROUP_ENABLED, GROUP_DENY_ONLY] {
            let mut value = baseline.clone();
            value.groups.push((sid("S-1-5-32-544"), attributes));
            rejected(&value);
        }
    }
    #[test]
    fn rejects_restricting_sids_and_disabled_or_missing_service_logon() {
        let baseline = observed();
        let mut value = baseline.clone();
        value.restricted = true;
        rejected(&value);
        let mut value = baseline.clone();
        value.restricting_sids = 1;
        rejected(&value);
        let mut value = baseline.clone();
        value.groups[0].1 = GROUP_LOGON_ID | GROUP_DENY_ONLY;
        rejected(&value);
        let mut value = baseline.clone();
        value.groups.remove(0);
        rejected(&value);
        let mut value = baseline.clone();
        value.groups.remove(1);
        rejected(&value);
    }
    #[test]
    fn rejects_all_privileges_except_change_notify() {
        let mut value = observed();
        value.privileges.push((TokenLuid { low: 24, high: 0 }, 0));
        rejected(&value);
        value.privileges = vec![(TokenLuid { low: 999, high: 0 }, 2)];
        rejected(&value);
    }
    #[test]
    fn requires_distinct_complete_virtual_service_users() {
        let value = observed();
        assert!(matches!(
            validate_observation(&runtime(), &runtime(), &value),
            Err(IsolationError::AuthorityContextRequired)
        ));
        for invalid in ["S-1-1-0", "S-1-5-33", "S-1-5-80-1-2-3-4"] {
            assert!(matches!(
                validate_observation(&sid(invalid), &owner(), &value),
                Err(IsolationError::AuthorityContextRequired)
            ));
            assert!(matches!(
                validate_observation(&runtime(), &sid(invalid), &value),
                Err(IsolationError::AuthorityContextRequired)
            ));
        }
    }
    #[test]
    fn recheck_rejects_pid_primary_token_and_logon_replacement() {
        let captured = observed().binding;
        assert!(validate_binding(captured, captured).is_ok());
        let mut changed = captured;
        changed.process_id += 1;
        assert!(validate_binding(captured, changed).is_err());
        let mut changed = captured;
        changed.token_id.low += 1;
        assert!(validate_binding(captured, changed).is_err());
        let mut changed = captured;
        changed.authentication_id.low += 1;
        assert!(validate_binding(captured, changed).is_err());
    }

    #[test]
    fn current_process_cannot_claim_another_normal_service_identity() {
        // Exercise the actual query-only native inspection path without
        // creating an account, changing a token, or granting a credential.
        let (_token, current) = observe_current().unwrap();
        let expected = if current.user == runtime() {
            sid("S-1-5-80-11-12-13-14-15")
        } else {
            runtime()
        };
        assert!(matches!(
            VerifiedNormalServicePrincipal::capture_current(
                &expected,
                &owner(),
                NormalServiceRole::TrustedReader
            ),
            Err(IsolationError::AuthorityContextRequired)
        ));
    }
}
