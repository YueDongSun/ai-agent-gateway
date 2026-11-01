//! Service layer - business logic above the database, below the Tauri commands.

pub(crate) mod codex_home_migration;
pub mod detect;
pub mod dsh;
pub mod import_cc_switch;
pub(crate) mod launcher;
pub mod mcp;
pub(crate) mod mirror_models;
pub(crate) mod mirror_sync;
pub(crate) mod model_catalog;
pub(crate) mod model_discovery;
pub(crate) mod owned_process;
pub mod prompt;
pub mod provider;
pub mod provider_screening;
pub mod provider_admission;
pub mod subscription_source;
pub mod proxy_runtime;
pub(crate) mod session_broker;
pub mod session_authority;
pub(crate) mod skills_ssot;
pub(crate) mod supplier_setup;
pub mod suppliers;
pub mod sync;
pub(crate) mod tailnet_publication;
pub mod workflow_registry;

pub(crate) mod tailnet_discovery;
