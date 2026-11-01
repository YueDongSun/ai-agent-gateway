//! Bounded session preferences and hard native Responses source ownership.
//!
//! Session preferences require an explicit Anthropic `metadata.user_id` or
//! OpenAI `user` marker, retain an eligible provider+model pair, and may fail
//! over. Server-side Responses references and encrypted items instead constrain
//! routing to their originating transport. Inline message and function-item ids
//! are metadata when their complete contents are present. Both tables are caller-scoped,
//! memory-only, and bounded by capacity and TTL. Only hashed source references
//! are retained.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::database::dao::routes::RouteHop;
use crate::proxy::canonical::ChatCompletionRequest;

const CAPACITY: usize = 2048;
const TTL: Duration = Duration::from_secs(30 * 60);

#[derive(Clone)]
struct Binding {
    provider: String,
    model: String,
    expires_at: Instant,
}

#[derive(Clone, Copy)]
#[allow(non_camel_case_types)]
pub(crate) struct Provider_binding {
    pub identity: u64,
    pub native_responses: bool,
}

pub(crate) fn next_transport_identity() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(non_camel_case_types)]
pub(crate) struct Source_target {
    pub provider: String,
    pub model: String,
    pub identity: u64,
}

#[derive(Clone)]
#[allow(non_camel_case_types)]
struct Source_binding {
    target: Source_target,
    expires_at: Instant,
}

#[derive(Default)]
#[allow(non_camel_case_types)]
pub(crate) struct Session_affinity {
    inner: Mutex<HashMap<(String, String), Binding>>,
    sources: Mutex<HashMap<(String, String), Source_binding>>,
}

/// The request's call identity + session identity, both as hashed refs.
/// `None` when the request carries no explicit session marker.
pub(crate) fn affinity_key(req: &ChatCompletionRequest) -> Option<(String, String)> {
    let session = session_ref(req)?;
    Some((client_ref(req), session))
}

/// Explicit session markers only. The per-request `request_id` is
/// deliberately NOT consulted.
fn session_ref(req: &ChatCompletionRequest) -> Option<String> {
    // Responses ids constrain source ownership, not ordinary session affinity.
    if let Some(value) = req
        .source_anthropic_request
        .as_ref()
        .and_then(|source| source.get("metadata"))
        .and_then(|metadata| metadata.get("user_id"))
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
    {
        return Some(crate::diagnostics::safe_reference(value));
    }
    if let Some(value) = req
        .extra
        .get("user")
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
    {
        return Some(crate::diagnostics::safe_reference(value));
    }
    None
}

/// Call identity for isolation. Absent credentials collapse to one local
/// identity; raw credential material is never stored.
pub(crate) fn client_ref(req: &ChatCompletionRequest) -> String {
    match req.client_credential.as_ref() {
        Some(credential) => {
            let mut identity = String::new();
            if credential.passthrough_only {
                identity.push_str("passthrough|");
            }
            if let Some(token) = &credential.authorization {
                identity.push_str(token);
                identity.push('|');
            }
            if let Some(key) = &credential.x_api_key {
                identity.push_str(key);
            }
            crate::diagnostics::safe_reference(&identity)
        }
        None => "local".to_string(),
    }
}

fn source_ref(kind: &str, value: &str) -> String {
    crate::diagnostics::safe_reference(&format!("{kind}|{value}"))
}

fn materialized_item(item: &serde_json::Value) -> bool {
    use serde_json::Value;
    if item.get("encrypted_content").is_some_and(|value| !value.is_null())
        || item.get("status").is_some_and(|status| status.as_str() != Some("completed"))
    {
        return false;
    }
    let nonempty_string = |key| item.get(key).and_then(Value::as_str).is_some_and(|value| !value.is_empty());
    match item.get("type") {
        None | Some(Value::String(_)) if item.get("type").is_none_or(|kind| kind.as_str() == Some("message")) => {
            matches!(item.get("role").and_then(Value::as_str), Some("user" | "assistant" | "developer" | "system"))
                && item.get("content").is_some_and(|content| content.is_string() || content.is_array())
        }
        Some(Value::String(kind)) if kind == "function_call" => {
            nonempty_string("call_id") && nonempty_string("name")
                && item.get("arguments").and_then(Value::as_str)
                    .is_some_and(|arguments| serde_json::from_str::<Value>(arguments).is_ok())
        }
        Some(Value::String(kind)) if kind == "function_call_output" => {
            nonempty_string("call_id") && item.get("output").is_some_and(|output| {
                match output {
                    Value::Array(parts) => parts.iter().all(|part| {
                        matches!(part.get("type").and_then(Value::as_str), Some("input_text" | "output_text" | "text"))
                            && part.get("text").and_then(Value::as_str).is_some()
                    }),
                    _ => true,
                }
            })
        }
        _ => false,
    }
}

