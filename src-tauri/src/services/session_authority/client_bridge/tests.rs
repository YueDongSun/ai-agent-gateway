//! Explicit synthetic executable fixtures. These never select an installed
//! application, native home, gateway, service, credential or upstream model.

use super::*;
use std::{
    io::{Read, Write},
    path::PathBuf,
    process::{Child, Command, ExitStatus, Stdio},
    time::Instant,
};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::{
    handshake::server::{Request, Response},
    http::StatusCode,
};

const FIXTURE_KEY: &str = "synthetic_fixture_key_never_a_live_credential";
const TEST_TIMEOUT: Duration = Duration::from_secs(10);
const PRE_DRAIN_WINDOW: Duration = Duration::from_secs(1);
const RAW_INITIALIZE: &str = r#"{ "id":9007199254740993,"method":"initialize","params":{"clientInfo":{"name":"fixture"},"opaque":"encrypted_fixture_value"}}"#;

struct ShimChild(Child);

impl Drop for ShimChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn shim(endpoint: &str) -> ShimChild {
    let executable = std::env::var_os("AAG_SESSION_SHIM_TEST_EXE")
        .map(PathBuf::from)
        .expect("set AAG_SESSION_SHIM_TEST_EXE to the explicit freshly built dedicated shim");
    assert!(executable.is_absolute() && executable.is_file());
    let mut command = Command::new(executable);
    command
        .arg("app-server")
        .env_clear()
        .env(URL_ENV, endpoint)
        .env(KEY_ENV, FIXTURE_KEY)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Winsock provider DLL resolution uses the OS root even when the
        // endpoint is numeric loopback. Keep only these OS bootstrap values;
        // no user home, application configuration or provider environment is
        // inherited by the dedicated fixture child.
        let system_root =
            std::env::var_os("SystemRoot").expect("fixture requires Windows SystemRoot");
        assert!(PathBuf::from(&system_root).is_absolute());
        command
            .env("SystemRoot", &system_root)
            .env("WINDIR", &system_root);
        command.creation_flags(0x0800_0000);
    }
    ShimChild(
        command
            .spawn()
            .expect("spawn the explicit synthetic shim fixture"),
    )
}

fn wait_exit(child: &mut Child, timeout: Duration) -> Option<ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().expect("inspect owned shim child") {
            return Some(status);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn collect_output(child: &mut ShimChild) -> (ExitStatus, Vec<u8>, Vec<u8>) {
    let mut stdout = child.0.stdout.take().expect("owned stdout");
    let mut stderr = child.0.stderr.take().expect("owned stderr");
    let stdout_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).expect("read owned stdout");
        bytes
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).expect("read owned stderr");
        bytes
    });
    let status = wait_exit(&mut child.0, TEST_TIMEOUT).unwrap_or_else(|| {
        let _ = child.0.kill();
        child.0.wait().expect("reap timed out owned shim");
        panic!("synthetic shim did not become terminal within the fixture deadline")
    });
    (
        status,
        stdout_reader.join().unwrap(),
        stderr_reader.join().unwrap(),
    )
}

async fn listener() -> (TcpListener, String) {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let endpoint = format!(
        "ws://127.0.0.1:{}/aag/codex",
        listener.local_addr().unwrap().port()
    );
    (listener, endpoint)
}

fn authenticate(
    request: &Request,
    response: Response,
) -> Result<Response, tokio_tungstenite::tungstenite::handshake::server::ErrorResponse> {
    if request.uri().path() == "/aag/codex"
        && request
            .headers()
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            == Some(format!("Bearer {FIXTURE_KEY}").as_str())
    {
        Ok(response)
    } else {
        let mut denied = tokio_tungstenite::tungstenite::http::Response::new(None);
        *denied.status_mut() = StatusCode::UNAUTHORIZED;
        Err(denied)
    }
}

fn websocket_error_class(error: &tokio_tungstenite::tungstenite::Error) -> &'static str {
    use tokio_tungstenite::tungstenite::Error;
    match error {
        Error::Io(_) => "io",
        Error::Http(_) => "http",
        Error::Protocol(_) => "protocol",
        Error::Capacity(_) => "capacity",
        Error::Utf8 => "utf8",
        Error::ConnectionClosed => "connection_closed",
        Error::AlreadyClosed => "already_closed",
        _ => "other",
    }
}

