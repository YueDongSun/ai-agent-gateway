use super::{
    contract::*, store::lower_hex, ReceiptRegistry, ReceiptSecret, ReceiptVersion,
    RequestObservation, RECEIPT_CAPACITY, RETENTION_SECONDS,
};
use crate::proxy::server::ProxyAppState;
use axum::{
    body::{Body, Bytes},
    extract::{Path, Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
    Extension, Json, Router,
};
use futures_util::{stream::BoxStream, Stream, StreamExt};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

const OPT_IN_HEADER: &str = "x-aag-receipt";
const CAPABILITY_HEADER: &str = "x-aag-receipt-capability";
const PRECONDITION_HEADER: &str = "x-aag-route-precondition";
const VERSION_HEADER: &str = "x-aag-receipt-version";
const NONCE_HEADER: &str = "x-aag-receipt-nonce";
const REQUEST_HEADER: &str = "x-aag-request-id";

#[derive(Clone)]
struct ReceiptHttpState {
    registry: Option<Arc<ReceiptRegistry>>,
    gateway_key: Arc<str>,
}

pub(crate) fn attach(router: Router<ProxyAppState>, gateway_key: &str) -> Router<ProxyAppState> {
    let state = ReceiptHttpState {
        registry: ReceiptRegistry::new().ok(),
        gateway_key: Arc::from(gateway_key),
    };
    router
        .route("/_aag/v1/receipt_capabilities", get(capabilities))
        .route("/_aag/v1/receipts/:nonce", get(receipt))
        .route("/_aag/v2/receipt_capabilities", get(capabilities_v2))
        .route("/_aag/v2/receipts/:nonce", get(receipt_v2))
        .layer(Extension(state.clone()))
        .layer(middleware::from_fn_with_state(state, observe))
}

fn error(status: StatusCode, code: &'static str) -> Response {
    let mut response = (status, Json(serde_json::json!({"error":{"code":code,"type":"routing_receipt_error","message":code}}))).into_response();
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    response
}

pub(crate) fn admission_error(code: &'static str) -> Response {
    let status = if code == "receipt_route_snapshot_unavailable" {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::CONFLICT
    };
    error(status, code)
}

fn unique_header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?;
    if values.next().is_some() {
        return None;
    }
    value.to_str().ok()
}

fn authenticated(headers: &HeaderMap, expected: &str) -> bool {
    if expected.is_empty() {
        return false;
    }
    let provided = if headers.contains_key("authorization") {
        unique_header(headers, "authorization").and_then(|value| value.strip_prefix("Bearer "))
    } else {
        unique_header(headers, "x-api-key")
    };
    let Some(provided) = provided else {
        return false;
    };
    let Ok(mut expected_mac) = Hmac::<Sha256>::new_from_slice(expected.as_bytes()) else {
        return false;
    };
    let Ok(mut provided_mac) = Hmac::<Sha256>::new_from_slice(provided.as_bytes()) else {
        return false;
    };
    expected_mac.update(b"aag-receipt-gateway-auth-v1");
    provided_mac.update(b"aag-receipt-gateway-auth-v1");
    expected_mac
        .verify_slice(&provided_mac.finalize().into_bytes())
        .is_ok()
}

fn authorize(state: &ReceiptHttpState, headers: &HeaderMap) -> Result<(), Response> {
    if state.gateway_key.is_empty() {
        return Err(error(
            StatusCode::SERVICE_UNAVAILABLE,
            "receipt_auth_required",
        ));
    }
    if !authenticated(headers, &state.gateway_key) {
        return Err(error(StatusCode::UNAUTHORIZED, "authentication_failed"));
    }
    Ok(())
}

async fn capabilities(
    Extension(state): Extension<ReceiptHttpState>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, &headers) {
        return response;
    }
    let Some(registry) = state.registry else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "receipt_unavailable");
    };
    let mut response = Json(serde_json::json!({
        "schema_version":1,"issuer":"aag","supported_versions":[1],
        "server_instance_id":registry.instance_id(),"supported_surfaces":["chat","messages","responses","gemini"],
        "receipt_capacity":RECEIPT_CAPACITY,"max_retained_wire_attempts":MAX_ATTEMPTS,
        "terminal_retention_seconds":RETENTION_SECONDS,"upstream_model_attestation":"unavailable",
        "messages_credential_scope":"stored_key_only_snapshot"
    })).into_response();
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    response
}

