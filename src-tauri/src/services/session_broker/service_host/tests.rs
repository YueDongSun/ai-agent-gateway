use super::*;

const POLICY_SID: &str = "S-1-5-80-1-2-3-4-5";
const RUNTIME_SID: &str = "S-1-5-80-6-7-8-9-10";

fn observed(user_sid: &str, session_id: u32) -> ObservedIdentity {
    ObservedIdentity {
        user_sid: user_sid.into(),
        session_id,
        privileges: vec!["SeChangeNotifyPrivilege".into()],
        administrator_member: false,
    }
}

fn verified_machine() -> ServiceMachine {
    let mut machine = ServiceMachine::new();
    machine
        .verify_identity(ServiceRole::Policy, &observed(POLICY_SID, 0), POLICY_SID)
        .unwrap();
    machine
}

#[test]
fn roles_select_only_fixed_authority_reader_and_generation_bound_runtime_accounts() {
    assert_eq!(ServiceRole::Policy.service_name(), "aag_session_authority");
    assert_eq!(ServiceRole::Reader.service_name(), "aag_session_reader");
    let runtime = ServiceRole::runtime("0123456789abcdef0123456789abcdef", 1).unwrap();
    assert_eq!(
        runtime.service_name(),
        "aag_session_runtime_0123456789abcdef0123456789abcdef_g1"
    );
    assert_ne!(ServiceRole::Policy.account_name(), runtime.account_name());
    assert_ne!(ServiceRole::Reader.account_name(), runtime.account_name());
    assert_ne!(
        runtime.service_name(),
        ServiceRole::runtime("0123456789abcdef0123456789abcdef", 2)
            .unwrap()
            .service_name()
    );
    assert!(
        ServiceRole::runtime("0123456789abcdef0123456789abcdef", u64::MAX)
            .unwrap()
            .service_name()
            .len()
            <= 256
    );
}

#[test]
fn runtime_role_rejects_arbitrary_names_path_characters_and_zero_generations() {
    for value in [
        "",
        "source",
        "0123456789ABCDEF0123456789ABCDEF",
        "0123456789abcdef0123456789abcde/",
        "0123456789abcdef0123456789abcdefx",
    ] {
        assert_eq!(
            ServiceRole::runtime(value, 1),
            Err(ServiceHostError::RoleInvalid)
        );
    }
    assert_eq!(
        ServiceRole::runtime("0123456789abcdef0123456789abcdef", 0),
        Err(ServiceHostError::RoleInvalid)
    );
}

#[test]
fn correct_service_user_still_requires_nonadmin_allowlisted_privileges() {
    let mut privileged = observed(POLICY_SID, 0);
    privileged.privileges.push("SeImpersonatePrivilege".into());
    assert_eq!(
        ServiceMachine::new().verify_identity(ServiceRole::Policy, &privileged, POLICY_SID),
        Err(ServiceHostError::PrivilegeForbidden)
    );
    // Attribute enablement is deliberately absent from the admission model:
    // a disabled extra privilege is also rejected instead of re-enableable.
    privileged.privileges = vec!["SeDebugPrivilege".into()];
    assert_eq!(
        ServiceMachine::new().verify_identity(ServiceRole::Policy, &privileged, POLICY_SID),
        Err(ServiceHostError::PrivilegeForbidden)
    );
    let mut administrator = observed(POLICY_SID, 0);
    administrator.administrator_member = true;
    assert_eq!(
        ServiceMachine::new().verify_identity(ServiceRole::Policy, &administrator, POLICY_SID),
        Err(ServiceHostError::AdministratorForbidden)
    );
}

#[test]
fn policy_accepts_exact_backup_restore_security_notify_privilege_whitelist() {
    let mut policy = observed(POLICY_SID, 0);
    policy.privileges = vec![
        "SeChangeNotifyPrivilege".into(),
        "SeBackupPrivilege".into(),
        "SeRestorePrivilege".into(),
        "SeSecurityPrivilege".into(),
    ];
    let mut machine = ServiceMachine::new();
    machine
        .verify_identity(ServiceRole::Policy, &policy, POLICY_SID)
        .unwrap();
    machine.ready(false).unwrap();
    for forbidden in [
        "SeImpersonatePrivilege",
        "SeDebugPrivilege",
        "SeTcbPrivilege",
        "SeAssignPrimaryTokenPrivilege",
        "SeTakeOwnershipPrivilege",
        "SeCreateGlobalPrivilege",
    ] {
        let mut invalid = policy.clone();
        invalid.privileges.push(forbidden.into());
        assert_eq!(
            ServiceMachine::new().verify_identity(ServiceRole::Policy, &invalid, POLICY_SID),
            Err(ServiceHostError::PrivilegeForbidden)
        );
    }
}

