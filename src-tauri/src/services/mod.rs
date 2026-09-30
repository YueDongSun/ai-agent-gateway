//! Service layer - business logic above the database, below the Tauri commands.

pub mod detect;
pub mod dsh;
pub mod import_cc_switch;
pub(crate) mod launcher;
pub mod mcp;
pub mod provider;
pub mod sync;
