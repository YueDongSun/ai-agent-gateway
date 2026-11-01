//! Request-scoped metadata observation; body lifetime includes disconnects.
#![allow(non_camel_case_types)]
use axum::{body::Body, extract::Request, middleware::Next, response::Response};
use futures_util::StreamExt;
use std::sync::{Arc, Mutex};
use std::time::Instant;

tokio::task_local! {
    static CURRENT_REQUEST: Arc<Mutex<Request_metadata>>;
}

struct Request_metadata {
    request_id: String,
    model_ref: String,
    surface: &'static str,
    hops_seen: usize,
    stream_status: Option<u16>,
    plan: crate::diagnostics::Plan_trace,
    config_generation: Option<u64>,
    /// Set when a mirror role request was served from a lower-tier role's
    /// supply; the surface relays it as the `x-aag-role-degraded` header.
    role_degraded: Option<String>,
}

pub(crate) fn request_id() -> String {
    CURRENT_REQUEST.try_with(|current| current.lock().unwrap_or_else(|error| error.into_inner()).request_id.clone())
        .unwrap_or_else(|_| crate::diagnostics::next_request_id())
}

pub(crate) fn observe_model(model: &str) {
    let _ = CURRENT_REQUEST.try_with(|current| {
        let mut current = current.lock().unwrap_or_else(|error| error.into_inner());
        current.model_ref = crate::diagnostics::safe_reference(model);
        super::request_status::begin(&current.request_id, current.surface, model, current.config_generation);
    });
}

/// Observe only an admitted provider call, after request construction succeeds.
pub(crate) fn observe_target(requested_model: &str, target_model: &str) {
    observe_model(requested_model);
    let _ = CURRENT_REQUEST.try_with(|current| {
        let current = current.lock().unwrap_or_else(|error| error.into_inner());
        super::request_status::target(&current.request_id, target_model);
    });
}

pub(crate) fn observe_failure(code: &str, status: Option<u16>) {
    Stream_observation::capture().failure(code, status);
}

pub(crate) fn observe_hop() {
    let _ = CURRENT_REQUEST.try_with(|current| {
        let mut current = current.lock().unwrap_or_else(|error| error.into_inner());
        current.hops_seen = current.hops_seen.saturating_add(1);
    });
}

/// Record the capability-routing decision for this request (content-free
/// codes produced by the planner). Only the last decision before the
/// response settles is kept — one event per request.
pub(crate) fn observe_plan(plan: crate::diagnostics::Plan_trace) {
    Stream_observation::capture().selected_plan(plan);
}

/// Pin the routing generation the request was admitted under, so the
/// finished event is never explained by a later routing table.
pub(crate) fn observe_generation(generation: u64) {
    let _ = CURRENT_REQUEST.try_with(|current| {
        let mut current = current.lock().unwrap_or_else(|error| error.into_inner());
        current.config_generation = Some(generation);
        super::request_status::generation(&current.request_id, generation);
    });
}

/// Record that the requested mirror role had no supply and a lower-tier role
/// of the same vendor served the request instead.
pub(crate) fn observe_role_degradation(requested_role: &str) {
    let _ = CURRENT_REQUEST.try_with(|current| {
        let mut current = current.lock().unwrap_or_else(|error| error.into_inner());
        current.role_degraded = Some(requested_role.to_string());
    });
}

/// The degraded role for this request, if any; consumed by the surface when
/// it attaches the `x-aag-role-degraded` response header.
pub(crate) fn role_degradation() -> Option<String> {
    CURRENT_REQUEST
        .try_with(|current| {
            current
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .role_degraded
                .clone()
        })
        .ok()
        .flatten()
}

pub(crate) struct Stream_observation(Option<Arc<Mutex<Request_metadata>>>);
impl Stream_observation {
    pub(crate) fn capture() -> Self {
        Self(CURRENT_REQUEST.try_with(Arc::clone).ok())
    }
    pub(crate) fn selected_plan(&self, plan: crate::diagnostics::Plan_trace) {
        if let Some(current) = &self.0 {
            current.lock().unwrap_or_else(|error| error.into_inner()).plan = plan;
        }
    }
    pub(crate) fn failed(&self) {
        if let Some(current) = &self.0 {
            let mut current = current.lock().unwrap_or_else(|error| error.into_inner());
            current.stream_status = Some(502);
            super::request_status::failed(&current.request_id, "upstream_stream_error", Some(502));
        }
    }
    pub(crate) fn failure(&self, code: &str, status: Option<u16>) {
        if let Some(current) = &self.0 {
            let current = current.lock().unwrap_or_else(|error| error.into_inner());
            super::request_status::failed(&current.request_id, code, status);
        }
    }
    pub(crate) fn streaming(&self) {
        if let Some(current) = &self.0 {
            let current = current.lock().unwrap_or_else(|error| error.into_inner());
            super::request_status::streaming(&current.request_id);
        }
    }
}

