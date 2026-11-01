//! Authenticated stdio/WebSocket adapter for managed native Desktop clients.
//! It owns no payload storage and never starts an unmanaged app-server.

use crate::services::session_broker::native_core::NativeFrame;
use futures_util::{SinkExt, StreamExt};
use std::{
    io::{BufRead, Write},
    net::IpAddr,
    time::Duration,
};
use tokio_tungstenite::tungstenite::{
    client::IntoClientRequest,
    protocol::{Message, WebSocketConfig},
};

const MAX_FRAME_BYTES: usize = 32 * 1024 * 1024;
const QUEUED_FRAMES: usize = 4;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const SEND_TIMEOUT: Duration = Duration::from_secs(30);
const URL_ENV: &str = "AAG_SESSION_BROKER_URL";
const KEY_ENV: &str = "AAG_SESSION_API_KEY";

#[cfg(test)]
#[path = "client_bridge/tests.rs"]
mod process_tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClientError {
    Configuration,
    Authentication,
    Connect,
    Transport,
    Frame,
    Capacity,
    Stdio,
}
impl ClientError {
    fn code(self) -> &'static str {
        match self {
            Self::Configuration => "session_client_configuration_invalid",
            Self::Authentication => "session_client_authentication_failed",
            Self::Connect => "session_client_connection_failed",
            Self::Transport => "session_client_transport_failed",
            Self::Frame => "session_client_frame_invalid",
            Self::Capacity => "session_client_capacity_exceeded",
            Self::Stdio => "session_client_stdio_failed",
        }
    }
}

fn endpoint(value: &str) -> Result<url::Url, ClientError> {
    let url = url::Url::parse(value).map_err(|_| ClientError::Configuration)?;
    // Url normalizes an explicit default :80 to None. Require a numeric port
    // in the supplied authority, then compare its effective parsed value so
    // omission remains forbidden while every explicit valid port is retained.
    let explicit_port = value
        .split_once("://")
        .and_then(|(_, rest)| rest.split('/').next())
        .and_then(|authority| authority.rsplit_once(':'))
        .and_then(|(_, port)| {
            port.bytes()
                .all(|byte| byte.is_ascii_digit())
                .then(|| port.parse::<u16>().ok())
                .flatten()
        })
        .filter(|port| *port != 0)
        .ok_or(ClientError::Configuration)?;
    let host = url
        .host_str()
        .ok_or(ClientError::Configuration)?
        .trim_matches(['[', ']']);
    let ip: IpAddr = host.parse().map_err(|_| ClientError::Configuration)?;
    if url.scheme() != "ws"
        || !ip.is_loopback()
        || url.port_or_known_default() != Some(explicit_port)
        || url.path() != "/aag/codex"
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(ClientError::Configuration);
    }
    Ok(url)
}

fn validate_key(value: &str) -> Result<(), ClientError> {
    if value.trim().is_empty() || value.len() > 8192 || value.chars().any(char::is_control) {
        Err(ClientError::Authentication)
    } else {
        Ok(())
    }
}

/// Dedicated shim executable only: stdin/stdout threads are bounded and owned
/// by that process. Its entrypoint exits when this function returns; it must
/// never be embedded in the Tauri or policy service process.
pub(super) fn run_stdio() -> i32 {
    let result = (|| {
        let raw_url = std::env::var(URL_ENV).map_err(|_| ClientError::Configuration)?;
        let url = endpoint(&raw_url)?;
        let key = std::env::var(KEY_ENV).map_err(|_| ClientError::Authentication)?;
        validate_key(&key)?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| ClientError::Transport)?;
        runtime.block_on(relay(url, &key))
    })();
    match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("{}", error.code());
            1
        }
    }
}