async fn capabilities_v2(
    State(proxy_state): State<ProxyAppState>,
    Extension(state): Extension<ReceiptHttpState>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, &headers) {
        return response;
    }
    let Some(registry) = state.registry else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "receipt_unavailable");
    };
    let Some(live) = proxy_state.live.as_ref() else {
        return admission_error("receipt_route_snapshot_unavailable");
    };
    // Every field describing routing comes from this single immutable Arc.
    let snapshot = live.current();
    if snapshot.generation == 0 {
        return admission_error("receipt_route_snapshot_unavailable");
    }
    let mut response = Json(serde_json::json!({
        "schema_version":2,"issuer":"aag","supported_versions":[2],
        "server_instance_id":registry.instance_id(),"generation":snapshot.generation,
        "pinned_provider_present":snapshot.pinned_provider.is_some(),
        "supported_surfaces":["chat","messages"],
        "receipt_capacity":RECEIPT_CAPACITY,"max_retained_wire_attempts":MAX_ATTEMPTS,
        "terminal_retention_seconds":RETENTION_SECONDS,"upstream_model_attestation":"unavailable",
        "messages_credential_scope":"stored_key_only_snapshot"
    }))
    .into_response();
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    response
}

async fn receipt(
    Extension(state): Extension<ReceiptHttpState>,
    Path(nonce): Path<String>,
    headers: HeaderMap,
) -> Response {
    receipt_response(state, nonce, headers, ReceiptVersion::V1)
}

async fn receipt_v2(
    Extension(state): Extension<ReceiptHttpState>,
    Path(nonce): Path<String>,
    headers: HeaderMap,
) -> Response {
    receipt_response(state, nonce, headers, ReceiptVersion::V2)
}

fn receipt_response(
    state: ReceiptHttpState,
    nonce: String,
    headers: HeaderMap,
    version: ReceiptVersion,
) -> Response {
    if let Err(response) = authorize(&state, &headers) {
        return response;
    }
    let unavailable = || error(StatusCode::NOT_FOUND, "receipt_unavailable");
    if !lower_hex(&nonce, 32) {
        return unavailable();
    }
    let Some(secret) = unique_header(&headers, CAPABILITY_HEADER).and_then(ReceiptSecret::parse)
    else {
        return unavailable();
    };
    let Some(registry) = state.registry else {
        return unavailable();
    };
    let mut response = match version {
        ReceiptVersion::V1 => {
            let Some(receipt) = registry.lookup(&nonce, &secret) else {
                return unavailable();
            };
            Json(receipt).into_response()
        }
        ReceiptVersion::V2 => {
            let Some(receipt) = registry.lookup_v2(&nonce, &secret) else {
                return unavailable();
            };
            Json(receipt).into_response()
        }
    };
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    response
}

fn generation_surface(request: &Request) -> Option<&'static str> {
    if request.method() != Method::POST {
        return None;
    }
    match request.uri().path() {
        "/v1/chat/completions" => Some("chat"),
        "/v1/messages" => Some("messages"),
        "/v1/responses" => Some("responses"),
        path if (path.starts_with("/v1beta/models/") || path.starts_with("/v1/models/"))
            && (path.ends_with(":generateContent") || path.ends_with(":streamGenerateContent")) =>
        {
            Some("gemini")
        }
        _ => None,
    }
}

fn parse_opt_in(value: &str) -> Result<(ReceiptVersion, String, ReceiptSecret), &'static str> {
    if value.len() != 100 {
        return Err("receipt_header_invalid");
    }
    let mut fields = value.split('.');
    let version = match fields.next() {
        Some("v1") => ReceiptVersion::V1,
        Some("v2") => ReceiptVersion::V2,
        _ => return Err("receipt_version_unsupported"),
    };
    let nonce = fields
        .next()
        .filter(|value| lower_hex(value, 32))
        .ok_or("receipt_header_invalid")?;
    let secret = fields
        .next()
        .and_then(ReceiptSecret::parse)
        .ok_or("receipt_header_invalid")?;
    if fields.next().is_some() {
        return Err("receipt_header_invalid");
    }
    Ok((version, nonce.into(), secret))
}

