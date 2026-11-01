//! One bounded FIFO writer. No process-owner lock is held during pipe writes.

use super::{multiplex::OutboundFrame, windows_io, NativeCoreError, NativeIoOperation};
use std::{
    fs::File,
    io::Write,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const WRITER_QUEUE_FRAMES: usize = 4;
const WRITE_CHUNK_BYTES: usize = 16 * 1024;
const JOIN_POLL: Duration = Duration::from_millis(10);
const DROP_TIMEOUT: Duration = Duration::from_secs(2);

pub(super) enum WriteReport {
    Written(u64),
    Failed {
        sequence: u64,
        error: NativeCoreError,
    },
}

pub(super) struct WriterOwner {
    sender: Option<SyncSender<OutboundFrame>>,
    reports: Receiver<WriteReport>,
    thread: Option<JoinHandle<()>>,
    stopped: Arc<AtomicBool>,
    failure: Arc<Mutex<Option<NativeCoreError>>>,
}

impl WriterOwner {
    pub fn start(file: File, max_outbound: usize) -> Result<Self, NativeCoreError> {
        Self::start_writer(file, max_outbound)
    }

    fn start_writer<W: Write + Send + 'static>(
        file: W,
        max_outbound: usize,
    ) -> Result<Self, NativeCoreError> {
        if max_outbound == 0 {
            return Err(NativeCoreError::InvalidConfiguration);
        }
        let (sender, receiver) = mpsc::sync_channel(WRITER_QUEUE_FRAMES.min(max_outbound));
        let (report_sender, reports) = mpsc::sync_channel(max_outbound);
        let stopped = Arc::new(AtomicBool::new(false));
        let failure = Arc::new(Mutex::new(None));
        let worker_stopped = stopped.clone();
        let worker_failure = failure.clone();
        let thread = thread::Builder::new()
            .name("aag_native_stdin".into())
            .spawn(move || {
                writer_loop(
                    file,
                    receiver,
                    report_sender,
                    &worker_stopped,
                    &worker_failure,
                );
            })
            .map_err(|error| NativeCoreError::Io {
                operation: NativeIoOperation::ThreadStart,
                os_code: error.raw_os_error().map(|code| code as u32),
            })?;
        Ok(Self {
            sender: Some(sender),
            reports,
            thread: Some(thread),
            stopped,
            failure,
        })
    }

    pub fn try_send(&self, frame: OutboundFrame) -> Result<(), TrySendError<OutboundFrame>> {
        match &self.sender {
            Some(sender) if !self.stopped.load(Ordering::SeqCst) => sender.try_send(frame),
            _ => Err(TrySendError::Disconnected(frame)),
        }
    }

    pub fn report(&self) -> Result<Option<WriteReport>, NativeCoreError> {
        match self.reports.try_recv() {
            Ok(report) => Ok(Some(report)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) if self.stopped.load(Ordering::SeqCst) => Ok(None),
            Err(TryRecvError::Disconnected) => {
                Err(self.failure().unwrap_or(NativeCoreError::WriterUnavailable))
            }
        }
    }

    pub fn failure(&self) -> Option<NativeCoreError> {
        self.failure
            .lock()
            .map(|value| *value)
            .unwrap_or(Some(NativeCoreError::WriterUnavailable))
    }

    pub fn stop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.sender.take();
    }

    /// On failure the same JoinHandle remains owned here for later reconciliation.
    pub fn join_until(&mut self, deadline: Instant) -> Result<(), NativeCoreError> {
        self.stop();
        loop {
            let Some(thread) = self.thread.as_ref() else {
                return Ok(());
            };
            if windows_io::thread_finished(thread)? {
                if self
                    .thread
                    .take()
                    .expect("checked writer handle")
                    .join()
                    .is_err()
                {
                    set_failure(&self.failure, NativeCoreError::WriterUnavailable);
                }
                return Ok(());
            }
            // Retry cancellation only for this owned thread; the first call may
            // race its transition from the queue wait into a synchronous write.
            windows_io::cancel_writer(thread)?;
            if Instant::now() >= deadline {
                return Err(NativeCoreError::WriterUnavailable);
            }
            thread::sleep(JOIN_POLL.min(deadline.saturating_duration_since(Instant::now())));
        }
    }
}

impl Drop for WriterOwner {
    fn drop(&mut self) {
        // Emergency cleanup only. A caller must retain an explicit failed
        // shutdown owner; this fallback is not terminal evidence.
        let _ = self.join_until(Instant::now() + DROP_TIMEOUT);
    }
}

fn writer_loop<W: Write>(
    mut file: W,
    receiver: Receiver<OutboundFrame>,
    reports: SyncSender<WriteReport>,
    stopped: &AtomicBool,
    failure: &Mutex<Option<NativeCoreError>>,
) {
    while !stopped.load(Ordering::SeqCst) {
        let Ok(frame) = receiver.recv() else {
            break;
        };
        if stopped.load(Ordering::SeqCst) {
            break;
        }
        let mut offset = 0;
        while offset < frame.bytes.len() && !stopped.load(Ordering::SeqCst) {
            let end = (offset + WRITE_CHUNK_BYTES).min(frame.bytes.len());
            match file.write(&frame.bytes[offset..end]) {
                Ok(0) => {
                    let error = NativeCoreError::TransportFailed;
                    set_failure(failure, error);
                    let _ = reports.try_send(WriteReport::Failed {
                        sequence: frame.sequence,
                        error,
                    });
                    return;
                }
                Ok(written) => offset += written,
                Err(error) => {
                    let error = NativeCoreError::Io {
                        operation: NativeIoOperation::Write,
                        os_code: error.raw_os_error().map(|code| code as u32),
                    };
                    set_failure(failure, error);
                    let _ = reports.try_send(WriteReport::Failed {
                        sequence: frame.sequence,
                        error,
                    });
                    return;
                }
            }
        }
        if offset != frame.bytes.len() {
            break;
        }
        if reports
            .try_send(WriteReport::Written(frame.sequence))
            .is_err()
        {
            set_failure(failure, NativeCoreError::CapacityExceeded);
            break;
        }
    }
}