pub(crate) fn source_error(code: &str) -> crate::proxy::canonical::ProviderError {
    crate::proxy::canonical::make_provider_error(
        code,
        "source_binding",
        Some(409),
        Some(false),
        None,
    )
}

impl Session_affinity {
    /// The bound provider for this key, if the binding is still live.
    pub(crate) fn bound_hop(&self, key: &(String, String)) -> Option<RouteHop> {
        let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        match inner.get(key) {
            Some(binding) if binding.expires_at > Instant::now() => {
                Some(RouteHop {
                    provider: binding.provider.clone(),
                    model: binding.model.clone(),
                })
            }
            Some(_) => {
                inner.remove(key);
                None
            }
            None => None,
        }
    }

    /// (Re)bind a session to the provider that actually served it. Only
    /// called from the success path: a failure-only switch rebinds to the
    /// failover winner instead of ping-ponging between providers.
    pub(crate) fn record_winner(&self, key: (String, String), provider: &str, model: &str) {
        let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        // Drop expired entries first so capacity counts live bindings only.
        let now = Instant::now();
        inner.retain(|_, binding| binding.expires_at > now);
        if inner.len() >= CAPACITY {
            // Evict the binding closest to expiry; the table is a soft
            // preference, so approximate eviction is fine.
            if let Some(oldest) = inner
                .iter()
                .min_by_key(|(_, binding)| binding.expires_at)
                .map(|(key, _)| key.clone())
            {
                inner.remove(&oldest);
            }
        }
        inner.insert(
            key,
            Binding {
                provider: provider.to_string(),
                model: model.to_string(),
                expires_at: now + TTL,
            },
        );
    }

    pub(crate) fn source_target(
        &self,
        req: &ChatCompletionRequest,
    ) -> Result<Option<Source_target>, crate::proxy::canonical::ProviderError> {
        let Some(source) = &req.source_responses_request else {
            return Ok(None);
        };
        if source.extra.get("conversation").is_some_and(|value| !value.is_null()) {
            return Err(source_error("responses_source_unknown"));
        }
        let mut refs = Vec::new();
        if let Some(id) = &source.previous_response_id {
            refs.push(source_ref("response", id));
        }
        if let Some(items) = source.input.as_array() {
            for item in items {
                // Codex assigns ids to inline messages; fallback conversion also
                // emits ids on complete function calls. Neither asks the upstream
                // to recover a server-side item when its full payload is supplied.
                // Opaque/incomplete items and explicit references retain the
                // ownership check; encrypted payloads are checked independently.
                if !materialized_item(item) {
                    if let Some(id) = item.get("id").and_then(serde_json::Value::as_str) {
                        refs.push(source_ref("item", id));
                    }
                }
                if let Some(encrypted) = item.get("encrypted_content").and_then(serde_json::Value::as_str) {
                    refs.push(source_ref("encrypted", encrypted));
                }
                if item.get("type").and_then(serde_json::Value::as_str) == Some("item_reference")
                    && item.get("id").and_then(serde_json::Value::as_str).is_none()
                {
                    return Err(source_error("responses_source_unknown"));
                }
            }
        }
        if refs.is_empty() {
            return Ok(None);
        }
        if refs.len() > 256 {
            return Err(source_error("responses_source_unknown"));
        }
        let client = client_ref(req);
        let mut sources = self.sources.lock().unwrap_or_else(|error| error.into_inner());
        let now = Instant::now();
        sources.retain(|_, binding| binding.expires_at > now);
        let mut target = None;
        for reference in refs {
            let binding = sources
                .get(&(client.clone(), reference))
                .ok_or_else(|| source_error("responses_source_unknown"))?;
            if target.as_ref().is_some_and(|target| target != &binding.target) {
                return Err(source_error("responses_source_mismatch"));
            }
            target = Some(binding.target.clone());
        }
        Ok(target)
    }

