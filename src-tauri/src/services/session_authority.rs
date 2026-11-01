//! Dedicated-process entrypoints for Windows session authority services.
//! The initial service qualification uses private empty metadata directories;
//! it never admits a conversation, model call, or live home migration.

#[cfg(windows)]
mod client_bridge;
pub(crate) mod client_config;
#[cfg(windows)]
mod client_invocation;
#[cfg(windows)]
mod native_role;
mod owner_client;
#[cfg(windows)]
mod protected_config;
#[cfg(windows)]
mod qualification;
#[cfg(windows)]
mod runtime_config;

/// The HTTP gateway receives a connection only after committed-installation,
/// protected-image and same-kernel-pipe/SCM owner checks. Missing production
/// authority remains unavailable; no native local app-server is a fallback.
#[cfg(windows)]
pub(crate) async fn connect_gateway_client(
) -> Result<crate::services::session_broker::local_transport::LocalConnection, String> {
    use crate::services::session_broker::local_transport::{
        LocalConnection, TransportCancellation, TransportLimits,
    };
    use std::time::{Duration, Instant};
    let root = crate::config::get_home_dir().join(".codex-cli");
    if !client_config::authority_object_present()? {
        return Err("session_client_authority_unavailable".into());
    }
    let peer = tokio::task::spawn_blocking(move || {
        owner_client::verify_client_authority(&root)?;
        owner_client::gateway_policy_peer().map_err(str::to_owned)
    })
    .await
    .map_err(|_| "session_client_authority_unavailable".to_owned())??;
    let connection = LocalConnection::connect(
        &peer,
        TransportLimits::default(),
        Instant::now() + Duration::from_secs(5),
        &TransportCancellation::new(),
    )
    .await
    .map_err(|_| "session_client_authority_unavailable".to_owned())?;
    tokio::task::spawn_blocking(move || {
        owner_client::verify_gateway_peer(&connection).map_err(str::to_owned)?;
        Ok(connection)
    })
    .await
    .map_err(|_| "session_client_authority_unavailable".to_owned())?
}

/// Dedicated console/service executable entrypoint. It is deliberately not
/// called by the Tauri application's process or its lifecycle callbacks.
pub fn main(arguments: Vec<std::ffi::OsString>) -> i32 {
    #[cfg(windows)]
    {
        if arguments
            .get(1)
            .is_some_and(|value| value == "qualify-service")
        {
            qualification::run(arguments)
        } else {
            client_invocation::run(arguments)
        }
    }
    #[cfg(not(windows))]
    {
        let _ = arguments;
        eprintln!("session_authority_windows_required");
        64
    }
}
