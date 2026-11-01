//! Strict authentication boundary for a Tailnet publication of the local API.
//! The underlying router and in-flight snapshots remain owned by the gateway.

mod connection_owner;
pub(crate) use connection_owner::{PublicationUpgradeError, PublicationUpgradeOwner};

use axum::{
    body::Body,
    extract::{Request, State},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    Router as AxumRouter,
};
use std::{
    io,
    net::{Ipv4Addr, SocketAddr},
    pin::Pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    task::{Context, Poll},
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::{TcpListener, TcpStream},
    sync::watch,
    task::{JoinHandle, JoinSet},
};
use tower::Service;

const MAX_PUBLICATION_CONNECTIONS: usize = 128;

/// Shared by native startup and the HTTP frontend. Cancelling is synchronous;
/// stop joins HTTP work and handlers registered through PublicationUpgradeOwner.
#[derive(Clone)]
pub(crate) struct PublicationCancellation {
    flag: Arc<AtomicBool>,
    signal: watch::Sender<bool>,
}

impl PublicationCancellation {
    pub fn new() -> Self {
        let (signal, _) = watch::channel(false);
        Self {
            flag: Arc::new(AtomicBool::new(false)),
            signal,
        }
    }

    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
        self.signal.send_replace(true);
    }

    pub fn flag(&self) -> &AtomicBool {
        &self.flag
    }

    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    async fn cancelled(&self) {
        let mut signal = self.signal.subscribe();
        while !*signal.borrow_and_update() {
            if signal.changed().await.is_err() {
                break;
            }
        }
    }
}

/// Uses the same admitted state and hot-routing owner as the local listener.
pub(crate) fn publication_router(
    state: super::server::ProxyAppState,
) -> Result<AxumRouter, String> {
    let key = state.auth_token.clone();
    protect_router(super::server::build_router(state), key)
}

pub(crate) struct PublicationFrontend {
    address: SocketAddr,
    owner: PublicationUpgradeOwner,
    task: Option<JoinHandle<io::Result<()>>>,
}

impl PublicationFrontend {
    pub async fn start(
        router: AxumRouter,
        cancellation: PublicationCancellation,
    ) -> Result<Self, String> {
        if cancellation.is_cancelled() {
            return Err("tailnet_publication_cancelled".into());
        }
        // This address is never supplied by a client or publication config.
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|_| "tailnet_publication_frontend_bind_failed")?;
        let address = listener
            .local_addr()
            .map_err(|_| "tailnet_publication_frontend_bind_failed")?;
        let task_cancellation = cancellation.clone();
        let owner = PublicationUpgradeOwner::new(cancellation.clone());
        let task = tokio::spawn(serve_frontend(
            listener,
            router,
            task_cancellation,
            owner.clone(),
        ));
        Ok(Self {
            address,
            owner,
            task: Some(task),
        })
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }

    pub fn is_finished(&self) -> bool {
        self.task.as_ref().is_none_or(JoinHandle::is_finished)
    }

    pub async fn stop(&mut self) -> Result<(), String> {
        self.owner.abort();
        if let Some(task) = self.task.as_mut() {
            let result = task
                .await
                .map_err(|_| "tailnet_publication_frontend_task_failed")
                .and_then(|result| {
                    result.map_err(|_| "tailnet_publication_frontend_accept_failed")
                });
            self.task = None;
            result?;
        }
        Ok(())
    }
}