fn parse_precondition(value: &str) -> Result<RouteAdmission, &'static str> {
    const INVALID: &str = "receipt_route_precondition_invalid";
    if value.len() > 58 {
        return Err(INVALID);
    }
    let mut fields = value.split('.');
    if fields.next() != Some("v2") {
        return Err(INVALID);
    }
    let instance = fields
        .next()
        .filter(|value| lower_hex(value, 32))
        .ok_or(INVALID)?;
    let generation = fields.next().ok_or(INVALID)?;
    if generation.is_empty()
        || generation.starts_with('0')
        || !generation.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(INVALID);
    }
    let generation = generation
        .parse::<u64>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or(INVALID)?;
    let pinned_provider_present = match fields.next() {
        Some("0") => false,
        Some("1") => true,
        _ => return Err(INVALID),
    };
    if fields.next().is_some() {
        return Err(INVALID);
    }
    Ok(RouteAdmission {
        server_instance_id: instance.into(),
        generation,
        pinned_provider_present,
    })
}

struct AdmissionGuard {
    observation: Option<RequestObservation>,
}
impl Drop for AdmissionGuard {
    fn drop(&mut self) {
        if let Some(observation) = self.observation.take() {
            observation.finish(None, BodyState::Dropped);
        }
    }
}

async fn observe(
    State(state): State<ReceiptHttpState>,
    mut request: Request,
    next: Next,
) -> Response {
    if !request.headers().contains_key(OPT_IN_HEADER) {
        if request.headers().contains_key(PRECONDITION_HEADER) {
            return error(
                StatusCode::BAD_REQUEST,
                "receipt_route_precondition_invalid",
            );
        }
        if request.headers().contains_key(CAPABILITY_HEADER)
            && !request.uri().path().starts_with("/_aag/v1/receipts/")
            && !request.uri().path().starts_with("/_aag/v2/receipts/")
        {
            return error(StatusCode::BAD_REQUEST, "receipt_surface_unsupported");
        }
        let advertise = request.method() == Method::GET
            && request.uri().path() == "/v1/models"
            && !state.gateway_key.is_empty()
            && state.registry.is_some();
        let mut response = next.run(request).await;
        if advertise && response.status().is_success() {
            response
                .headers_mut()
                .insert(VERSION_HEADER, HeaderValue::from_static("1"));
        }
        return response;
    }
    let Some(surface) = generation_surface(&request) else {
        return error(StatusCode::BAD_REQUEST, "receipt_surface_unsupported");
    };
    if let Err(response) = authorize(&state, request.headers()) {
        return response;
    }
    let Some(value) = unique_header(request.headers(), OPT_IN_HEADER) else {
        return error(StatusCode::BAD_REQUEST, "receipt_header_invalid");
    };
    let (version, nonce, secret) = match parse_opt_in(value) {
        Ok(value) => value,
        Err(code) => return error(StatusCode::BAD_REQUEST, code),
    };
    let expected_admission = match version {
        ReceiptVersion::V1 => {
            if request.headers().contains_key(PRECONDITION_HEADER) {
                return error(
                    StatusCode::BAD_REQUEST,
                    "receipt_route_precondition_invalid",
                );
            }
            None
        }
        ReceiptVersion::V2 => {
            if !matches!(surface, "chat" | "messages") {
                return error(StatusCode::BAD_REQUEST, "receipt_surface_unsupported");
            }
            let Some(value) = unique_header(request.headers(), PRECONDITION_HEADER) else {
                return error(
                    StatusCode::BAD_REQUEST,
                    "receipt_route_precondition_invalid",
                );
            };
            match parse_precondition(value) {
                Ok(expected) => Some(expected),
                Err(code) => return error(StatusCode::BAD_REQUEST, code),
            }
        }
    };
    if request.headers().contains_key(CAPABILITY_HEADER) {
        return error(StatusCode::BAD_REQUEST, "receipt_header_invalid");
    }
    // Consume the capability before any protocol handler or provider can see it.
    request.headers_mut().remove(OPT_IN_HEADER);
    request.headers_mut().remove(PRECONDITION_HEADER);
    let Some(registry) = state.registry else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "receipt_unavailable");
    };
    let request_id = crate::proxy::request_timeline::request_id();
    let reserved = match expected_admission {
        Some(expected) => registry.reserve_v2(&nonce, &secret, &request_id, surface, expected),
        None => registry.reserve(&nonce, &secret, &request_id, surface),
    };
    let observation = match reserved {
        Ok(observation) => observation,
        Err("receipt_nonce_reused") => return error(StatusCode::CONFLICT, "receipt_nonce_reused"),
        Err(code) => return error(StatusCode::SERVICE_UNAVAILABLE, code),
    };
    drop(secret);
    let mut guard = AdmissionGuard {
        observation: Some(observation.clone()),
    };
    let mut response = super::scope(observation.clone(), next.run(request)).await;
    guard.observation = None;
    response
        .headers_mut()
        .insert(VERSION_HEADER, HeaderValue::from_static(version.header()));
    response.headers_mut().insert(
        NONCE_HEADER,
        HeaderValue::from_str(&nonce).expect("validated hex nonce"),
    );
    if let Ok(value) = HeaderValue::from_str(&request_id) {
        response.headers_mut().insert(REQUEST_HEADER, value);
    }
    let status = response.status().as_u16();
    let (parts, body) = response.into_parts();
    Response::from_parts(
        parts,
        Body::from_stream(ReceiptBody {
            source: Some(body.into_data_stream().boxed()),
            observation,
            status,
            settled: false,
            body_error: false,
        }),
    )
}