struct Response_observation {
    metadata: Arc<Mutex<Request_metadata>>,
    started: Instant,
    status: u16,
    streamed: bool,
    exhausted: bool,
}
impl Drop for Response_observation {
    fn drop(&mut self) {
        let metadata = self.metadata.lock().unwrap_or_else(|error| error.into_inner());
        let status = metadata.stream_status.unwrap_or({
            if !self.exhausted && self.streamed && self.status < 400 { 499 } else { self.status }
        });
        super::request_status::finish(&metadata.request_id, status);
        crate::diagnostics::record_request_event_with_model_ref(
            &metadata.request_id, metadata.surface, &metadata.model_ref, status,
            self.started.elapsed().as_millis().min(u64::MAX as u128) as u64,
            self.streamed, (metadata.hops_seen > 0).then_some(metadata.hops_seen > 1),
            &metadata.plan, metadata.config_generation,
        );
    }
}

pub(crate) async fn observe_request(request: Request, next: Next) -> Response {
    let surface = match request.uri().path() {
        "/v1/chat/completions" => "chat",
        "/v1/messages" | "/v1/messages/count_tokens" => "messages",
        "/v1/responses" => "responses",
        "/v1/models" | "/v1beta/models" => "models",
        path if path.starts_with("/v1beta/models/") || path.starts_with("/v1/models/") => "gemini",
        _ => return next.run(request).await,
    };
    let metadata = Arc::new(Mutex::new(Request_metadata {
        request_id: crate::diagnostics::next_request_id(),
        model_ref: crate::diagnostics::safe_reference(""), surface,
        hops_seen: 0, stream_status: None,
        plan: crate::diagnostics::Plan_trace::default(),
        config_generation: None,
        role_degraded: None,
    }));
    // The guard also owns cancellation while the handler is waiting upstream.
    let mut observation = Response_observation {
        metadata: Arc::clone(&metadata), started: Instant::now(), status: 499,
        streamed: false, exhausted: false,
    };
    let response = CURRENT_REQUEST.scope(Arc::clone(&metadata), next.run(request)).await;
    let streamed = response.headers().get("content-type").and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("text/event-stream"));
    observation.status = response.status().as_u16();
    observation.streamed = streamed;
    observation.exhausted = !streamed;
    if !streamed {
        return response;
    }
    let (parts, body) = response.into_parts();
    // Own the guard before the generator starts, so an unread body is recorded too.
    let stream = async_stream::stream! {
        let mut observation = observation;
        let mut source = body.into_data_stream();
        while let Some(chunk) = source.next().await {
            if chunk.is_err() { observation.status = 502; }
            yield chunk;
        }
        observation.exhausted = true;
    };
    Response::from_parts(parts, Body::from_stream(stream))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{routing::post, Router};
    use tower::Service;

    async fn respond() -> Response {
        observe_model("https://private.invalid/model?token=fixture");
        observe_hop();
        observe_hop();
        let observation = Stream_observation::capture();
        let source = async_stream::stream! {
            yield Ok::<_, std::io::Error>(bytes::Bytes::from_static(b"data: fixture

"));
            observation.failed();
        };
        Response::builder().header("content-type", "text/event-stream")
            .body(Body::from_stream(source)).unwrap()
    }

    #[tokio::test]
    async fn timeline_records_unread_stream_as_cancelled_not_success() {
        crate::diagnostics::clear_request_timeline();
        let mut app = Router::new().route("/v1/chat/completions", post(respond))
            .layer(axum::middleware::from_fn(observe_request));
        let response = app.call(Request::builder().uri("/v1/chat/completions")
            .method("POST").body(Body::empty()).unwrap()).await.unwrap();
        assert!(crate::diagnostics::request_timeline().is_empty());
        drop(response);
        let events = crate::diagnostics::request_timeline();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].status, 499);
        assert_eq!(events[0].failed_over, Some(true));
        assert_eq!(events[0].model_ref, crate::diagnostics::safe_reference("https://private.invalid/model?token=fixture"));
        assert!(!serde_json::to_string(&events).unwrap().contains("private.invalid"));
        let status = super::super::request_status::snapshot().into_iter()
            .find(|row| row.request_id == events[0].request_id).unwrap();
        assert_eq!(status.phase, "cancelled");
        assert_eq!(status.error_code.as_deref(), Some("request_cancelled"));
    }

    #[tokio::test]
    async fn timeline_records_stream_error_after_body_consumption() {
        crate::diagnostics::clear_request_timeline();
        let mut app = Router::new().route("/v1/chat/completions", post(respond))
            .layer(axum::middleware::from_fn(observe_request));
        let response = app.call(Request::builder().uri("/v1/chat/completions")
            .method("POST").body(Body::empty()).unwrap()).await.unwrap();
        axum::body::to_bytes(response.into_body(), 4096).await.unwrap();
        let events = crate::diagnostics::request_timeline();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].status, 502);
        assert!(events[0].streamed);
        assert_eq!(events[0].failed_over, Some(true));
        let status = super::super::request_status::snapshot().into_iter()
            .find(|row| row.request_id == events[0].request_id).unwrap();
        assert_eq!(status.phase, "error");
        assert_eq!(status.error_code.as_deref(), Some("upstream_stream_error"));
    }

    #[tokio::test]
    async fn timeline_records_consumed_success_once() {
        crate::diagnostics::clear_request_timeline();
        let mut app = Router::new().route("/v1/messages", post(|| async {
            observe_model("fixture_model");
            observe_hop();
            Response::builder().header("content-type", "text/event-stream")
                .body(Body::from("event: message_stop\ndata: {}\n\n")).unwrap()
        })).layer(axum::middleware::from_fn(observe_request));
        let response = app.call(Request::builder().uri("/v1/messages")
            .method("POST").body(Body::empty()).unwrap()).await.unwrap();
        assert!(crate::diagnostics::request_timeline().is_empty());
        axum::body::to_bytes(response.into_body(), 4096).await.unwrap();
        let events = crate::diagnostics::request_timeline();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].status, 200);
        assert!(events[0].streamed);
        assert_eq!(events[0].failed_over, Some(false));
        let status = super::super::request_status::snapshot().into_iter()
            .find(|row| row.request_id == events[0].request_id).unwrap();
        assert_eq!(status.phase, "completed");
    }

    #[tokio::test]
    async fn timeline_keeps_http_failure_and_correlation_id() {
        crate::diagnostics::clear_request_timeline();
        let mut app = Router::new().route("/v1/messages", post(|| async {
            Response::builder().status(401).body(Body::from(request_id())).unwrap()
        })).layer(axum::middleware::from_fn(observe_request));
        let response = app.call(Request::builder().uri("/v1/messages")
            .method("POST").body(Body::empty()).unwrap()).await.unwrap();
        let body = axum::body::to_bytes(response.into_body(), 4096).await.unwrap();
        let events = crate::diagnostics::request_timeline();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].status, 401);
        assert_eq!(events[0].request_id, String::from_utf8(body.to_vec()).unwrap());
        assert_eq!(events[0].failed_over, None);
    }

    #[tokio::test]
    async fn live_status_cancels_requests_waiting_for_upstream_headers() {
        let started = Arc::new(tokio::sync::Notify::new());
        let observed_id = Arc::new(Mutex::new(None::<String>));
        let mut app = Router::new().route("/v1/responses", post({
            let started = Arc::clone(&started);
            let observed_id = Arc::clone(&observed_id);
            move || {
                let started = Arc::clone(&started);
                let observed_id = Arc::clone(&observed_id);
                async move {
                    observe_target("5.6sol", "concrete_model");
                    *observed_id.lock().unwrap() = Some(request_id());
                    started.notify_one();
                    std::future::pending::<Response>().await
                }
            }
        })).layer(axum::middleware::from_fn(observe_request));
        let operation = tokio::spawn(async move {
            app.call(Request::builder().uri("/v1/responses").method("POST")
                .body(Body::empty()).unwrap()).await
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), started.notified()).await.unwrap();
        let request_id = observed_id.lock().unwrap().clone().unwrap();
        let active = super::super::request_status::snapshot().into_iter()
            .find(|row| row.request_id == request_id).unwrap();
        assert_eq!(active.phase, "requesting");
        assert_eq!(active.resolved_model.as_deref(), Some("concrete_model"));
        operation.abort();
        assert!(operation.await.unwrap_err().is_cancelled());
        let cancelled = super::super::request_status::snapshot().into_iter()
            .find(|row| row.request_id == request_id).unwrap();
        assert_eq!(cancelled.phase, "cancelled");
        assert_eq!(cancelled.status, Some(499));
    }
}
