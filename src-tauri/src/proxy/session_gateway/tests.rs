//! Real WS and LocalConnection framing on task-owned loopback/duplex fixtures.
//! These fixtures do not establish a committed native authority or OS identity.

use super::*;
use crate::{
    proxy::{
        publication::{PublicationCancellation, PublicationFrontend},
        router::Router,
    },
    services::session_broker::local_transport::{
        fixture_duplex_for_gateway, SendOutcome, TransportFrame, TransportLimits,
    },
};
use axum::{
    body::Body,
    http::{Request as HttpRequest, StatusCode},
    Extension, Router as AxumRouter,
};
use futures_util::{SinkExt, StreamExt};
use std::{
    collections::{HashMap, VecDeque},
    sync::atomic::AtomicUsize,
};
use tokio::{io::AsyncReadExt, net::TcpListener, sync::oneshot, task::JoinHandle};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, Error as WsError, Message},
};
use tower::Service;

const KEY: &str = "synthetic_gateway_key";
const WAIT: Duration = Duration::from_secs(5);
struct FixtureFactory {
    connections: Mutex<VecDeque<LocalConnection>>,
    attempts: AtomicUsize,
}
#[async_trait::async_trait]
impl TestConnectionFactory for FixtureFactory {
    async fn connect(&self) -> Result<LocalConnection, String> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        self.connections
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| "synthetic_authority_unavailable".into())
    }
}
fn factory(connections: Vec<LocalConnection>) -> Arc<FixtureFactory> {
    Arc::new(FixtureFactory {
        connections: Mutex::new(connections.into()),
        attempts: AtomicUsize::new(0),
    })
}
fn state() -> ProxyAppState {
    ProxyAppState {
        router: Arc::new(Router::new(HashMap::new(), HashMap::new())),
        auth_token: KEY.into(),
        pinned_provider: Arc::new(Mutex::new(None)),
        responses_cot_enabled: Arc::new(AtomicBool::new(false)),
        passthrough_auth_allowed: true,
        db: None,
        live: None,
        allow_client_config_repair: false,
    }
}
fn app(factory: Arc<FixtureFactory>, cancel: &SessionGatewayCancellation) -> AxumRouter {
    AxumRouter::new()
        .route(PATH, route())
        .layer(Extension(TestConnector(factory)))
        .layer(Extension(cancel.clone()))
        .with_state(state())
}
async fn send(mut router: AxumRouter, request: HttpRequest<Body>) -> axum::response::Response {
    std::future::poll_fn(|context| Service::<HttpRequest<Body>>::poll_ready(&mut router, context))
        .await
        .unwrap();
    router.call(request).await.unwrap()
}
fn deadline() -> Instant {
    Instant::now() + WAIT
}
fn limits() -> TransportLimits {
    TransportLimits {
        frame_bytes: FRAME_BYTES,
        queued_bytes: FRAME_BYTES * 2,
        queued_frames: 2,
        connections: 2,
    }
}
fn pipe_pair() -> (LocalConnection, LocalConnection) {
    let (left, right) = tokio::io::duplex(4096);
    (
        fixture_duplex_for_gateway(left, limits()).unwrap(),
        fixture_duplex_for_gateway(right, limits()).unwrap(),
    )
}
fn rpc(raw: &str) -> TransportFrame {
    TransportFrame::parse(raw.as_bytes().to_vec(), FRAME_BYTES).unwrap()
}
fn ws_request(address: std::net::SocketAddr) -> HttpRequest<()> {
    let mut request = format!("ws://{address}{PATH}")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("authorization", format!("Bearer {KEY}").parse().unwrap());
    request
}
struct FixtureServer {
    address: std::net::SocketAddr,
    cancel: SessionGatewayCancellation,
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}
impl FixtureServer {
    async fn start(factory: Arc<FixtureFactory>) -> Self {
        let cancel = SessionGatewayCancellation::new();
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let (shutdown, stop) = oneshot::channel();
        let app = app(factory, &cancel);
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = stop.await;
                })
                .await
                .unwrap();
        });
        Self {
            address,
            cancel,
            shutdown: Some(shutdown),
            task,
        }
    }
    async fn stop(&mut self) {
        assert_eq!(
            self.cancel.close(deadline()).await,
            SessionGatewayClosed::Closed
        );
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        tokio::time::timeout(WAIT, &mut self.task)
            .await
            .unwrap()
            .unwrap();
    }
}
impl Drop for FixtureServer {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.task.abort();
    }
}
async fn ws_terminal<S>(socket: &mut tokio_tungstenite::WebSocketStream<S>)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    tokio::time::timeout(WAIT, async {
        loop {
            match socket.next().await {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
                _ => panic!("unexpected payload after relay stop"),
            }
        }
    })
    .await
    .expect("WebSocket relay did not terminate");
}

