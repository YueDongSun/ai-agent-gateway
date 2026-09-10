//! Data Access Object layer.
//!
//! All DAO methods are `impl Database` blocks spread across submodules,
//! mirroring cc-switch's `database/dao/mod.rs`.

pub mod backup;
pub mod mcp;
pub mod providers;
pub mod proxy_config;
pub mod proxy_providers;
pub mod routes;
pub mod sessions;
pub mod settings;
pub mod skills;
pub mod sync_config;
pub mod usage;
