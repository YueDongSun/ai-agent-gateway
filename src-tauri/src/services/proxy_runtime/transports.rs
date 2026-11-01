use crate::database::dao::proxy_providers::ProxyProvider;
use crate::proxy::canonical::{
    ChatCompletionRequest, OAIChatResponse, OAIStreamChunk, ProviderError,
};
use crate::proxy::circuit_breaker::CircuitBreaker;
use crate::proxy::providers::{AnthropicAuthScheme, Provider};
use async_trait::async_trait;
use futures_core::Stream;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;

#[derive(Clone, Default)]
#[allow(non_camel_case_types)]
pub(crate) struct Transport_cache {
    entries: HashMap<String, Transport_entry>,
}

#[derive(Clone)]
#[allow(non_camel_case_types)]
struct Transport_entry {
    config: ProxyProvider,
    auth_scheme: AnthropicAuthScheme,
    transport: Arc<dyn Provider>,
    identity: u64,
}

impl Transport_cache {
    pub(crate) fn source_bindings(
        &self,
    ) -> HashMap<String, crate::proxy::session_affinity::Provider_binding> {
        self.entries
            .iter()
            .map(|(id, entry)| {
                (
                    id.clone(),
                    crate::proxy::session_affinity::Provider_binding {
                        identity: entry.identity,
                        native_responses: matches!(
                            entry.config.provider_type.as_str(),
                            "openai-responses" | "openai_responses"
                        ),
                    },
                )
            })
            .collect()
    }

    pub(crate) fn build(
        &mut self,
        config: &ProxyProvider,
        previous: Option<&Self>,
    ) -> Result<Arc<dyn Provider>, String> {
        self.build_with_auth(config, previous, AnthropicAuthScheme::default())
    }

    pub(crate) fn build_with_auth(
        &mut self,
        config: &ProxyProvider,
        previous: Option<&Self>,
        auth_scheme: AnthropicAuthScheme,
    ) -> Result<Arc<dyn Provider>, String> {
        let previous = previous
            .and_then(|cache| cache.entries.get(&config.id))
            .filter(|entry| entry.auth_scheme == auth_scheme && same_transport(&entry.config, config));
        let (transport, identity) = match previous {
            Some(entry) => (Arc::clone(&entry.transport), entry.identity),
            None => (
                Arc::from(
                    crate::proxy::providers::build_provider_with_auth(config, auth_scheme)
                        .map_err(|_| "proxy_provider_build_failed".to_string())?,
                ),
                crate::proxy::session_affinity::next_transport_identity(),
            ),
        };
        self.entries.insert(
            config.id.clone(),
            Transport_entry {
                config: config.clone(),
                auth_scheme,
                transport: Arc::clone(&transport),
                identity,
            },
        );
        Ok(Arc::new(Provider_view {
            transport,
            models: config.models.clone(),
            enabled: config.enabled,
        }))
    }
}

fn same_transport(old: &ProxyProvider, new: &ProxyProvider) -> bool {
    old.id == new.id
        && old.provider_type == new.provider_type
        && old.api_key == new.api_key
        && old.base_url == new.base_url
        && old.timeout_ms == new.timeout_ms
        && old.circuit_breaker == new.circuit_breaker
        && old.extra_headers == new.extra_headers
}

#[allow(non_camel_case_types)]
struct Provider_view {
    transport: Arc<dyn Provider>,
    models: Vec<String>,
    enabled: bool,
}

