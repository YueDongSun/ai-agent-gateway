//! SCM adapter for one dedicated own-process service. No installer operations.
//! References: Microsoft Learn SetServiceStatus and HandlerEx contracts.

use super::{
    ffi::*, identity, FatalServiceExit, ServiceContext, ServiceControl, ServiceEntry,
    ServiceHostConfig, ServiceHostError, ServiceMachine, ServicePhase, ServiceRole, StatusReporter,
    StatusSnapshot, CANCELLATION_POLL, CLEANUP_TIMEOUT, STARTUP_TIMEOUT,
};
use std::{
    ffi::c_void,
    panic::{catch_unwind, AssertUnwindSafe},
    ptr::null_mut,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc, Arc, Mutex, OnceLock,
    },
    time::Instant,
};

// SCM supplies no application context to ServiceMain. This immutable bootstrap
// is the sole adapter exception: fixed role/function pointers, no broker state.
struct Bootstrap {
    role: ServiceRole,
    entry: ServiceEntry,
    fatal_exit: fn(FatalServiceExit) -> !,
}
static BOOTSTRAP: OnceLock<Bootstrap> = OnceLock::new();

struct NativeReporter {
    handle: AtomicUsize,
}

pub(super) fn status_for(status: StatusSnapshot) -> ServiceStatus {
    let (current_state, controls_accepted, wait_hint) = match status.phase {
        ServicePhase::StartPending => (2, 0, STARTUP_TIMEOUT.as_millis() as u32),
        ServicePhase::Running => (4, 0x1 | 0x4, 0),
        ServicePhase::StopPending => (3, 0, CLEANUP_TIMEOUT.as_millis() as u32),
        ServicePhase::Stopped => (1, 0, 0),
    };
    ServiceStatus {
        service_type: 0x10,
        current_state,
        controls_accepted,
        win32_exit_code: if status.failure.is_some() { 1066 } else { 0 },
        service_specific_exit_code: status.failure.map(|error| error as u32 + 1).unwrap_or(0),
        checkpoint: status.checkpoint,
        wait_hint,
    }
}

impl StatusReporter for NativeReporter {
    fn report(&self, snapshot: StatusSnapshot) -> Result<(), ServiceHostError> {
        let handle = self.handle.load(Ordering::SeqCst) as Handle;
        if handle.is_null() || unsafe { SetServiceStatus(handle, &status_for(snapshot)) } == 0 {
            return Err(ServiceHostError::StatusReportFailed);
        }
        Ok(())
    }
}

pub(super) fn dispatch_native(
    config: ServiceHostConfig,
    entry: ServiceEntry,
) -> Result<(), ServiceHostError> {
    BOOTSTRAP
        .set(Bootstrap {
            role: config.role,
            entry,
            fatal_exit: config.fatal_exit,
        })
        .map_err(|_| ServiceHostError::AlreadyDispatched)?;
    let mut name = identity::wide(&config.role.service_name());
    let table = [
        ServiceTableEntry {
            name: name.as_mut_ptr(),
            service_main: Some(service_main),
        },
        ServiceTableEntry {
            name: null_mut(),
            service_main: None,
        },
    ];
    if unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) } == 0 {
        return Err(ServiceHostError::DispatcherUnavailable);
    }
    Ok(())
}

fn fatal(bootstrap: &Bootstrap, reason: ServiceHostError) -> ! {
    // Even an incorrectly implemented fatal callback cannot unwind over SCM.
    let _ = catch_unwind(AssertUnwindSafe(|| {
        (bootstrap.fatal_exit)(FatalServiceExit { reason })
    }));
    std::process::abort()
}

unsafe extern "system" fn service_main(count: u32, arguments: *mut *mut u16) {
    let Some(bootstrap) = BOOTSTRAP.get() else {
        std::process::abort();
    };
    match catch_unwind(AssertUnwindSafe(|| {
        run_service(bootstrap, count, arguments)
    })) {
        Ok(Ok(())) => {}
        Ok(Err(error)) => fatal(bootstrap, error),
        Err(_) => fatal(bootstrap, ServiceHostError::CallbackPanicked),
    }
}

/// No locks, I/O, worker joins, or business callbacks run on the SCM dispatcher.
pub(super) fn apply_control(control: &ServiceControl, code: u32) -> u32 {
    match code {
        1 | 5 => {
            control.cancel();
            0
        }
        4 => 0,
        _ => 120,
    }
}

unsafe extern "system" fn control_handler(
    code: u32,
    _: u32,
    _: *mut c_void,
    pointer: *mut c_void,
) -> u32 {
    if pointer.is_null() {
        return 6;
    }
    // The one Arc anchor below remains valid for SCM's process lifetime,
    // including callbacks racing fatal exit or normal dispatcher return.
    let control = &*pointer.cast::<ServiceControl>();
    match catch_unwind(AssertUnwindSafe(|| apply_control(control, code))) {
        Ok(result) => result,
        Err(_) => {
            control.callback_panicked.store(true, Ordering::SeqCst);
            control.cancel();
            1064
        }
    }
}