struct ReceiptBody {
    source: Option<BoxStream<'static, Result<Bytes, axum::Error>>>,
    observation: RequestObservation,
    status: u16,
    settled: bool,
    body_error: bool,
}
impl ReceiptBody {
    fn finish(&mut self, body: BodyState) {
        if self.settled {
            return;
        }
        // Drop upstream owners before recording the HTTP body's terminal state.
        self.source = None;
        self.observation.finish(Some(self.status), body);
        self.settled = true;
    }
}
impl Stream for ReceiptBody {
    type Item = Result<Bytes, axum::Error>;
    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let Some(source) = this.source.as_mut() else {
            return Poll::Ready(None);
        };
        match source.as_mut().poll_next(context) {
            Poll::Ready(None) => {
                this.finish(if this.body_error {
                    BodyState::Error
                } else {
                    BodyState::Exhausted
                });
                Poll::Ready(None)
            }
            Poll::Ready(Some(Err(error))) => {
                this.body_error = true;
                Poll::Ready(Some(Err(error)))
            }
            other => other,
        }
    }
}
impl Drop for ReceiptBody {
    fn drop(&mut self) {
        self.finish(BodyState::Dropped);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn header_is_canonical_bounded_and_secret_debug_is_redacted() {
        let value = format!("v1.{}.{}", "1".repeat(32), "a".repeat(64));
        let (version, nonce, secret) = parse_opt_in(&value).unwrap();
        assert!(matches!(version, ReceiptVersion::V1));
        assert_eq!(nonce, "1".repeat(32));
        assert!(!format!("{secret:?}").contains(&"a".repeat(64)));
        for malformed in [
            format!("{value}, {value}"),
            value.replace("v1", "v3"),
            value.to_uppercase(),
            format!(" {value}"),
        ] {
            assert!(parse_opt_in(&malformed).is_err());
        }
    }
    #[test]
    fn gateway_auth_is_explicit_and_does_not_inherit_disabled_auth_or_passthrough() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", HeaderValue::from_static("Bearer fixture"));
        assert!(authenticated(&headers, "fixture"));
        assert!(!authenticated(&headers, ""));
        assert!(!authenticated(&headers, "other"));
        headers.append("authorization", HeaderValue::from_static("Bearer fixture"));
        assert!(!authenticated(&headers, "fixture"));
    }
}