#[test]
fn reader_and_every_runtime_generation_reject_policy_backup_restore_security_privileges() {
    for role in [
        ServiceRole::Reader,
        ServiceRole::runtime("0123456789abcdef0123456789abcdef", 1).unwrap(),
        ServiceRole::runtime("0123456789abcdef0123456789abcdef", 2).unwrap(),
    ] {
        ServiceMachine::new()
            .verify_identity(role, &observed(RUNTIME_SID, 0), RUNTIME_SID)
            .unwrap();
        for extra in [
            "SeBackupPrivilege",
            "SeRestorePrivilege",
            "SeSecurityPrivilege",
        ] {
            let mut invalid = observed(RUNTIME_SID, 0);
            invalid.privileges.push(extra.into());
            assert_eq!(
                ServiceMachine::new().verify_identity(role, &invalid, RUNTIME_SID),
                Err(ServiceHostError::PrivilegeForbidden)
            );
        }
    }
}

#[test]
fn running_requires_verified_token_user_session_zero_and_explicit_business_ready() {
    let mut machine = ServiceMachine::new();
    assert!(machine.ready(false).is_err());
    assert_eq!(machine.phase, ServicePhase::StartPending);
    machine
        .verify_identity(ServiceRole::Policy, &observed(POLICY_SID, 0), POLICY_SID)
        .unwrap();
    assert_eq!(machine.phase, ServicePhase::StartPending);
    machine.ready(false).unwrap();
    assert_eq!(machine.phase, ServicePhase::Running);
}

#[test]
fn matching_service_group_cannot_substitute_for_the_normal_token_user() {
    let mut machine = ServiceMachine::new();
    // The expected service SID can be present in TokenGroups. Only TokenUser
    // is supplied as the observed identity, so the interactive owner is denied.
    assert_eq!(
        machine.verify_identity(
            ServiceRole::Policy,
            &observed("S-1-5-21-100-200-300-1001", 0),
            POLICY_SID
        ),
        Err(ServiceHostError::IdentityMismatch)
    );
    assert!(machine.ready(false).is_err());
    assert_eq!(
        machine.verify_identity(ServiceRole::Policy, &observed(RUNTIME_SID, 0), POLICY_SID),
        Err(ServiceHostError::IdentityMismatch)
    );
}

#[test]
fn correct_account_in_an_interactive_session_cannot_report_running() {
    let mut machine = ServiceMachine::new();
    assert_eq!(
        machine.verify_identity(ServiceRole::Policy, &observed(POLICY_SID, 1), POLICY_SID),
        Err(ServiceHostError::SessionNotZero)
    );
    assert!(machine.ready(false).is_err());
}

#[test]
fn stop_cancels_late_readiness_and_is_pending_until_business_returns() {
    let mut machine = verified_machine();
    machine.request_stop(Instant::now()).unwrap();
    assert_eq!(machine.phase, ServicePhase::StopPending);
    assert_eq!(machine.ready(true), Err(ServiceHostError::StopRequested));
    machine.finish(Ok(())).unwrap();
    assert_eq!(machine.phase, ServicePhase::Stopped);
}

#[test]
fn cleanup_timeout_keeps_stop_pending_instead_of_claiming_business_stopped() {
    let mut machine = verified_machine();
    machine.ready(false).unwrap();
    let started = Instant::now();
    machine.request_stop(started).unwrap();
    assert_eq!(
        machine.check_cleanup_deadline(started + CLEANUP_TIMEOUT),
        Err(ServiceHostError::CleanupTimeout)
    );
    assert_eq!(machine.phase, ServicePhase::StopPending);
}

#[test]
fn failed_startup_reports_failure_and_never_claims_running() {
    let mut machine = verified_machine();
    assert_eq!(
        machine.finish(Ok(())),
        Err(ServiceHostError::BusinessNotReady)
    );
    assert_eq!(machine.phase, ServicePhase::Stopped);
    assert_eq!(machine.failure, Some(ServiceHostError::BusinessNotReady));
}

#[derive(Default)]
struct RecordingReporter {
    statuses: Mutex<Vec<StatusSnapshot>>,
    fail_running: AtomicBool,
}

impl StatusReporter for RecordingReporter {
    fn report(&self, status: StatusSnapshot) -> Result<(), ServiceHostError> {
        if status.phase == ServicePhase::Running && self.fail_running.load(Ordering::SeqCst) {
            return Err(ServiceHostError::StatusReportFailed);
        }
        self.statuses.lock().unwrap().push(status);
        Ok(())
    }
}

fn context(started: Instant) -> (ServiceContext, Arc<RecordingReporter>) {
    let reporter = Arc::new(RecordingReporter::default());
    let context = ServiceContext {
        control: Arc::new(ServiceControl {
            role: ServiceRole::Policy,
            cancelled: AtomicBool::new(false),
            machine: Mutex::new(verified_machine()),
            started,
            stop_elapsed_ms: std::sync::atomic::AtomicU64::new(u64::MAX),
            callback_panicked: AtomicBool::new(false),
            reporter: reporter.clone(),
        }),
    };
    (context, reporter)
}

#[test]
fn repeated_stop_does_not_extend_the_cleanup_deadline() {
    let mut machine = verified_machine();
    let started = Instant::now();
    machine.request_stop(started).unwrap();
    machine.request_stop(started + CLEANUP_TIMEOUT).unwrap();
    assert_eq!(
        machine.check_cleanup_deadline(started + CLEANUP_TIMEOUT),
        Err(ServiceHostError::CleanupTimeout)
    );
    assert_eq!(machine.phase, ServicePhase::StopPending);
}

