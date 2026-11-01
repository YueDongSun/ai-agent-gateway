//! Model-target resolution for the prompt-enhancement pipeline.
//!
//! Two targets are supported, both backed by the local AAG model pool:
//!
//! - gateway  - resolve through the RUNNING gateway router, so the request
//!   inherits the configured route aliases, ordered hops, and circuit
//!   breakers. This is the mode that matches "models from the local API".
//! - provider - call one configured upstream provider directly. It keeps the
//!   feature usable while the gateway is stopped, at the cost of no failover
//!   and no breaker participation.

use crate::database::dao::routes::RouteHop;
use crate::database::Database;
use crate::proxy::canonical::{ChatCompletionRequest, OAIChatResponse, ProviderError};
use crate::proxy::router::{Router, SURFACE_CHAT};
use serde::Deserialize;
use std::sync::Arc;

/// Bookkeeping key written into the router last_used map. It is not a real
/// agent app type, so the gateway failover-winner persistence ignores it.
pub use crate::proxy::prompt_budget::PROMPT_APP_TYPE;

/// Which local model backend serves one enhancement stage.
#[allow(non_camel_case_types)]
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Prompt_target {
    /// Route through the running local gateway (failover + breakers).
    Gateway { model: String },
    /// Call one configured upstream provider directly.
    Provider {
        #[serde(rename = "providerId")]
        provider_id: String,
        model: String,
    },
}

impl Prompt_target {
    /// The model name this target asks for on the wire.
    pub fn model(&self) -> &str {
        match self {
            Prompt_target::Gateway { model } => model,
            Prompt_target::Provider { model, .. } => model,
        }
    }

    /// Content-free label for the response and local logs.
    pub fn label(&self) -> String {
        match self {
            Prompt_target::Gateway { model } => format!("gateway:{model}"),
            Prompt_target::Provider { provider_id, model } => {
                format!("provider:{provider_id}/{model}")
            }
        }
    }

    /// Validate the target at the IPC boundary before any work is scheduled.
    pub fn validate(&self) -> Result<(), String> {
        if self.model().trim().is_empty() {
            return Err("prompt_enhance_target_model_required".to_string());
        }
        match self {
            Prompt_target::Provider { provider_id, .. } if provider_id.trim().is_empty() => {
                Err("prompt_enhance_target_provider_required".to_string())
            }
            _ => Ok(()),
        }
    }
}

/// One completed dispatch: the upstream response plus the provider that served
/// it when the gateway resolved the route.
#[allow(non_camel_case_types)]
pub struct Prompt_dispatch {
    pub response: OAIChatResponse,
    pub provider: Option<String>,
    /// Request-local configured identity. Upstream response model labels may
    /// identify a dated build and must never be used to select fallback hops.
    pub serving_hop: RouteHop,
}

/// Execute one canonical chat request against the selected target.
pub async fn dispatch(
    db: &Database,
    router: Option<Arc<Router>>,
    target: &Prompt_target,
    request: &ChatCompletionRequest,
) -> Result<Prompt_dispatch, String> {
    match target {
        Prompt_target::Gateway { .. } => {
            let router = router.ok_or_else(|| "prompt_enhance_gateway_not_running".to_string())?;
            let (response, serving_hop) = router
                .complete_for_with_hop(SURFACE_CHAT, PROMPT_APP_TYPE, request)
                .await
                .map_err(upstream_failure)?;
            Ok(Prompt_dispatch {
                response,
                provider: Some(serving_hop.provider.clone()),
                serving_hop,
            })
        }
        Prompt_target::Provider { provider_id, model } => {
            let provider_row = db
                .get_all_proxy_providers()
                .map_err(|_| "prompt_enhance_providers_unavailable".to_string())?
                .into_iter()
                .find(|candidate| candidate.id == *provider_id)
                .ok_or_else(|| "prompt_enhance_provider_not_found".to_string())?;
            if !provider_row.enabled {
                return Err("prompt_enhance_provider_disabled".to_string());
            }
            if crate::services::provider_admission::enforcement_enabled(db) {
                crate::services::provider_admission::ensure_provider_admitted(db, &provider_row, model)
                    .await?;
            }
            let auth_scheme = crate::services::provider_admission::auth_scheme(db, &provider_row)?;
            let provider = crate::proxy::providers::build_provider_with_auth(&provider_row, auth_scheme)
                .map_err(|_| "prompt_enhance_provider_build_failed".to_string())?;
            let request = crate::proxy::prompt_budget::request_for_hop(
                PROMPT_APP_TYPE,
                request,
                model,
                provider.wire_protocol(),
            );
            let response = provider
                .complete(&request, model)
                .await
                .map_err(upstream_failure)?;
            Ok(Prompt_dispatch {
                response,
                provider: Some(provider_row.id.clone()),
                serving_hop: RouteHop {
                    provider: provider_row.id,
                    model: model.clone(),
                },
            })
        }
    }
}