fn child_error_code(stderr: &[u8]) -> &'static str {
    match std::str::from_utf8(stderr).map(str::trim) {
        Ok("session_client_connection_failed") => "session_client_connection_failed",
        Ok("session_client_authentication_failed") => "session_client_authentication_failed",
        Ok("session_client_transport_failed") => "session_client_transport_failed",
        Ok("session_client_frame_invalid") => "session_client_frame_invalid",
        Ok("session_client_configuration_invalid") => "session_client_configuration_invalid",
        Ok("") => "empty",
        _ => "unexpected_stderr",
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "starts only the explicitly supplied dedicated shim against a synthetic localhost WebSocket"]
async fn actual_shim_drains_complete_raw_stdout_before_stdin_eof_exit() {
    let (listener, endpoint) = listener().await;
    let response = format!(
        r#"{{"id":9007199254740993,"result":{{"opaque":"encrypted_fixture_value","n":9007199254740993,"padding":"{}"}}}}"#,
        "x".repeat(1024 * 1024)
    );
    let expected = format!("{response}\n").into_bytes();
    let (ready_send, ready) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = tokio::time::timeout(TEST_TIMEOUT, listener.accept())
            .await
            .expect("fixture_accept_timeout")
            .unwrap_or_else(|error| {
                panic!(
                    "fixture_accept_io kind={:?} os={:?}",
                    error.kind(),
                    error.raw_os_error()
                )
            });
        let mut websocket = tokio_tungstenite::accept_hdr_async(stream, authenticate)
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "fixture_handshake_failed class={}",
                    websocket_error_class(&error)
                )
            });
        let incoming = tokio::time::timeout(TEST_TIMEOUT, websocket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(
            incoming.into_text().unwrap() == RAW_INITIALIZE,
            "fixture_initialize_bytes_changed"
        );
        websocket.send(Message::Text(response)).await.unwrap();
        websocket
            .send(Message::Ping(b"fixture_admitted".to_vec()))
            .await
            .unwrap();
        let pong = tokio::time::timeout(TEST_TIMEOUT, websocket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(pong, Message::Pong(b"fixture_admitted".to_vec()));
        ready_send.send(()).unwrap();
        let closed = tokio::time::timeout(TEST_TIMEOUT, websocket.next())
            .await
            .unwrap();
        assert!(matches!(closed, Some(Ok(Message::Close(_)))));
        websocket.flush().await.unwrap();
    });
    let mut child = shim(&endpoint);
    let mut stdin = child.0.stdin.take().unwrap();
    stdin
        .write_all(format!("{RAW_INITIALIZE}\n").as_bytes())
        .unwrap();
    stdin.flush().unwrap();
    if !matches!(tokio::time::timeout(TEST_TIMEOUT, ready).await, Ok(Ok(()))) {
        drop(stdin);
        let (status, stdout, stderr) = collect_output(&mut child);
        server.abort();
        panic!(
            "fixture_not_admitted_before_drain exit={:?} stdout_bytes={} stderr_code={}",
            status.code(),
            stdout.len(),
            child_error_code(&stderr)
        );
    }
    drop(stdin);
    // Leave stdout unread until an early process exit or the bounded window.
    // The 1 MiB response exceeds a pipe buffer, making the old exit race
    // observable without relying on a favorable reader/thread schedule.
    let _ = wait_exit(&mut child.0, PRE_DRAIN_WINDOW);
    let (status, stdout, stderr) = collect_output(&mut child);
    assert!(status.success());
    assert_eq!(
        stdout.len(),
        expected.len(),
        "the dedicated process exited with queued stdout bytes"
    );
    assert!(stdout == expected, "fixture_response_bytes_changed");
    assert!(stderr.is_empty());
    server.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "starts only the explicitly supplied dedicated shim against a synthetic localhost WebSocket"]
async fn actual_shim_rejects_malformed_stdio_and_unauthorized_handshake_without_key_output() {
    for unauthorized in [false, true] {
        let (listener, endpoint) = listener().await;
        let server = tokio::spawn(async move {
            let (stream, _) = tokio::time::timeout(TEST_TIMEOUT, listener.accept())
                .await
                .expect("fixture_accept_timeout")
                .unwrap_or_else(|error| {
                    panic!(
                        "fixture_accept_io kind={:?} os={:?}",
                        error.kind(),
                        error.raw_os_error()
                    )
                });
            if unauthorized {
                let denied = tokio_tungstenite::accept_hdr_async(
                    stream,
                    |_request: &Request, _response: Response| {
                        let mut response =
                            tokio_tungstenite::tungstenite::http::Response::new(None);
                        *response.status_mut() = StatusCode::UNAUTHORIZED;
                        Err(response)
                    },
                )
                .await;
                assert!(denied.is_err());
            } else {
                let mut websocket = tokio_tungstenite::accept_hdr_async(stream, authenticate)
                    .await
                    .unwrap_or_else(|error| {
                        panic!(
                            "fixture_handshake_failed class={}",
                            websocket_error_class(&error)
                        )
                    });
                let _ = tokio::time::timeout(TEST_TIMEOUT, websocket.next())
                    .await
                    .unwrap();
            }
        });
        let mut child = shim(&endpoint);
        let mut stdin = child.0.stdin.take().unwrap();
        if !unauthorized {
            stdin.write_all(b"{malformed_fixture_json}\n").unwrap();
            stdin.flush().unwrap();
        }
        let (status, stdout, stderr) = collect_output(&mut child);
        drop(stdin);
        assert!(!status.success());
        assert!(stdout.is_empty());
        let stderr = String::from_utf8(stderr).unwrap();
        let expected = if unauthorized {
            "session_client_authentication_failed\n"
        } else {
            "session_client_frame_invalid\n"
        };
        assert!(
            stderr.replace("\r\n", "\n") == expected,
            "fixture_rejection_code_changed code={}",
            child_error_code(stderr.as_bytes())
        );
        assert!(!stderr.contains(FIXTURE_KEY));
        server.await.unwrap();
    }
}

#[test]
#[ignore = "starts only the explicitly supplied dedicated shim with an invalid synthetic endpoint"]
fn actual_shim_rejects_untrusted_endpoint_without_native_fallback_or_key_output() {
    let mut child = shim("ws://192.0.2.1:8989/aag/codex");
    drop(child.0.stdin.take());
    let (status, stdout, stderr) = collect_output(&mut child);
    assert!(!status.success());
    assert!(stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&stderr).replace("\r\n", "\n")
            == "session_client_configuration_invalid\n",
        "fixture_endpoint_rejection_code_changed code={}",
        child_error_code(&stderr)
    );
}
