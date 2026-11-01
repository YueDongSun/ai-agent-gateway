//! Explicit native controls for the authenticated Tailnet gateway frontend.

use super::proxy::RunningProxy;
use crate::{
    database::Database,
    services::tailnet_publication::{
        self, settings, PublicationConfig, PublicationSnapshot, PublicationState,
        Publication_service,
    },
};
use serde::Serialize;
use tauri::State;
use tokio::sync::Mutex;

#[derive(Serialize)]
pub(crate) struct TailnetPublicationStatus {
    config: PublicationConfig,
    publication: PublicationSnapshot,
    endpoint: Option<String>,
    endpoint_source: Option<&'static str>,
    metadata_error: Option<String>,
}

#[tauri::command]
pub(crate) fn tailnet_publication_config_get(
    db: State<'_, Database>,
) -> Result<PublicationConfig, String> {
    settings::read_config(&db)
}

#[tauri::command]
pub(crate) async fn tailnet_publication_config_save(
    db: State<'_, Database>,
    state: State<'_, Mutex<Option<RunningProxy>>>,
    publication: State<'_, Publication_service>,
    config: PublicationConfig,
) -> Result<(), String> {
    // No operation holding a publication lock may then acquire the proxy lock.
    let _guard = state.lock().await;
    settings::save_config(&db, &config)?;
    publication.stop().await.map(|_| ())
}

#[tauri::command]
pub(crate) async fn tailnet_publication_start(
    db: State<'_, Database>,
    state: State<'_, Mutex<Option<RunningProxy>>>,
    publication: State<'_, Publication_service>,
) -> Result<PublicationSnapshot, String> {
    let guard = state.lock().await;
    let gateway = guard
        .as_ref()
        .and_then(RunningProxy::publication_state)
        .ok_or_else(|| "tailnet_publication_local_gateway_required".to_string())?;
    let config = settings::read_config(&db)?;
    publication.start(config, gateway).await
}

#[tauri::command]
pub(crate) async fn tailnet_publication_stop(
    state: State<'_, Mutex<Option<RunningProxy>>>,
    publication: State<'_, Publication_service>,
) -> Result<PublicationSnapshot, String> {
    // Cancellation can interrupt startup even while startup owns proxy state.
    publication.cancel();
    let _guard = state.lock().await;
    publication.stop().await
}

#[tauri::command]
pub(crate) async fn tailnet_publication_status(
    db: State<'_, Database>,
    state: State<'_, Mutex<Option<RunningProxy>>>,
    publication: State<'_, Publication_service>,
) -> Result<TailnetPublicationStatus, String> {
    let (config, snapshot) = {
        let _guard = state.lock().await;
        (settings::read_config(&db)?, publication.status().await)
    };
    let metadata = tokio::task::spawn_blocking(tailnet_publication::read_self_status)
        .await
        .map_err(|_| "tailnet_publication_metadata_task_failed".to_string())?;
    let (endpoint, metadata_error) = match metadata {
        Ok(metadata) => (
            if snapshot.state == PublicationState::Running {
                metadata.self_ip.zip(snapshot.port).map(|(address, port)| {
                    format!("http://{}", std::net::SocketAddr::new(address, port))
                })
            } else {
                None
            },
            None,
        ),
        Err(error) => (None, Some(error.to_string())),
    };
    Ok(TailnetPublicationStatus {
        config,
        publication: snapshot,
        endpoint_source: endpoint.as_ref().map(|_| "tailscale_status_self"),
        endpoint,
        metadata_error,
    })
}