/// Continue on one already configured gateway candidate without changing the
/// shared route. Recheck request capabilities and preserve router admission,
/// breakers, credentials and accounting rather than building a direct provider.
pub(super) async fn dispatch_hop(
    router: Arc<Router>,
    request: &ChatCompletionRequest,
    hop: &crate::database::dao::routes::RouteHop,
) -> Result<Prompt_dispatch, String> {
    let eligible = router
        .preview_model(
            SURFACE_CHAT,
            &request.model,
            crate::proxy::planner::request_features(request),
            None,
        )
        .map_err(upstream_failure)?;
    if !eligible
        .iter()
        .any(|candidate| candidate.provider == hop.provider && candidate.model == hop.model)
    {
        return Err("prompt_enhance_fallback_ineligible".into());
    }
    let (response, serving_hop) = router
        .complete_hops_with_hop(
            SURFACE_CHAT,
            PROMPT_APP_TYPE,
            request,
            Arc::new(vec![hop.clone()]),
        )
        .await
        .map_err(upstream_failure)?;
    Ok(Prompt_dispatch {
        response,
        provider: Some(serving_hop.provider.clone()),
        serving_hop,
    })
}

/// Map an upstream failure onto a stable, content-free error code. Upstream
/// messages are never forwarded: a provider error message can echo request or
/// response text.
fn upstream_failure(error: ProviderError) -> String {
    format!(
        "prompt_enhance_upstream_failed:{}:{}",
        error.code,
        error
            .status
            .map(|status| status.to_string())
            .unwrap_or_else(|| "none".into()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gateway_target() -> Prompt_target {
        Prompt_target::Gateway {
            model: "prompt-route".into(),
        }
    }

    #[test]
    fn gateway_target_deserializes_from_the_frontend_contract() {
        let parsed: Prompt_target =
            serde_json::from_str(r#"{"kind":"gateway","model":"prompt-route"}"#).expect("parses");
        assert!(matches!(parsed, Prompt_target::Gateway { .. }));
        assert_eq!(parsed.label(), "gateway:prompt-route");
        assert_eq!(parsed.model(), "prompt-route");
    }

    #[test]
    fn provider_target_deserializes_camel_case_ids() {
        let parsed: Prompt_target =
            serde_json::from_str(r#"{"kind":"provider","providerId":"p1","model":"m1"}"#)
                .expect("parses");
        match parsed {
            Prompt_target::Provider {
                ref provider_id, ..
            } => assert_eq!(provider_id, "p1"),
            _ => panic!("expected provider target"),
        }
        assert_eq!(parsed.label(), "provider:p1/m1");
        assert_eq!(parsed.model(), "m1");
    }

    #[test]
    fn validation_rejects_blank_model_and_blank_provider() {
        assert_eq!(
            Prompt_target::Gateway { model: "  ".into() }
                .validate()
                .unwrap_err(),
            "prompt_enhance_target_model_required"
        );
        assert_eq!(
            Prompt_target::Provider {
                provider_id: " ".into(),
                model: "m".into(),
            }
            .validate()
            .unwrap_err(),
            "prompt_enhance_target_provider_required"
        );
        assert!(gateway_target().validate().is_ok());
    }

    #[test]
    fn upstream_failure_never_forwards_provider_messages() {
        let error =
            crate::proxy::canonical::make_error("secret upstream body echo", Some(429), None);
        let code = upstream_failure(error);
        assert_eq!(code, "prompt_enhance_upstream_failed:provider_error:429");
        assert!(!code.contains("secret"));
    }
}
