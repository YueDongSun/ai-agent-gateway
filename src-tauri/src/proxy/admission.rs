//! Optional per-attempt admission boundary. The application owns current
//! entitlement/configuration checks; the proxy never imports their service.
use async_trait::async_trait;
use std::future::Future;

#[async_trait]
pub trait Admission_gate: Send + Sync {
    /// Checked before acquiring a breaker permit or issuing an upstream call.
    /// Revocation affects subsequent attempts, not an already admitted stream.
    async fn allows(&self, provider_id: &str, model: &str) -> bool;
}

#[async_trait]
impl<F, Fut> Admission_gate for F
where
    F: Fn(String, String) -> Fut + Send + Sync,
    Fut: Future<Output = bool> + Send,
{
    async fn allows(&self, provider_id: &str, model: &str) -> bool {
        self(provider_id.to_owned(), model.to_owned()).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        database::dao::routes::RouteHop,
        proxy::{
            canonical::ChatCompletionRequest,
            providers::{mock::MockProvider, Provider},
            router::Router,
        },
    };
    use std::{
        collections::HashMap,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
    };

    fn router() -> Router {
        let providers: HashMap<String, Arc<dyn Provider>> = ["first", "second"]
            .into_iter()
            .map(|id| {
                (
                    id.into(),
                    Arc::new(MockProvider::new(
                        id.into(),
                        vec!["m".into()],
                        true,
                        Default::default(),
                    )) as Arc<dyn Provider>,
                )
            })
            .collect();
        Router::new(
            providers,
            HashMap::from([(
                "route".into(),
                vec![
                    RouteHop {
                        provider: "first".into(),
                        model: "m".into(),
                    },
                    RouteHop {
                        provider: "second".into(),
                        model: "m".into(),
                    },
                ],
            )]),
        )
    }

    #[tokio::test]
    async fn gate_skips_excluded_provider_and_rechecks_revocation_on_next_call() {
        let active = Arc::new(AtomicBool::new(true));
        let gate_active = active.clone();
        let gate = move |provider: String, model: String| {
            let allowed =
                gate_active.load(Ordering::SeqCst) && provider == "second" && model == "m";
            std::future::ready(allowed)
        };
        let router = router().with_admission_gate(Arc::new(gate));
        let request = ChatCompletionRequest {
            model: "route".into(),
            ..Default::default()
        };
        router
            .complete_for("chat", "fixture", &request)
            .await
            .unwrap();
        assert_eq!(
            router
                .last_used
                .lock()
                .unwrap()
                .get("fixture")
                .map(String::as_str),
            Some("second")
        );
        active.store(false, Ordering::SeqCst);
        assert!(router
            .complete_for("chat", "fixture", &request)
            .await
            .is_err());
        assert!(router.health().iter().all(|row| row.breaker.failures == 0));
    }

    #[tokio::test]
    async fn missing_gate_preserves_existing_explicit_opt_in_boundary() {
        let router = router();
        let request = ChatCompletionRequest {
            model: "route".into(),
            ..Default::default()
        };
        router
            .complete_for("chat", "fixture", &request)
            .await
            .unwrap();
        assert_eq!(
            router
                .last_used
                .lock()
                .unwrap()
                .get("fixture")
                .map(String::as_str),
            Some("first")
        );
    }
}