#[async_trait]
impl Provider for Provider_view {
    fn wire_protocol(&self) -> &'static str {
        self.transport.wire_protocol()
    }

    fn id(&self) -> &str {
        self.transport.id()
    }

    fn enabled(&self) -> bool {
        self.enabled
    }

    fn models(&self) -> &[String] {
        &self.models
    }

    fn breaker(&self) -> &CircuitBreaker {
        self.transport.breaker()
    }

    fn requires_client_credential(&self) -> bool {
        self.transport.requires_client_credential()
    }

    async fn complete(
        &self,
        request: &ChatCompletionRequest,
        upstream_model: &str,
    ) -> Result<OAIChatResponse, ProviderError> {
        self.transport.complete(request, upstream_model).await
    }

    async fn complete_observed(
        &self, request: &ChatCompletionRequest, upstream_model: &str,
        observation: crate::proxy::routing_receipts::ProviderObservation,
    ) -> Result<OAIChatResponse, ProviderError> {
        self.transport.complete_observed(request, upstream_model, observation).await
    }

    async fn count_tokens(
        &self,
        body: serde_json::Value,
        credential: Option<&crate::proxy::canonical::Client_credential>,
    ) -> Result<u32, ProviderError> {
        self.transport.count_tokens(body, credential).await
    }

    async fn stream(
        &self,
        request: &ChatCompletionRequest,
        upstream_model: &str,
    ) -> Result<Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>>, ProviderError> {
        self.transport.stream(request, upstream_model).await
    }

    async fn stream_observed(
        &self, request: &ChatCompletionRequest, upstream_model: &str,
        observation: crate::proxy::routing_receipts::ProviderObservation,
    ) -> Result<Pin<Box<dyn Stream<Item = OAIStreamChunk> + Send>>, ProviderError> {
        self.transport.stream_observed(request, upstream_model, observation).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::dao::proxy_providers::CircuitBreakerConfig;

    fn fixture() -> ProxyProvider {
        ProxyProvider {
            id: "transport_fixture".into(),
            name: "Fixture".into(),
            provider_type: "mock".into(),
            api_key: String::new(),
            base_url: String::new(),
            models: vec!["mock-fast".into()],
            enabled: true,
            timeout_ms: 1_000,
            weight: 1,
            circuit_breaker: CircuitBreakerConfig::default(),
            model_mapping: HashMap::new(),
            extra_headers: HashMap::new(),
            auto_track_models: false,
            icon: None,
            icon_color: None,
        }
    }

    #[test]
    fn runtime_views_preserve_every_native_transport_protocol() {
        let mut cache = Transport_cache::default();
        for (provider_type, expected) in [
            ("anthropic", "anthropic"),
            ("openai", "openai-chat"),
            ("openai-responses", "openai-responses"),
            ("gemini", "gemini"),
        ] {
            let mut config = fixture();
            config.id = format!("protocol_{provider_type}");
            config.provider_type = provider_type.into();
            config.base_url = "https://example.invalid".into();
            let wrapped = cache.build(&config, None).unwrap();
            assert_eq!(
                wrapped.wire_protocol(),
                expected,
                "provider={provider_type}"
            );
            assert_eq!(
                wrapped.wire_protocol(),
                cache.entries[&config.id].transport.wire_protocol()
            );
        }
    }

    #[test]
    fn ranked_routing_prefers_the_native_protocol_through_runtime_views() {
        use crate::proxy::planner::{Model_planner, Planned_hop, Request_features, Resolution};
        use crate::proxy::router::{Router, SURFACE_GEMINI};

        struct RankedFixture;
        impl Model_planner for RankedFixture {
            fn resolve(
                &self,
                selector: &str,
                _features: Request_features,
            ) -> Result<Resolution, ProviderError> {
                Ok(Resolution::Alias {
                    alias: selector.into(),
                    tier: 1,
                    hops: ["anthropic", "gemini"]
                        .into_iter()
                        .map(|provider| Planned_hop {
                            provider: provider.into(),
                            model: "configured-model".into(),
                            tier: Some(1),
                            priority: 0,
                            basis: "fixture".into(),
                            downgraded: false,
                            reason: "fixture".into(),
                        })
                        .collect(),
                })
            }
        }
        let mut cache = Transport_cache::default();
        let mut providers = HashMap::new();
        for provider_type in ["anthropic", "gemini"] {
            let mut config = fixture();
            config.id = provider_type.into();
            config.provider_type = provider_type.into();
            config.base_url = "https://example.invalid".into();
            config.models = vec!["configured-model".into()];
            providers.insert(config.id.clone(), cache.build(&config, None).unwrap());
        }
        let router = Router::new(providers, HashMap::new()).with_planner(Arc::new(RankedFixture));
        let hops = router
            .resolve_model(SURFACE_GEMINI, "alias:sonnet", Default::default())
            .unwrap();
        assert_eq!(
            hops.iter()
                .map(|hop| hop.provider.as_str())
                .collect::<Vec<_>>(),
            ["gemini", "anthropic"]
        );
    }

    #[test]
    fn model_and_enabled_changes_preserve_transport_and_breaker() {
        let config = fixture();
        let mut first = Transport_cache::default();
        let old = first.build(&config, None).unwrap();
        old.breaker().record_failure();
        let mut edited = config.clone();
        edited.models.push("mock-new".into());
        edited.enabled = false;
        let mut second = Transport_cache::default();
        let new = second.build(&edited, Some(&first)).unwrap();
        assert!(Arc::ptr_eq(
            &first.entries[&config.id].transport,
            &second.entries[&config.id].transport
        ));
        assert_eq!(new.breaker().snapshot().failures, 1);
        assert_eq!(new.models(), edited.models.as_slice());
        assert!(!new.enabled());
        assert!(old.enabled());
        assert_eq!(old.models(), config.models.as_slice());
    }

    #[test]
    fn source_identity_follows_transport_reuse_and_rebuilds() {
        let mut config = fixture();
        config.provider_type = "openai-responses".into();
        let mut first = Transport_cache::default();
        first.build(&config, None).unwrap();
        let mut edited = config.clone();
        edited.models.push("mock-new".into());
        let mut second = Transport_cache::default();
        second.build(&edited, Some(&first)).unwrap();
        assert_eq!(
            first.source_bindings()[&config.id].identity,
            second.source_bindings()[&config.id].identity,
        );
        assert!(second.source_bindings()[&config.id].native_responses);
        let mut rebuilt = config.clone();
        rebuilt.base_url = "https://example.invalid/v1".into();
        let mut third = Transport_cache::default();
        third.build(&rebuilt, Some(&second)).unwrap();
        assert_ne!(
            second.source_bindings()[&config.id].identity,
            third.source_bindings()[&config.id].identity,
        );
    }

    #[test]
    fn credential_change_does_not_share_admission_with_old_transport() {
        let config = fixture();
        let mut first = Transport_cache::default();
        let old = first.build(&config, None).unwrap();
        old.breaker().record_failure();
        let mut edited = config.clone();
        edited.api_key = "synthetic_new_credential".into();
        let mut second = Transport_cache::default();
        let new = second.build(&edited, Some(&first)).unwrap();
        assert!(!Arc::ptr_eq(
            &first.entries[&config.id].transport,
            &second.entries[&config.id].transport
        ));
        assert_eq!(new.breaker().snapshot().failures, 0);
        assert_eq!(old.breaker().snapshot().failures, 1);
    }

    #[test]
    fn authentication_scheme_change_rotates_transport_identity_without_changing_provider_fields() {
        let mut config = fixture();
        config.provider_type = "anthropic".into();
        config.base_url = "https://fixture.invalid/anthropic".into();
        config.api_key = "fixture_token".into();
        let mut first = Transport_cache::default();
        first.build_with_auth(&config, None, AnthropicAuthScheme::ApiKey).unwrap();
        let mut retained = Transport_cache::default();
        retained.build_with_auth(&config, Some(&first), AnthropicAuthScheme::ApiKey).unwrap();
        assert!(Arc::ptr_eq(&first.entries[&config.id].transport, &retained.entries[&config.id].transport));
        assert_eq!(first.source_bindings()[&config.id].identity, retained.source_bindings()[&config.id].identity);
        let mut changed = Transport_cache::default();
        changed.build_with_auth(&config, Some(&retained), AnthropicAuthScheme::Bearer).unwrap();
        assert!(!Arc::ptr_eq(&retained.entries[&config.id].transport, &changed.entries[&config.id].transport));
        assert_ne!(retained.source_bindings()[&config.id].identity, changed.source_bindings()[&config.id].identity);
        assert_eq!(changed.entries[&config.id].auth_scheme, AnthropicAuthScheme::Bearer);
    }
}