async fn relay(url: url::Url, key: &str) -> Result<(), ClientError> {
    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|_| ClientError::Configuration)?;
    request.headers_mut().insert(
        "authorization",
        format!("Bearer {key}")
            .parse()
            .map_err(|_| ClientError::Authentication)?,
    );
    let config = WebSocketConfig {
        max_message_size: Some(MAX_FRAME_BYTES),
        max_frame_size: Some(MAX_FRAME_BYTES),
        ..Default::default()
    };
    let connected = tokio::time::timeout(
        CONNECT_TIMEOUT,
        tokio_tungstenite::connect_async_with_config(request, Some(config), true),
    )
    .await
    .map_err(|_| ClientError::Connect)?;
    let (mut websocket, _) = connected.map_err(|error| match error {
        tokio_tungstenite::tungstenite::Error::Http(response)
            if response.status().as_u16() == 401 =>
        {
            ClientError::Authentication
        }
        _ => ClientError::Connect,
    })?;
    let (input_send, mut input) =
        tokio::sync::mpsc::channel::<Result<Option<String>, ClientError>>(QUEUED_FRAMES);
    let (output_send, output) = std::sync::mpsc::sync_channel::<String>(QUEUED_FRAMES);
    let (failure_send, mut failures) = tokio::sync::mpsc::channel(1);
    let (output_done_send, output_done) = tokio::sync::oneshot::channel();
    let input_worker = std::thread::Builder::new()
        .name("aag_client_stdin".into())
        .spawn(move || {
            let stdin = std::io::stdin();
            let mut reader = stdin.lock();
            loop {
                let frame = read_frame(&mut reader);
                let terminal = !matches!(&frame, Ok(Some(_)));
                if input_send.blocking_send(frame).is_err() || terminal {
                    break;
                }
            }
        })
        .map_err(|_| ClientError::Stdio)?;
    let output_worker = std::thread::Builder::new()
        .name("aag_client_stdout".into())
        .spawn(move || {
            let stdout = std::io::stdout();
            let mut writer = stdout.lock();
            let mut result = Ok(());
            while let Ok(frame) = output.recv() {
                if writer
                    .write_all(frame.as_bytes())
                    .and_then(|_| writer.write_all(b"\n"))
                    .and_then(|_| writer.flush())
                    .is_err()
                {
                    let _ = failure_send.blocking_send(ClientError::Stdio);
                    result = Err(ClientError::Stdio);
                    break;
                }
            }
            let _ = output_done_send.send(result);
        })
        .map_err(|_| ClientError::Stdio)?;
    loop {
        tokio::select! {
            failed=failures.recv()=>return Err(failed.unwrap_or(ClientError::Stdio)),
            incoming=input.recv()=>match incoming {
                Some(Ok(Some(frame)))=>{tokio::time::timeout(SEND_TIMEOUT,websocket.send(Message::Text(frame))).await.map_err(|_|ClientError::Transport)?.map_err(|_|ClientError::Transport)?;},
                Some(Ok(None))|None=>{
                    let closed=tokio::time::timeout(SEND_TIMEOUT,websocket.close(None)).await;
                    // Dedicated main uses process::exit. Close the bounded
                    // queue and await its writer before returning success;
                    // otherwise an admitted response can be cut off while
                    // stdout is backpressured, even after a successful close.
                    drop(output_send);
                    let drained=tokio::time::timeout(SEND_TIMEOUT,output_done)
                        .await.map_err(|_|ClientError::Stdio)?
                        .map_err(|_|ClientError::Stdio)?;
                    output_worker.join().map_err(|_|ClientError::Stdio)?;
                    input_worker.join().map_err(|_|ClientError::Stdio)?;
                    drained?;
                    closed.map_err(|_|ClientError::Transport)?.map_err(|_|ClientError::Transport)?;
                    return Ok(());
                },
                Some(Err(error))=>return Err(error),
            },
            incoming=websocket.next()=>match incoming {
                Some(Ok(Message::Text(frame)))=>{
                    if frame.len()>MAX_FRAME_BYTES || NativeFrame::parse(frame.clone()).is_err(){return Err(ClientError::Frame);}
                    output_send.try_send(frame).map_err(|error|match error {std::sync::mpsc::TrySendError::Full(_)=>ClientError::Capacity,_=>ClientError::Stdio})?;
                },
                Some(Ok(Message::Ping(bytes)))=>{tokio::time::timeout(SEND_TIMEOUT,websocket.send(Message::Pong(bytes))).await.map_err(|_|ClientError::Transport)?.map_err(|_|ClientError::Transport)?;},
                Some(Ok(Message::Pong(_)))=>{},
                // An unsolicited upstream close can leave an unacknowledged
                // request or an undrained stdout frame; never report success.
                Some(Ok(Message::Close(_)))|None=>return Err(ClientError::Transport),
                _=>return Err(ClientError::Transport),
            }
        }
    }
}