#[test]
fn readiness_failure_closes_admission_without_claiming_running() {
    let (context, reporter) = context(Instant::now());
    reporter.fail_running.store(true, Ordering::SeqCst);
    assert_eq!(context.ready(), Err(ServiceHostError::StatusReportFailed));
    assert!(context.is_cancelled());
    assert_eq!(context.phase().unwrap(), ServicePhase::StopPending);
    assert!(reporter.statuses.lock().unwrap().is_empty());
}

#[test]
fn late_business_readiness_times_out_and_cannot_reopen_a_stopped_service() {
    let (context, reporter) = context(Instant::now() - STARTUP_TIMEOUT);
    assert_eq!(context.ready(), Err(ServiceHostError::StartupTimeout));
    assert_eq!(context.phase().unwrap(), ServicePhase::StopPending);
    assert!(context.is_cancelled());
    assert!(reporter
        .statuses
        .lock()
        .unwrap()
        .iter()
        .all(|status| status.phase != ServicePhase::Running));
    let mut machine = context.control.machine.lock().unwrap();
    assert_eq!(
        machine.finish(Ok(())),
        Err(ServiceHostError::StartupTimeout)
    );
    assert_eq!(machine.finish(Ok(())), Err(ServiceHostError::StateInvalid));
}

#[test]
fn business_panic_retains_stop_pending_until_the_dedicated_process_exits() {
    let mut machine = verified_machine();
    machine.ready(false).unwrap();
    assert_eq!(
        machine.finish(Err(ServiceHostError::BusinessPanicked)),
        Err(ServiceHostError::BusinessPanicked)
    );
    assert_eq!(machine.phase, ServicePhase::StopPending);
}

#[cfg(windows)]
#[test]
fn stop_shutdown_and_interrogate_have_bounded_flag_only_callbacks() {
    let (context, _) = context(Instant::now());
    assert_eq!(native::apply_control(&context.control, 4), 0);
    assert!(!context.is_cancelled());
    assert_eq!(native::apply_control(&context.control, 999), 120);
    assert!(!context.is_cancelled());
    assert_eq!(native::apply_control(&context.control, 1), 0);
    assert!(context.is_cancelled());
    assert_eq!(context.ready(), Err(ServiceHostError::StopRequested));
    let (shutdown_context, _) = self::context(Instant::now());
    assert_eq!(native::apply_control(&shutdown_context.control, 5), 0);
    assert!(shutdown_context.wait_cancelled(Duration::ZERO));
}

#[cfg(windows)]
#[test]
fn native_status_accepts_controls_only_after_verified_business_readiness() {
    for phase in [
        ServicePhase::StartPending,
        ServicePhase::StopPending,
        ServicePhase::Stopped,
    ] {
        assert_eq!(
            native::status_for(StatusSnapshot {
                phase,
                checkpoint: 0,
                failure: None
            })
            .controls_accepted,
            0
        );
    }
    let running = native::status_for(StatusSnapshot {
        phase: ServicePhase::Running,
        checkpoint: 0,
        failure: None,
    });
    assert_eq!(running.controls_accepted, 0x1 | 0x4);
    let failed = native::status_for(StatusSnapshot {
        phase: ServicePhase::Stopped,
        checkpoint: 0,
        failure: Some(ServiceHostError::BusinessNotReady),
    });
    assert_eq!(failed.win32_exit_code, 1066);
    assert_ne!(failed.service_specific_exit_code, 0);
}

#[cfg(windows)]
#[test]
fn cooperative_business_cleanup_reports_stopped_once_after_the_worker_returns() {
    fn business(context: ServiceContext) -> Result<(), ServiceHostError> {
        context.ready()?;
        context.request_stop();
        Ok(())
    }
    let (context, reporter) = context(Instant::now());
    native::supervise(&context.control, business).unwrap();
    let phases: Vec<_> = reporter
        .statuses
        .lock()
        .unwrap()
        .iter()
        .map(|status| status.phase)
        .collect();
    assert_eq!(
        phases,
        vec![
            ServicePhase::Running,
            ServicePhase::StopPending,
            ServicePhase::Stopped
        ]
    );
}

#[cfg(windows)]
#[test]
fn supervised_business_panic_is_contained_without_a_successful_stopped_status() {
    fn business(context: ServiceContext) -> Result<(), ServiceHostError> {
        context.ready()?;
        panic!("synthetic_service_business_panic");
    }
    let (context, reporter) = context(Instant::now());
    assert_eq!(
        native::supervise(&context.control, business),
        Err(ServiceHostError::BusinessPanicked)
    );
    assert_eq!(context.phase().unwrap(), ServicePhase::StopPending);
    assert!(reporter
        .statuses
        .lock()
        .unwrap()
        .iter()
        .all(|status| status.phase != ServicePhase::Stopped));
}