impl Drop for PublicationFrontend {
    fn drop(&mut self) {
        self.owner.abort();
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

async fn serve_frontend(
    listener: TcpListener,
    router: AxumRouter,
    cancellation: PublicationCancellation,
    owner: PublicationUpgradeOwner,
) -> io::Result<()> {
    let _shutdown = connection_owner::FrontendGuard(owner.clone());
    let mut connections = JoinSet::new();
    let result = loop {
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => break Ok(()),
            _ = connections.join_next(), if !connections.is_empty() => {},
            _ = owner.capacity_available(), if !owner.has_capacity() => {},
            accepted = listener.accept(), if owner.has_capacity() => {
                match accepted {
                    Ok((stream, _)) => {
                        let (stream, guard) = match owner.attach(stream) {
                            Ok(value) => value,
                            Err(_) if cancellation.is_cancelled() => break Ok(()),
                            Err(error) => break Err(error),
                        };
                        let router = router.clone();
                        let cancellation = cancellation.clone();
                        let upgrade_owner = owner.clone();
                        connections.spawn(async move {
                            let service = hyper::service::service_fn(move |request: hyper::Request<hyper::body::Incoming>| {
                                let mut router = router.clone();
                                let cancellation = cancellation.clone();
                                let upgrade_owner = upgrade_owner.clone();
                                async move {
                                    // Refuse already-read requests when cancellation is
                                    // observed; stop also closes sockets and joins tasks.
                                    if cancellation.is_cancelled() {
                                        return Ok::<_, std::convert::Infallible>(
                                            axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response());
                                    }
                                    std::future::poll_fn(|context| {
                                        Service::<Request>::poll_ready(&mut router, context)
                                    }).await?;
                                    let mut request = request.map(Body::new);
                                    request.extensions_mut().insert(upgrade_owner);
                                    router.call(request).await
                                }
                            });
                            let _ = hyper::server::conn::http1::Builder::new()
                                .serve_connection(PublicationIo { stream, _guard: guard }, service)
                                .with_upgrades().await;
                        });
                    }
                    Err(error) => break Err(error),
                }
            }
        }
    };
    drop(listener);
    // Stop the actual sockets first, including IO already moved into upgrades.
    owner.abort();
    // Graceful HTTP shutdown can wait forever for SSE; abort and join it.
    connections.abort_all();
    while connections.join_next().await.is_some() {}
    owner.join_handlers().await;
    result
}

/// The existing Hyper dependency needs only the Tokio I/O trait bridge here.
struct PublicationIo {
    stream: TcpStream,
    _guard: connection_owner::ConnectionGuard,
}

impl hyper::rt::Read for PublicationIo {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        mut cursor: hyper::rt::ReadBufCursor<'_>,
    ) -> Poll<io::Result<()>> {
        let filled = {
            // Hyper exposes writable uninitialized capacity; Tokio initializes
            // exactly the filled range before we advance Hyper's cursor.
            let mut buffer = tokio::io::ReadBuf::uninit(unsafe { cursor.as_mut() });
            match Pin::new(&mut self.get_mut().stream).poll_read(context, &mut buffer) {
                Poll::Ready(Ok(())) => buffer.filled().len(),
                Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                Poll::Pending => return Poll::Pending,
            }
        };
        unsafe {
            cursor.advance(filled);
        }
        Poll::Ready(Ok(()))
    }
}

impl hyper::rt::Write for PublicationIo {
    fn poll_write(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().stream).poll_write(context, bytes)
    }
    fn poll_flush(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_flush(context)
    }
    fn poll_shutdown(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_shutdown(context)
    }
}

pub(crate) fn protect_router(
    router: AxumRouter,
    existing_key: String,
) -> Result<AxumRouter, String> {
    if existing_key.trim().is_empty() {
        return Err("tailnet_publication_existing_key_required".into());
    }
    // The outer fallback service covers unknown paths and method mismatches
    // too. New routes belong inside this boundary, never after its layer.
    Ok(AxumRouter::new()
        .fallback_service(router)
        .layer(middleware::from_fn_with_state(
            existing_key,
            require_existing_key,
        )))
}

