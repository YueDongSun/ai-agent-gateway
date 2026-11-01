//! Bounded duplex tasks. A write acknowledgement means bytes written only.
//! Queue budgets apply independently to each direction, including active frames.

use super::*;
use std::{future::Future, io, sync::Mutex as StdMutex};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::{oneshot, OwnedSemaphorePermit},
    task::JoinHandle,
};

pub(super) struct TaskControl {
    pub cancel: TransportCancellation,
    failure: StdMutex<Option<TransportError>>,
    cleanup_failure: StdMutex<Option<TransportError>>,
    join: StdMutex<Option<JoinHandle<()>>>,
    done: AtomicBool,
    finished: Notify,
}
impl TaskControl {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            cancel: TransportCancellation::new(),
            failure: StdMutex::new(None),
            cleanup_failure: StdMutex::new(None),
            join: StdMutex::new(None),
            done: AtomicBool::new(false),
            finished: Notify::new(),
        })
    }
    pub fn spawn<F>(self: &Arc<Self>, future: F) -> Result<(), TransportError>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| TransportError::RuntimeUnavailable)?;
        let guard = CompletionGuard(self.clone());
        let task = runtime.spawn(async move {
            let _guard = guard;
            future.await;
        });
        *self.join.lock().unwrap_or_else(|error| error.into_inner()) = Some(task);
        Ok(())
    }
    pub fn fail(&self, error: TransportError) {
        let mut failure = self
            .failure
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if failure.is_none() {
            *failure = Some(error);
        }
        self.cancel.cancel();
    }
    pub fn error(&self) -> TransportError {
        self.failure
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .unwrap_or(TransportError::Cancelled)
    }
    pub fn cleanup_failed(&self, error: TransportError) {
        *self
            .cleanup_failure
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Some(error);
    }
    fn closed_outcome(&self) -> TransportClosed {
        self.cleanup_failure
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .map_or(TransportClosed::Closed, TransportClosed::OutcomeUnknown)
    }
    pub fn abort(&self) {
        self.cancel.cancel();
        if let Some(task) = self
            .join
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
        {
            task.abort();
        }
    }
    pub async fn close(&self, deadline: Instant) -> TransportClosed {
        self.cancel.cancel();
        let task = self
            .join
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if let Some(mut task) = task {
            match tokio::time::timeout_at(deadline.into(), &mut task).await {
                Ok(Ok(())) => self.closed_outcome(),
                Ok(Err(error)) if error.is_cancelled() && self.done.load(Ordering::SeqCst) => {
                    self.closed_outcome()
                }
                Ok(Err(_)) => TransportClosed::OutcomeUnknown(TransportError::Kernel {
                    operation: KernelOperation::Join,
                    code: 0,
                }),
                Err(_) => {
                    self.cleanup_failed(TransportError::DeadlineExceeded);
                    task.abort();
                    TransportClosed::OutcomeUnknown(TransportError::DeadlineExceeded)
                }
            }
        } else {
            let wait = async {
                loop {
                    let notified = self.finished.notified();
                    tokio::pin!(notified);
                    notified.as_mut().enable();
                    if self.done.load(Ordering::SeqCst) {
                        return;
                    }
                    notified.await;
                }
            };
            match tokio::time::timeout_at(deadline.into(), wait).await {
                Ok(()) => self.closed_outcome(),
                Err(_) => TransportClosed::OutcomeUnknown(TransportError::DeadlineExceeded),
            }
        }
    }
}
struct CompletionGuard(Arc<TaskControl>);
impl Drop for CompletionGuard {
    fn drop(&mut self) {
        self.0.cancel.cancel();
        self.0.done.store(true, Ordering::SeqCst);
        self.0.finished.notify_waiters();
    }
}

pub(super) struct OutgoingFrame {
    frame: TransportFrame,
    _bytes: OwnedSemaphorePermit,
    completed: oneshot::Sender<Result<(), TransportError>>,
}
pub(super) struct IncomingFrame {
    pub frame: TransportFrame,
    _bytes: OwnedSemaphorePermit,
}

pub(super) fn start<S>(
    stream: S,
    peer: PeerAttestation,
    limits: TransportLimits,
    connection_permit: Option<OwnedSemaphorePermit>,
) -> Result<LocalConnection, TransportError>
where
    S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    limits.validate()?;
    let control = TaskControl::new();
    let driver_control = control.clone();
    let (outgoing, outgoing_rx) = mpsc::channel(limits.queued_frames);
    let (incoming_tx, incoming) = mpsc::channel(limits.queued_frames);
    let outgoing_bytes = Arc::new(Semaphore::new(limits.queued_bytes));
    let incoming_bytes = Arc::new(Semaphore::new(limits.queued_bytes));
    control.spawn(async move {
        let _connection_permit = connection_permit;
        let (reader, writer) = tokio::io::split(stream);
        let result = tokio::select! {
            biased;
            _ = driver_control.cancel.cancelled() => Ok(()),
            result = read_loop(reader, &incoming_tx, incoming_bytes, limits) => result,
            result = write_loop(writer, outgoing_rx) => result,
        };
        if let Err(error) = result {
            driver_control.fail(error);
        }
        // Dropping both halves cancels outstanding overlapped IO; no RPC retry.
    })?;
    Ok(LocalConnection {
        peer,
        limits,
        control,
        outgoing,
        outgoing_bytes,
        incoming: Mutex::new(incoming),
    })
}

