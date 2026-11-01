//! Foreground, port-scoped Tailnet publication foundation.
//! HTTP authorization and application/proxy integration belong to the caller.

mod config;
mod error;
#[cfg(all(windows, test))]
mod live_acceptance;
mod manager;
mod metadata;
mod process;
pub(crate) mod settings;
mod status;
#[cfg(test)]
mod tests;
#[cfg(windows)]
mod windows_process;

pub(crate) use config::{PublicationConfig, PublicationPlan};
pub(crate) use error::PublicationError;
pub(crate) use manager::{PublicationManager, PublicationShutdown, PublicationSnapshot};
pub(crate) use metadata::{read_self_status, TailnetSelfStatus};
pub(crate) use process::{read_serve_status, NativePublication, PublicationState};
pub(crate) use settings::Publication_service;
pub(crate) use status::{OwnedRoute, Preflight, ServeStatus};

pub(crate) use metadata::trusted_executable as trusted_tailscale_executable;
