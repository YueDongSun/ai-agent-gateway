//! I/O on the actor's own pipe and writer-thread handles only.

use super::{NativeCoreError, NativeIoOperation, READ_CHUNK_BYTES};
use std::{ffi::c_void, fs::File, os::windows::io::AsRawHandle, ptr::null_mut, thread::JoinHandle};

const ERROR_BROKEN_PIPE: u32 = 109;
const ERROR_PIPE_NOT_CONNECTED: u32 = 233;
const ERROR_NOT_FOUND: u32 = 1168;
const WAIT_OBJECT_0: u32 = 0;
const WAIT_TIMEOUT: u32 = 258;

#[link(name = "Kernel32")]
extern "system" {
    fn PeekNamedPipe(
        pipe: *mut c_void,
        buffer: *mut c_void,
        size: u32,
        read: *mut u32,
        available: *mut u32,
        left: *mut u32,
    ) -> i32;
    fn ReadFile(
        file: *mut c_void,
        buffer: *mut c_void,
        size: u32,
        read: *mut u32,
        overlapped: *mut c_void,
    ) -> i32;
    fn CancelSynchronousIo(thread: *mut c_void) -> i32;
    fn WaitForSingleObject(handle: *mut c_void, milliseconds: u32) -> u32;
    fn GetLastError() -> u32;
}

pub(super) enum PipeRead {
    Data(Vec<u8>),
    Idle,
    Eof,
}

/// There is exactly one stdout reader. ReadFile is limited to the bytes already
/// reported available by PeekNamedPipe, so it never waits for another frame.
pub(super) fn read_chunk(file: &File) -> Result<PipeRead, NativeCoreError> {
    let mut available = 0;
    // SAFETY: the borrowed pipe remains owned by the actor for this call.
    if unsafe {
        PeekNamedPipe(
            file.as_raw_handle(),
            null_mut(),
            0,
            null_mut(),
            &mut available,
            null_mut(),
        )
    } == 0
    {
        let code = unsafe { GetLastError() };
        return if matches!(code, ERROR_BROKEN_PIPE | ERROR_PIPE_NOT_CONNECTED) {
            Ok(PipeRead::Eof)
        } else {
            Err(io_error(NativeIoOperation::Peek, code))
        };
    }
    if available == 0 {
        return Ok(PipeRead::Idle);
    }
    let wanted = (available as usize).min(READ_CHUNK_BYTES);
    let mut bytes = vec![0u8; wanted];
    let mut read = 0;
    // SAFETY: the buffer covers wanted bytes; no other reader can consume them.
    if unsafe {
        ReadFile(
            file.as_raw_handle(),
            bytes.as_mut_ptr().cast(),
            wanted as u32,
            &mut read,
            null_mut(),
        )
    } == 0
    {
        let code = unsafe { GetLastError() };
        return if matches!(code, ERROR_BROKEN_PIPE | ERROR_PIPE_NOT_CONNECTED) {
            Ok(PipeRead::Eof)
        } else {
            Err(io_error(NativeIoOperation::Read, code))
        };
    }
    if read == 0 {
        return Ok(PipeRead::Eof);
    }
    bytes.truncate(read as usize);
    Ok(PipeRead::Data(bytes))
}

pub(super) fn cancel_writer(thread: &JoinHandle<()>) -> Result<(), NativeCoreError> {
    // SAFETY: this is the join handle of our single stdin writer, not a caller-
    // supplied handle. Cancellation requests do not claim thread termination.
    if unsafe { CancelSynchronousIo(thread.as_raw_handle()) } != 0 {
        return Ok(());
    }
    let code = unsafe { GetLastError() };
    if code == ERROR_NOT_FOUND {
        Ok(())
    } else {
        Err(io_error(NativeIoOperation::Cancel, code))
    }
}

pub(super) fn thread_finished(thread: &JoinHandle<()>) -> Result<bool, NativeCoreError> {
    // Wait for actual kernel termination before calling Rust join, rather than
    // relying on is_finished while thread-local cleanup may still be running.
    match unsafe { WaitForSingleObject(thread.as_raw_handle(), 0) } {
        WAIT_OBJECT_0 => Ok(true),
        WAIT_TIMEOUT => Ok(false),
        _ => Err(io_error(NativeIoOperation::Join, unsafe { GetLastError() })),
    }
}

fn io_error(operation: NativeIoOperation, os_code: u32) -> NativeCoreError {
    NativeCoreError::Io {
        operation,
        os_code: Some(os_code),
    }
}