    pub(crate) fn record_response(
        &self,
        client: &str,
        response: &serde_json::Value,
        target: &Source_target,
    ) {
        let mut refs = Vec::new();
        if let Some(id) = response.get("id").and_then(serde_json::Value::as_str).filter(|id| !id.is_empty()) {
            refs.push(source_ref("response", id));
        }
        if let Some(output) = response.get("output").and_then(serde_json::Value::as_array) {
            for item in output.iter().take(128) {
                if let Some(id) = item.get("id").and_then(serde_json::Value::as_str) {
                    refs.push(source_ref("item", id));
                }
                if let Some(encrypted) = item.get("encrypted_content").and_then(serde_json::Value::as_str) {
                    refs.push(source_ref("encrypted", encrypted));
                }
            }
        }
        let now = Instant::now();
        let mut sources = self.sources.lock().unwrap_or_else(|error| error.into_inner());
        sources.retain(|_, binding| binding.expires_at > now);
        for reference in refs {
            if sources.len() >= CAPACITY {
                if let Some(oldest) = sources.iter().min_by_key(|(_, binding)| binding.expires_at).map(|(key, _)| key.clone()) {
                    sources.remove(&oldest);
                }
            }
            sources.insert((client.to_string(), reference), Source_binding {
                target: target.clone(),
                expires_at: now + TTL,
            });
        }
    }

    #[cfg(test)]
    pub(crate) fn bound_provider(&self, key: &(String, String)) -> Option<String> {
        self.bound_hop(key).map(|hop| hop.provider)
    }