fn read_frame(reader: &mut impl BufRead) -> Result<Option<String>, ClientError> {
    let mut bytes = Vec::new();
    loop {
        let available = reader.fill_buf().map_err(|_| ClientError::Stdio)?;
        if available.is_empty() {
            return if bytes.is_empty() {
                Ok(None)
            } else {
                Err(ClientError::Frame)
            };
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let count = newline.map(|offset| offset + 1).unwrap_or(available.len());
        if bytes.len().saturating_add(count) > MAX_FRAME_BYTES + 2 {
            return Err(ClientError::Capacity);
        }
        bytes.extend_from_slice(&available[..count]);
        reader.consume(count);
        if newline.is_some() {
            break;
        }
    }
    bytes.pop();
    if bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(ClientError::Capacity);
    }
    let frame = String::from_utf8(bytes).map_err(|_| ClientError::Frame)?;
    NativeFrame::parse(frame.clone()).map_err(|_| ClientError::Frame)?;
    Ok(Some(frame))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn client_key_never_goes_in_a_url_or_a_non_loopback_plaintext_connection() {
        for invalid in [
            "ws://100.64.0.1:8989/aag/codex",
            "ws://localhost:8989/aag/codex",
            "ws://127.0.0.1:8989/aag/codex?key=fixture",
            "ws://fixture@127.0.0.1:8989/aag/codex",
            "ws://127.0.0.1/aag/codex",
        ] {
            assert!(endpoint(invalid).is_err());
        }
        assert!(endpoint("ws://127.0.0.1:8989/aag/codex").is_ok());
        assert!(endpoint("ws://[::1]:8989/aag/codex").is_ok());
        assert!(validate_key("fixture_existing_key").is_ok());
        assert!(validate_key("fixture\r\nheader").is_err());
    }
    #[test]
    fn client_endpoint_preserves_explicit_default_port_eighty() {
        assert!(endpoint("ws://127.0.0.1:80/aag/codex").is_ok());
        assert!(endpoint("ws://[::1]:80/aag/codex").is_ok());
        for invalid in [
            "ws://127.0.0.1/aag/codex",
            "ws://[::1]/aag/codex",
            "ws://127.0.0.1:0/aag/codex",
            "ws://127.0.0.1:65536/aag/codex",
        ] {
            assert!(endpoint(invalid).is_err());
        }
    }
    #[test]
    fn stdio_preserves_raw_frames_and_rejects_truncated_or_ambiguous_envelopes() {
        let original="{\"id\":1,\"method\":\"turn/start\",\"params\":{\"opaque\":\"encrypted_value\",\"n\":9007199254740993}}";
        let mut reader = std::io::Cursor::new(format!("{original}\r\n"));
        assert_eq!(read_frame(&mut reader).unwrap().as_deref(), Some(original));
        assert!(read_frame(&mut reader).unwrap().is_none());
        for invalid in ["{\"id\":1}", "{\"id\":1,\"id\":2,\"result\":null}\n"] {
            assert!(read_frame(&mut std::io::Cursor::new(invalid)).is_err());
        }
    }
}