async fn require_existing_key(
    State(existing_key): State<String>,
    request: Request,
    next: Next,
) -> Response {
    // Discovery exposes only a fixed product/version marker. All API data,
    // unknown paths, method mismatches and websocket upgrades retain auth.
    if request.method() == axum::http::Method::GET && request.uri().path() == "/aag/discovery" {
        return next.run(request).await;
    }
    if let Err(response) = super::server::check_publication_auth(&request, &existing_key) {
        return *response;
    }
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::{router::Router, server::ProxyAppState};
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use std::{
        collections::HashMap,
        sync::{atomic::AtomicBool, Arc, Mutex},
    };
    use tower::Service;

    const EXISTING_KEY: &str = "fixture_existing_gateway_key";

    #[tokio::test]
    async fn discovery_is_content_free_and_does_not_open_other_routes() {
        let router = protect_router(local_router(), EXISTING_KEY.into()).unwrap();
        let response = send(router.clone(), Request::builder().uri("/aag/discovery").body(Body::empty()).unwrap()).await;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 2048).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["service"], "ai-agent-gateway");
        assert!(value.get("runtime_kind").is_none());
        assert_eq!(value.as_object().unwrap().len(), 4);
        let response = send(
            router.clone(),
            Request::builder()
                .uri("/aag/discovery?runtime=1")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 2048).await.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["runtime_kind"], "desktop");
        assert_eq!(value.as_object().unwrap().len(), 5);
        for path in ["/health", "/v1/models", "/unknown"] {
            let response = send(router.clone(), Request::builder().uri(path).body(Body::empty()).unwrap()).await;
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
        let response = send(router, Request::builder().method("POST").uri("/aag/discovery").body(Body::empty()).unwrap()).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    async fn send(mut router: AxumRouter, request: Request<Body>) -> axum::response::Response {
        std::future::poll_fn(|context| Service::<Request<Body>>::poll_ready(&mut router, context))
            .await
            .unwrap();
        router.call(request).await.unwrap()
    }

    fn local_router() -> AxumRouter {
        super::super::server::build_router(ProxyAppState {
            router: Arc::new(Router::new(HashMap::new(), HashMap::new())),
            auth_token: EXISTING_KEY.into(),
            pinned_provider: Arc::new(Mutex::new(None)),
            responses_cot_enabled: Arc::new(AtomicBool::new(false)),
            passthrough_auth_allowed: true,
            db: None,
            live: None,
            allow_client_config_repair: false,
        })
    }

    #[test]
    fn publication_refuses_an_absent_existing_key() {
        for token in ["", "   "] {
            assert_eq!(
                protect_router(local_router(), token.into())
                    .err()
                    .as_deref(),
                Some("tailnet_publication_existing_key_required")
            );
        }
    }

    #[tokio::test]
    async fn every_published_surface_rejects_missing_or_wrong_gateway_credentials() {
        let router = protect_router(local_router(), EXISTING_KEY.into()).unwrap();
        for path in [
            "/health",
            "/v1/models",
            "/v1/chat/completions",
            "/v1/responses",
            "/v1/messages",
            "/v1/messages/count_tokens",
            "/claude-desktop/v1/messages",
            "/claude-desktop/v1/models",
            "/v1beta/models",
            "/v1beta/models/fixture:generateContent",
            "/unknown",
        ] {
            for credential in [None, Some("Bearer fixture_wrong_key")] {
                let mut request = Request::builder().uri(path);
                if let Some(credential) = credential {
                    request = request.header("authorization", credential);
                }
                let response = send(router.clone(), request.body(Body::empty()).unwrap()).await;
                assert_eq!(
                    response.status(),
                    StatusCode::UNAUTHORIZED,
                    "path {path}, credential {credential:?}"
                );
            }
        }
    }

    #[tokio::test]
    async fn the_existing_key_and_sdk_authentication_forms_still_reach_the_local_routes() {
        let router = protect_router(local_router(), EXISTING_KEY.into()).unwrap();
        for (path, name, value) in [
            ("/health", "authorization", format!("Bearer {EXISTING_KEY}")),
            ("/v1/models", "x-api-key", EXISTING_KEY.into()),
            ("/v1beta/models", "x-goog-api-key", EXISTING_KEY.into()),
            (
                "/v1beta/models",
                "authorization",
                format!("Bearer {EXISTING_KEY}"),
            ),
        ] {
            let request = Request::builder()
                .uri(path)
                .header(name, value)
                .body(Body::empty())
                .unwrap();
            let response = send(router.clone(), request).await;
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "path {path}, header {name}"
            );
        }
        let request = Request::builder()
            .uri(format!("/v1beta/models?key={EXISTING_KEY}"))
            .body(Body::empty())
            .unwrap();
        assert_eq!(send(router, request).await.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn publication_keeps_existing_credential_precedence() {
        let router = protect_router(local_router(), EXISTING_KEY.into()).unwrap();
        let request = Request::builder()
            .uri("/health")
            .header("authorization", "Bearer fixture_wrong_key")
            .header("x-api-key", EXISTING_KEY)
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            send(router, request).await.status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn publication_frontend_shares_the_local_state_without_stopping_the_local_router() {
        let cancellation = PublicationCancellation::new();
        let mut frontend = PublicationFrontend::start(
            protect_router(local_router(), EXISTING_KEY.into()).unwrap(),
            cancellation,
        )
        .await
        .unwrap();
        assert!(frontend.address().ip().is_loopback());
        let client = reqwest::Client::new();
        let endpoint = format!("http://{}/health", frontend.address());
        assert_eq!(
            client.get(&endpoint).send().await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            client
                .get(&endpoint)
                .bearer_auth(EXISTING_KEY)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        let address = frontend.address();
        frontend.stop().await.unwrap();
        assert!(TcpStream::connect(address).await.is_err());
        assert_eq!(
            send(
                local_router(),
                Request::builder()
                    .uri("/health")
                    .header("authorization", format!("Bearer {EXISTING_KEY}"))
                    .body(Body::empty())
                    .unwrap()
            )
            .await
            .status(),
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn publication_stop_closes_authenticated_websocket_and_its_owned_handler() {
        use axum::{
            extract::ws::{Message as ServerMessage, WebSocketUpgrade},
            routing::get,
            Extension,
        };
        use futures::{SinkExt, StreamExt};
        use std::{sync::atomic::AtomicUsize, time::Duration};
        use tokio_tungstenite::{
            connect_async,
            tungstenite::{
                client::IntoClientRequest, Error as ClientError, Message as ClientMessage,
            },
        };

        const WAIT: Duration = Duration::from_secs(3);
        struct HandlerDropped(Arc<AtomicUsize>);
        impl Drop for HandlerDropped {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let dropped = Arc::new(AtomicUsize::new(0));
        let parked = Arc::new(tokio::sync::Notify::new());
        let route_dropped = dropped.clone();
        let route_parked = parked.clone();
        let local = local_router();
        let router = local.clone().route(
            "/fixture/ws",
            get(
                move |upgrade: WebSocketUpgrade,
                      Extension(owner): Extension<PublicationUpgradeOwner>| {
                    let dropped = route_dropped.clone();
                    let parked = route_parked.clone();
                    async move {
                        upgrade.on_upgrade(move |mut socket| async move {
                            owner
                                .spawn(async move {
                                    let _guard = HandlerDropped(dropped);
                                    let Some(Ok(ServerMessage::Text(text))) = socket.recv().await
                                    else {
                                        return;
                                    };
                                    if socket.send(ServerMessage::Text(text)).await.is_err() {
                                        return;
                                    }
                                    parked.notify_one();
                                    // Socket EOF alone cannot finish this future. The frontend
                                    // must cancel and join the registered upgrade handler.
                                    std::future::pending::<()>().await;
                                    drop(socket);
                                })
                                .expect("frontend must own the admitted upgrade handler");
                        })
                    }
                },
            ),
        );
        let mut frontend = PublicationFrontend::start(
            protect_router(router, EXISTING_KEY.into()).unwrap(),
            PublicationCancellation::new(),
        )
        .await
        .unwrap();
        let endpoint = format!("ws://{}/fixture/ws", frontend.address());
        for credential in [None, Some("Bearer fixture_wrong_key")] {
            let mut request = endpoint.as_str().into_client_request().unwrap();
            if let Some(credential) = credential {
                request
                    .headers_mut()
                    .insert("authorization", credential.parse().unwrap());
            }
            let result = tokio::time::timeout(WAIT, connect_async(request))
                .await
                .unwrap();
            match result {
                Err(ClientError::Http(response)) => {
                    assert_eq!(response.status(), StatusCode::UNAUTHORIZED)
                }
                _ => panic!("WebSocket publication admitted an absent or wrong gateway key"),
            }
        }
        let mut request = endpoint.as_str().into_client_request().unwrap();
        request.headers_mut().insert(
            "authorization",
            format!("Bearer {EXISTING_KEY}").parse().unwrap(),
        );
        let (mut socket, response) = tokio::time::timeout(WAIT, connect_async(request))
            .await
            .unwrap()
            .expect("authenticated WebSocket must upgrade before publication stop");
        assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
        tokio::time::timeout(
            WAIT,
            socket.send(ClientMessage::Text("fixture_echo".into())),
        )
        .await
        .unwrap()
        .unwrap();
        let echoed = tokio::time::timeout(WAIT, socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(echoed, ClientMessage::Text("fixture_echo".into()));
        tokio::time::timeout(WAIT, parked.notified()).await.unwrap();
        assert_eq!(dropped.load(Ordering::SeqCst), 0);

        let address = frontend.address();
        tokio::time::timeout(WAIT, frontend.stop())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            dropped.load(Ordering::SeqCst),
            1,
            "registered upgrade handler survived stop"
        );
        tokio::time::timeout(WAIT, async {
            loop {
                match socket.next().await {
                    None | Some(Err(_)) => break,
                    Some(Ok(ClientMessage::Close(_))) => {}
                    Some(Ok(_)) => panic!("upgraded connection retained access after stop"),
                }
            }
        })
        .await
        .expect("upgraded socket survived publication stop");
        assert!(tokio::time::timeout(WAIT, TcpStream::connect(address))
            .await
            .unwrap()
            .is_err());
        assert_eq!(
            send(
                local,
                Request::builder()
                    .uri("/health")
                    .header("authorization", format!("Bearer {EXISTING_KEY}"))
                    .body(Body::empty())
                    .unwrap()
            )
            .await
            .status(),
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn upgraded_websockets_retain_publication_capacity_until_their_io_drops() {
        use axum::{
            extract::ws::{Message as ServerMessage, WebSocketUpgrade},
            routing::get,
            Extension,
        };
        use futures::{SinkExt, StreamExt};
        use std::time::Duration;
        use tokio_tungstenite::{
            connect_async,
            tungstenite::{client::IntoClientRequest, Message as ClientMessage},
        };

        const WAIT: Duration = Duration::from_secs(3);
        let router = AxumRouter::new().route(
            "/fixture/ws",
            get(
                |upgrade: WebSocketUpgrade,
                 Extension(owner): Extension<PublicationUpgradeOwner>| async move {
                    upgrade.on_upgrade(move |mut socket| async move {
                        owner
                            .spawn(async move {
                                while let Some(Ok(message)) = socket.recv().await {
                                    match message {
                                        ServerMessage::Text(text) => {
                                            if socket.send(ServerMessage::Text(text)).await.is_err()
                                            {
                                                break;
                                            }
                                        }
                                        ServerMessage::Close(_) => break,
                                        _ => {}
                                    }
                                }
                            })
                            .expect("frontend must own each admitted upgrade handler");
                    })
                },
            ),
        );
        let mut frontend = PublicationFrontend::start(
            protect_router(router, EXISTING_KEY.into()).unwrap(),
            PublicationCancellation::new(),
        )
        .await
        .unwrap();
        let endpoint = format!("ws://{}/fixture/ws", frontend.address());
        let request = || {
            let mut request = endpoint.as_str().into_client_request().unwrap();
            request.headers_mut().insert(
                "authorization",
                format!("Bearer {EXISTING_KEY}").parse().unwrap(),
            );
            request
        };
        let mut clients = Vec::new();
        tokio::time::timeout(Duration::from_secs(10), async {
            for _ in 0..MAX_PUBLICATION_CONNECTIONS {
                let (mut socket, response) = connect_async(request()).await.unwrap();
                assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
                socket
                    .send(ClientMessage::Text("occupied".into()))
                    .await
                    .unwrap();
                assert_eq!(
                    socket.next().await.unwrap().unwrap(),
                    ClientMessage::Text("occupied".into())
                );
                clients.push(socket);
            }
        })
        .await
        .expect("could not establish the bounded upgrade fixture");
        // HTTP handshakes have finished, but their upgraded IO still consumes
        // every connection slot. Keep this attempt pending, never replay it.
        let extra = connect_async(request());
        tokio::pin!(extra);
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut extra)
                .await
                .is_err(),
            "completed HTTP handshakes incorrectly released upgraded capacity"
        );
        drop(clients.pop().unwrap());
        let (mut admitted, response) = tokio::time::timeout(WAIT, &mut extra)
            .await
            .expect("dropping upgraded IO did not release its connection slot")
            .unwrap();
        assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
        admitted
            .send(ClientMessage::Text("released".into()))
            .await
            .unwrap();
        assert_eq!(
            tokio::time::timeout(WAIT, admitted.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            ClientMessage::Text("released".into())
        );
        tokio::time::timeout(WAIT, frontend.stop())
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn publication_stop_drops_pending_http_sse_and_idle_connections() {
        use axum::routing::get;
        use std::{convert::Infallible, sync::atomic::AtomicUsize, time::Duration};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        struct Dropped(Arc<AtomicUsize>);
        impl Drop for Dropped {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let dropped = Arc::new(AtomicUsize::new(0));
        let pending_entered = Arc::new(tokio::sync::Notify::new());
        let pending_dropped = dropped.clone();
        let entered = pending_entered.clone();
        let sse_dropped = dropped.clone();
        let router = AxumRouter::new()
            .route("/pending", get(move || {
                let guard = Dropped(pending_dropped.clone());
                let entered = entered.clone();
                async move {
                    let _guard = guard;
                    entered.notify_one();
                    std::future::pending::<Response>().await
                }
            }))
            .route("/sse", get(move || {
                let guard = Dropped(sse_dropped.clone());
                async move {
                    let stream = async_stream::stream! {
                        let _guard = guard;
                        yield Ok::<_, Infallible>(bytes::Bytes::from_static(b"data: fixture\n\n"));
                        std::future::pending::<()>().await;
                    };
                    ([ ("content-type", "text/event-stream") ], Body::from_stream(stream))
                }
            }));
        let mut frontend = PublicationFrontend::start(
            protect_router(router, EXISTING_KEY.into()).unwrap(),
            PublicationCancellation::new(),
        )
        .await
        .unwrap();
        let address = frontend.address();
        let mut pending = TcpStream::connect(address).await.unwrap();
        pending.write_all(format!("GET /pending HTTP/1.1\r\nHost: fixture\r\nAuthorization: Bearer {EXISTING_KEY}\r\n\r\n").as_bytes()).await.unwrap();
        tokio::time::timeout(Duration::from_secs(3), pending_entered.notified())
            .await
            .unwrap();
        let mut sse = TcpStream::connect(address).await.unwrap();
        sse.write_all(format!("GET /sse HTTP/1.1\r\nHost: fixture\r\nAuthorization: Bearer {EXISTING_KEY}\r\n\r\n").as_bytes()).await.unwrap();
        let mut received = Vec::new();
        tokio::time::timeout(Duration::from_secs(3), async {
            while !received
                .windows(b"data: fixture".len())
                .any(|window| window == b"data: fixture")
            {
                let mut buffer = [0; 1024];
                let count = sse.read(&mut buffer).await.unwrap();
                assert_ne!(count, 0);
                received.extend_from_slice(&buffer[..count]);
            }
        })
        .await
        .unwrap();
        let mut idle = TcpStream::connect(address).await.unwrap();
        idle.write_all(b"GET /pending HTTP/1.1\r\n").await.unwrap();
        tokio::time::timeout(Duration::from_secs(3), frontend.stop())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(dropped.load(Ordering::SeqCst), 2);
        for stream in [&mut pending, &mut sse, &mut idle] {
            tokio::time::timeout(Duration::from_secs(3), async {
                let mut buffer = [0; 128];
                loop {
                    match stream.read(&mut buffer).await {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                }
            })
            .await
            .expect("connection survived publication stop");
        }
        assert!(TcpStream::connect(address).await.is_err());
    }
}