unsafe fn verify_service_name(
    role: ServiceRole,
    count: u32,
    arguments: *mut *mut u16,
) -> Result<(), ServiceHostError> {
    if count == 0 || arguments.is_null() || (*arguments).is_null() {
        return Err(ServiceHostError::ServiceNameMismatch);
    }
    let expected = identity::wide(&role.service_name());
    for (index, value) in expected.iter().enumerate() {
        if *(*arguments).add(index) != *value {
            return Err(ServiceHostError::ServiceNameMismatch);
        }
    }
    Ok(())
}

fn stop_pending(
    control: &ServiceControl,
    failure: Option<ServiceHostError>,
) -> Result<(), ServiceHostError> {
    control.cancel();
    let mut machine = control
        .machine
        .lock()
        .map_err(|_| ServiceHostError::StateInvalid)?;
    if let Some(failure) = failure {
        machine.failure = Some(failure);
    }
    if machine.phase != ServicePhase::StopPending {
        machine.request_stop(control.stop_started())?;
        control.report(&machine)?;
    }
    Ok(())
}

fn finish(
    control: &ServiceControl,
    result: Result<(), ServiceHostError>,
) -> Result<(), ServiceHostError> {
    if control.cancelled.load(Ordering::SeqCst) {
        stop_pending(control, None)?;
    }
    let mut machine = control
        .machine
        .lock()
        .map_err(|_| ServiceHostError::StateInvalid)?;
    let result = machine.finish(result);
    // A panicked business callback cannot attest that its children quiesced;
    // the machine stays STOP_PENDING and the dedicated process must die.
    control.report(&machine)?;
    result
}

pub(super) fn supervise(
    control: &Arc<ServiceControl>,
    entry: ServiceEntry,
) -> Result<(), ServiceHostError> {
    let context = ServiceContext {
        control: control.clone(),
    };
    let (sender, receiver) = mpsc::sync_channel(1);
    let spawned = std::thread::Builder::new()
        .name("aag_service_business".into())
        .spawn(move || {
            let result = catch_unwind(AssertUnwindSafe(|| entry(context)))
                .unwrap_or(Err(ServiceHostError::BusinessPanicked));
            let _ = sender.send(result);
        });
    let worker = match spawned {
        Ok(worker) => worker,
        Err(_) => return finish(control, Err(ServiceHostError::BusinessFailed)),
    };
    loop {
        if control.callback_panicked.load(Ordering::SeqCst) {
            stop_pending(control, Some(ServiceHostError::CallbackPanicked))?;
            return Err(ServiceHostError::CallbackPanicked);
        }
        if control.cancelled.load(Ordering::SeqCst) {
            stop_pending(control, None)?;
        }
        {
            let machine = control
                .machine
                .lock()
                .map_err(|_| ServiceHostError::StateInvalid)?;
            if machine.phase == ServicePhase::StartPending
                && control.started.elapsed() >= STARTUP_TIMEOUT
            {
                drop(machine);
                stop_pending(control, Some(ServiceHostError::StartupTimeout))?;
            } else {
                machine.check_cleanup_deadline(Instant::now())?;
            }
        }
        match receiver.recv_timeout(CANCELLATION_POLL) {
            Ok(result) => {
                if worker.join().is_err() {
                    stop_pending(control, Some(ServiceHostError::BusinessPanicked))?;
                    return Err(ServiceHostError::BusinessPanicked);
                }
                return finish(control, result);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                stop_pending(control, Some(ServiceHostError::BusinessPanicked))?;
                return Err(ServiceHostError::BusinessPanicked);
            }
        }
    }
}

unsafe fn run_service(
    bootstrap: &Bootstrap,
    count: u32,
    arguments: *mut *mut u16,
) -> Result<(), ServiceHostError> {
    let reporter = Arc::new(NativeReporter {
        handle: AtomicUsize::new(0),
    });
    let control = Arc::new(ServiceControl {
        role: bootstrap.role,
        cancelled: AtomicBool::new(false),
        machine: Mutex::new(ServiceMachine::new()),
        started: Instant::now(),
        stop_elapsed_ms: AtomicU64::new(u64::MAX),
        callback_panicked: AtomicBool::new(false),
        reporter: reporter.clone(),
    });
    // Exactly one anchor is retained per own-process dispatch. SCM has no
    // unregister/rundown API for this context. The process reclaims it at exit;
    // retaining it prevents UAF on callbacks racing final STOPPED/fatal exit.
    let anchor = Arc::into_raw(control.clone());
    let name = identity::wide(&bootstrap.role.service_name());
    let handle = RegisterServiceCtrlHandlerExW(
        name.as_ptr(),
        Some(control_handler),
        anchor.cast_mut().cast(),
    );
    if handle.is_null() {
        drop(Arc::from_raw(anchor));
        return Err(ServiceHostError::HandlerRegistrationFailed);
    }
    reporter.handle.store(handle as usize, Ordering::SeqCst);
    {
        let machine = control
            .machine
            .lock()
            .map_err(|_| ServiceHostError::StateInvalid)?;
        control.report(&machine)?;
    }
    let identity_result = (|| {
        verify_service_name(bootstrap.role, count, arguments)?;
        let expected = identity::expected_user_sid(bootstrap.role)?;
        let observed = identity::current_identity()?;
        control
            .machine
            .lock()
            .map_err(|_| ServiceHostError::StateInvalid)?
            .verify_identity(bootstrap.role, &observed, &expected)
    })();
    if let Err(error) = identity_result {
        return finish(&control, Err(error));
    }
    supervise(&control, bootstrap.entry)
}