    /// Affinity can reorder eligible hops but never create a new one.
    pub(crate) fn preferred_hops(
        &self,
        key: Option<&(String, String)>,
        hops: std::sync::Arc<Vec<RouteHop>>,
    ) -> std::sync::Arc<Vec<RouteHop>> {
        let Some(key) = key else {
            return hops;
        };
        let Some(bound) = self.bound_hop(key) else {
            return hops;
        };
        if !hops.iter().any(|hop| hop.provider == bound.provider && hop.model == bound.model) {
            return hops;
        }
        let mut preferred: Vec<RouteHop> = Vec::with_capacity(hops.len());
        let mut rest: Vec<RouteHop> = Vec::with_capacity(hops.len());
        for hop in hops.iter() {
            if hop.provider == bound.provider && hop.model == bound.model {
                preferred.push(hop.clone());
            } else {
                rest.push(hop.clone());
            }
        }
        preferred.extend(rest);
        std::sync::Arc::new(preferred)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::canonical::Client_credential;

    fn responses_source(previous_response_id: &str) -> crate::proxy::responses::ResponsesRequest {
        serde_json::from_value(serde_json::json!({
            "model": "m",
            "input": "hi",
            "previous_response_id": previous_response_id,
        }))
        .expect("fixture responses request")
    }

    fn request_with(user: Option<&str>) -> ChatCompletionRequest {
        let mut req = ChatCompletionRequest {
            model: "m".into(),
            messages: vec![],
            stream: false,
            temperature: None,
            max_tokens: None,
            tools: None,
            tool_choice: None,
            extra: Default::default(),
            source_anthropic_request: None,
            source_gemini_request: None,
            source_responses_request: None,
            request_id: Some("req_not_a_session_1".into()),
            client_credential: None,
        };
        if let Some(user) = user {
            req.extra
                .insert("user".into(), serde_json::Value::String(user.into()));
        }
        req
    }

    #[test]
    fn only_explicit_markers_identify_a_session() {
        // Per-request ids are never session identities.
        assert_eq!(session_ref(&request_with(None)), None);
        // Explicit markers are.
        assert!(session_ref(&request_with(Some("user-a"))).is_some());
        let mut anthropic = request_with(None);
        anthropic.source_anthropic_request = Some(serde_json::json!({
            "metadata": {"user_id": "user-b"}
        }));
        assert!(session_ref(&anthropic).is_some());
        let mut responses = request_with(None);
        responses.source_responses_request = Some(responses_source("resp_123"));
        assert_eq!(session_ref(&responses), None);
    }

    #[test]
    fn responses_source_does_not_replace_explicit_session_marker() {
        let mut req = request_with(Some("user-a"));
        req.source_anthropic_request =
            Some(serde_json::json!({"metadata": {"user_id": "user-b"}}));
        req.source_responses_request = Some(responses_source("resp_123"));
        let expected = crate::diagnostics::safe_reference("user-b");
        assert_eq!(session_ref(&req).as_deref(), Some(expected.as_str()));
    }

    #[test]
    fn bindings_are_isolated_per_call_identity() {
        let affinity = Session_affinity::default();
        let mut client_a = request_with(Some("shared-session"));
        client_a.client_credential = Some(Client_credential {
            passthrough_only: false,
            authorization: None,
            x_api_key: Some("key-a".into()),
            anthropic_beta: None,
        });
        let mut client_b = request_with(Some("shared-session"));
        client_b.client_credential = Some(Client_credential {
            passthrough_only: false,
            authorization: None,
            x_api_key: Some("key-b".into()),
            anthropic_beta: None,
        });
        let key_a = affinity_key(&client_a).expect("explicit session");
        let key_b = affinity_key(&client_b).expect("explicit session");
        assert_ne!(key_a, key_b, "same session name under different callers");
        affinity.record_winner(key_a.clone(), "provider-x", "m");
        assert_eq!(
            affinity.bound_provider(&key_a).as_deref(),
            Some("provider-x")
        );
        assert_eq!(affinity.bound_provider(&key_b), None);
    }

    #[test]
    fn preferred_hops_rotates_only_chain_members() {
        let affinity = Session_affinity::default();
        let key = ("local".to_string(), "sess".to_string());
        affinity.record_winner(key.clone(), "p2", "m");
        let hops = std::sync::Arc::new(vec![
            RouteHop {
                provider: "p1".into(),
                model: "m".into(),
            },
            RouteHop {
                provider: "p2".into(),
                model: "m".into(),
            },
            RouteHop {
                provider: "p3".into(),
                model: "m".into(),
            },
        ]);
        let preferred = affinity.preferred_hops(Some(&key), hops.clone());
        let order: Vec<&str> = preferred.iter().map(|hop| hop.provider.as_str()).collect();
        assert_eq!(order, vec!["p2", "p1", "p3"]);
        // A binding outside the chain never invents a hop.
        let outsider = ("local".to_string(), "sess-2".to_string());
        affinity.record_winner(outsider.clone(), "p9", "m");
        let unchanged = affinity.preferred_hops(Some(&outsider), hops.clone());
        assert_eq!(unchanged.len(), hops.len());
        assert_eq!(unchanged[0].provider, "p1");
        // No key: order is exactly the plan.
        let planned = affinity.preferred_hops(None, hops);
        assert_eq!(planned[0].provider, "p1");
    }

    #[test]
    fn capacity_evicts_and_ttl_expires() {
        let affinity = Session_affinity::default();
        for i in 0..(CAPACITY + 10) {
            affinity.record_winner(("local".into(), format!("s{i}")), "p", "m");
        }
        assert!(affinity.inner.lock().unwrap().len() <= CAPACITY);
        // A fresh binding with a zero TTL-equivalent: force expiry by
        // rewriting the expiry in the past.
        let key = ("local".to_string(), "expired".to_string());
        affinity.record_winner(key.clone(), "p", "m");
        {
            let mut inner = affinity.inner.lock().unwrap();
            inner.get_mut(&key).unwrap().expires_at = Instant::now() - Duration::from_secs(1);
        }
        assert_eq!(affinity.bound_provider(&key), None);
    }
}