fn set_failure(failure: &Mutex<Option<NativeCoreError>>, error: NativeCoreError) {
    if let Ok(mut value) = failure.lock() {
        value.get_or_insert(error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Condvar;

    struct ShortWriter {
        bytes: Arc<Mutex<Vec<u8>>>,
        fail_after: Option<usize>,
    }
    impl Write for ShortWriter {
        fn write(&mut self, input: &[u8]) -> std::io::Result<usize> {
            let mut bytes = self.bytes.lock().unwrap();
            if self.fail_after.is_some_and(|limit| bytes.len() >= limit) {
                return Err(std::io::Error::from(std::io::ErrorKind::Interrupted));
            }
            let count = input.len().min(3);
            bytes.extend_from_slice(&input[..count]);
            Ok(count)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn wait_report(writer: &WriterOwner) -> WriteReport {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(report) = writer.report().unwrap() {
                return report;
            }
            assert!(Instant::now() < deadline, "writer report deadline");
            thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn partial_writes_keep_frames_fifo_and_never_replay_interrupted_bytes() {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let mut writer = WriterOwner::start_writer(
            ShortWriter {
                bytes: bytes.clone(),
                fail_after: None,
            },
            2,
        )
        .unwrap();
        assert!(writer
            .try_send(OutboundFrame {
                sequence: 1,
                bytes: b"first\n".to_vec()
            })
            .is_ok());
        assert!(writer
            .try_send(OutboundFrame {
                sequence: 2,
                bytes: b"second\n".to_vec()
            })
            .is_ok());
        assert!(matches!(wait_report(&writer), WriteReport::Written(1)));
        assert!(matches!(wait_report(&writer), WriteReport::Written(2)));
        writer
            .join_until(Instant::now() + Duration::from_secs(2))
            .unwrap();
        assert_eq!(&*bytes.lock().unwrap(), b"first\nsecond\n");

        let bytes = Arc::new(Mutex::new(Vec::new()));
        let mut writer = WriterOwner::start_writer(
            ShortWriter {
                bytes: bytes.clone(),
                fail_after: Some(3),
            },
            1,
        )
        .unwrap();
        assert!(writer
            .try_send(OutboundFrame {
                sequence: 3,
                bytes: b"uncertain\n".to_vec()
            })
            .is_ok());
        assert!(matches!(
            wait_report(&writer),
            WriteReport::Failed { sequence: 3, .. }
        ));
        writer
            .join_until(Instant::now() + Duration::from_secs(2))
            .unwrap();
        assert_eq!(&*bytes.lock().unwrap(), b"unc");
    }

    #[derive(Default)]
    struct Gate {
        entered: bool,
        released: bool,
    }
    struct GatedWriter {
        gate: Arc<(Mutex<Gate>, Condvar)>,
    }
    impl Write for GatedWriter {
        fn write(&mut self, input: &[u8]) -> std::io::Result<usize> {
            let (mutex, wake) = &*self.gate;
            let mut gate = mutex.lock().unwrap();
            gate.entered = true;
            wake.notify_all();
            while !gate.released {
                gate = wake.wait(gate).unwrap();
            }
            Ok(input.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    struct ReleaseGuard(Arc<(Mutex<Gate>, Condvar)>);
    impl Drop for ReleaseGuard {
        fn drop(&mut self) {
            let (mutex, wake) = &*self.0;
            mutex.lock().unwrap().released = true;
            wake.notify_all();
        }
    }

    #[test]
    fn failed_join_retains_the_same_writer_until_explicit_reconciliation() {
        let gate = Arc::new((Mutex::new(Gate::default()), Condvar::new()));
        let mut writer = WriterOwner::start_writer(GatedWriter { gate: gate.clone() }, 1).unwrap();
        // Created after writer so unwinding releases the fixture before cleanup.
        let release = ReleaseGuard(gate.clone());
        assert!(writer
            .try_send(OutboundFrame {
                sequence: 1,
                bytes: b"pending\n".to_vec()
            })
            .is_ok());
        let deadline = Instant::now() + Duration::from_secs(2);
        let (mutex, wake) = &*gate;
        let mut state = mutex.lock().unwrap();
        while !state.entered {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "gated writer start deadline");
            state = wake.wait_timeout(state, remaining).unwrap().0;
        }
        drop(state);
        assert!(writer.join_until(Instant::now()).is_err());
        assert!(
            writer.thread.is_some(),
            "failed cleanup must retain ownership"
        );
        drop(release);
        writer
            .join_until(Instant::now() + Duration::from_secs(2))
            .unwrap();
        assert!(writer.thread.is_none());
    }
}