#[tokio::test]
async fn strict_auth_precedes_upgrade_and_unavailable_authority_never_upgrades() {
    let factory = factory(Vec::new());
    let cancel = SessionGatewayCancellation::new();
    for method in ["GET", "POST", "PATCH"] {
        for credential in [None, Some("Bearer fixture_subscription_oauth")] {
            let mut request = HttpRequest::builder().method(method).uri(PATH);
            if let Some(value) = credential {
                request = request.header("authorization", value);
            }
            let response = send(app(factory.clone(), &cancel), request.body(Body::empty()).unwrap()).await;
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
    }
    assert_eq!(factory.attempts.load(Ordering::SeqCst), 0);
    let mut server = FixtureServer::start(factory.clone()).await;
    let result = connect_async(ws_request(server.address)).await;
    match result {
        Err(WsError::Http(response)) => {
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE)
        }
        _ => panic!("unavailable synthetic authority upgraded"),
    }
    assert_eq!(factory.attempts.load(Ordering::SeqCst), 1);
    server.stop().await;
}

#[tokio::test]
async fn opaque_frames_roundtrip_and_invalid_or_binary_payloads_never_reach_the_pipe() {
    let (connection, peer) = pipe_pair();
    let mut server = FixtureServer::start(factory(vec![connection])).await;
    let (mut socket, _) = connect_async(ws_request(server.address)).await.unwrap();
    let request = r#"{ "id":1,"method":"future/unknown","params":{"opaque":123456789012345678901234567890,"value":"a\\b"}}"#;
    socket.send(Message::Text(request.into())).await.unwrap();
    assert_eq!(
        peer.receive(deadline()).await.unwrap().bytes(),
        request.as_bytes()
    );
    let response = r#"{"id":1, "result":{"encrypted_content":"opaque\\u0000","number":900719925474099312345}}"#;
    assert_eq!(
        peer.send(rpc(response), deadline()).await,
        SendOutcome::Written
    );
    assert_eq!(
        socket.next().await.unwrap().unwrap(),
        Message::Text(response.into())
    );
    socket
        .send(Message::Text(
            r#"{"id":1,"id":2,"method":"turn/start"}"#.into(),
        ))
        .await
        .unwrap();
    ws_terminal(&mut socket).await;
    assert!(peer.receive(deadline()).await.is_err());
    peer.close(deadline()).await;
    server.stop().await;

    let (connection, peer) = pipe_pair();
    let mut server = FixtureServer::start(factory(vec![connection])).await;
    let (mut socket, _) = connect_async(ws_request(server.address)).await.unwrap();
    socket
        .send(Message::Binary(b"binary_rpc".to_vec()))
        .await
        .unwrap();
    ws_terminal(&mut socket).await;
    assert!(peer.receive(deadline()).await.is_err());
    peer.close(deadline()).await;
    server.stop().await;
}

#[tokio::test]
async fn local_stop_and_uncertain_pipe_delivery_terminate_without_a_retry() {
    let (connection, peer) = pipe_pair();
    let mut server = FixtureServer::start(factory(vec![connection])).await;
    let (mut socket, _) = connect_async(ws_request(server.address)).await.unwrap();
    let admitted = r#"{"id":1,"method":"model/list","params":{}}"#;
    socket.send(Message::Text(admitted.into())).await.unwrap();
    assert_eq!(peer.receive(deadline()).await.unwrap().bytes(), admitted.as_bytes());
    server.cancel.cancel();
    server.stop().await;
    ws_terminal(&mut socket).await;
    assert!(peer.receive(deadline()).await.is_err());
    peer.close(deadline()).await;

    // One byte of capacity and no reader guarantee a partial length-header
    // delivery. The real LocalConnection then reports its deadline as unknown.
    let (left, mut stalled) = tokio::io::duplex(1);
    let connection = fixture_duplex_for_gateway(left, limits()).unwrap();
    let mut server = FixtureServer::start(factory(vec![connection])).await;
    let (mut socket, _) = connect_async(ws_request(server.address)).await.unwrap();
    socket
        .send(Message::Text(
            r#"{"id":1,"method":"turn/start","params":{}}"#.into(),
        ))
        .await
        .unwrap();
    ws_terminal(&mut socket).await;
    let mut partial = Vec::new();
    tokio::time::timeout(WAIT, stalled.read_to_end(&mut partial))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(partial.len(), 1);
    server.stop().await;
}

#[tokio::test]
async fn publication_stop_owns_the_whole_relay_and_terminates_its_pipe() {
    let (connection, peer) = pipe_pair();
    let cancel = SessionGatewayCancellation::new();
    let mut frontend = PublicationFrontend::start(
        app(factory(vec![connection]), &cancel),
        PublicationCancellation::new(),
    )
    .await
    .unwrap();
    let (mut socket, _) = connect_async(ws_request(frontend.address())).await.unwrap();
    let request = r#"{"id":1,"method":"model/list","params":{}}"#;
    socket.send(Message::Text(request.into())).await.unwrap();
    assert_eq!(
        peer.receive(deadline()).await.unwrap().bytes(),
        request.as_bytes()
    );
    tokio::time::timeout(WAIT, frontend.stop())
        .await
        .unwrap()
        .unwrap();
    ws_terminal(&mut socket).await;
    assert!(peer.receive(deadline()).await.is_err());
    peer.close(deadline()).await;
    assert_eq!(cancel.close(deadline()).await, SessionGatewayClosed::Closed);
}