pub(super) async fn send(
    connection: &LocalConnection,
    frame: TransportFrame,
    deadline: Instant,
) -> SendOutcome {
    if connection.control.cancel.is_cancelled() {
        return SendOutcome::NotSent(connection.control.error());
    }
    if frame.bytes().len() > connection.limits.frame_bytes {
        return SendOutcome::NotSent(TransportError::FrameTooLarge);
    }
    let reserve = async {
        let bytes = connection
            .outgoing_bytes
            .clone()
            .acquire_many_owned((frame.bytes().len() + 4) as u32)
            .await
            .map_err(|_| TransportError::Closed)?;
        let slot = connection
            .outgoing
            .reserve()
            .await
            .map_err(|_| connection.control.error())?;
        Ok::<_, TransportError>((bytes, slot))
    };
    let (bytes, slot) = tokio::select! {
        biased;
        _ = connection.control.cancel.cancelled() => return SendOutcome::NotSent(connection.control.error()),
        _ = tokio::time::sleep_until(deadline.into()) => return SendOutcome::NotSent(TransportError::DeadlineExceeded),
        result = reserve => match result { Ok(value) => value, Err(error) => return SendOutcome::NotSent(error) },
    };
    // Recheck after acquiring capacity. Admission is the uncertainty boundary.
    if connection.control.cancel.is_cancelled() {
        return SendOutcome::NotSent(connection.control.error());
    }
    if Instant::now() >= deadline {
        return SendOutcome::NotSent(TransportError::DeadlineExceeded);
    }
    let (completed, completion) = oneshot::channel();
    slot.send(OutgoingFrame {
        frame,
        _bytes: bytes,
        completed,
    });
    let mut admission = AdmissionGuard {
        control: &connection.control,
        active: true,
    };
    let outcome = tokio::select! {
        biased;
        result = completion => match result {
            Ok(Ok(())) => SendOutcome::Written,
            Ok(Err(error)) => SendOutcome::OutcomeUnknown(error),
            Err(_) => SendOutcome::OutcomeUnknown(connection.control.error()),
        },
        _ = connection.control.cancel.cancelled() => SendOutcome::OutcomeUnknown(connection.control.error()),
        _ = tokio::time::sleep_until(deadline.into()) => {
            connection.control.fail(TransportError::DeadlineExceeded);
            SendOutcome::OutcomeUnknown(TransportError::DeadlineExceeded)
        },
    };
    admission.active = false;
    outcome
}
/// Dropping a caller's admitted send future also closes the connection. It may
/// already have written bytes, so the owner must reconcile before another RPC.
struct AdmissionGuard<'a> {
    control: &'a TaskControl,
    active: bool,
}
impl Drop for AdmissionGuard<'_> {
    fn drop(&mut self) {
        if self.active {
            self.control.fail(TransportError::Cancelled);
        }
    }
}

async fn read_loop<R: AsyncRead + Unpin>(
    mut reader: R,
    incoming: &mpsc::Sender<IncomingFrame>,
    bytes: Arc<Semaphore>,
    limits: TransportLimits,
) -> Result<(), TransportError> {
    loop {
        let mut header = [0; 4];
        // A clean EOF before any header is distinct from a truncated envelope.
        let first = reader
            .read(&mut header[..1])
            .await
            .map_err(|error| io_error(error, KernelOperation::Read))?;
        if first == 0 {
            return Err(TransportError::Closed);
        }
        reader
            .read_exact(&mut header[1..])
            .await
            .map_err(read_error)?;
        let length = codec::decode_length(header, limits.frame_bytes)?;
        let permit = bytes
            .clone()
            .acquire_many_owned((length + 4) as u32)
            .await
            .map_err(|_| TransportError::Closed)?;
        let mut body = vec![0; length];
        reader.read_exact(&mut body).await.map_err(read_error)?;
        let frame = TransportFrame::parse(body, limits.frame_bytes)?;
        incoming
            .send(IncomingFrame {
                frame,
                _bytes: permit,
            })
            .await
            .map_err(|_| TransportError::Closed)?;
    }
}
async fn write_loop<W: AsyncWrite + Unpin>(
    mut writer: W,
    mut outgoing: mpsc::Receiver<OutgoingFrame>,
) -> Result<(), TransportError> {
    while let Some(value) = outgoing.recv().await {
        let result = async {
            writer
                .write_all(&codec::encode_length(&value.frame))
                .await?;
            writer.write_all(value.frame.bytes()).await
        }
        .await
        .map_err(|error| io_error(error, KernelOperation::Write));
        let _ = value.completed.send(result);
        result?;
    }
    Err(TransportError::Closed)
}
fn read_error(error: io::Error) -> TransportError {
    if matches!(
        error.kind(),
        io::ErrorKind::UnexpectedEof | io::ErrorKind::BrokenPipe
    ) {
        TransportError::TruncatedFrame
    } else {
        io_error(error, KernelOperation::Read)
    }
}
pub(super) fn io_error(error: io::Error, operation: KernelOperation) -> TransportError {
    TransportError::Kernel {
        operation,
        code: error.raw_os_error().unwrap_or(0) as u32,
    }
}
