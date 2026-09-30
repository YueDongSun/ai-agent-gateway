//! Tauri command modules, grouped by responsibility.
//!
//! Extracted from `lib.rs` in the file-structure refactor - the command
//! bodies are moved verbatim; only module paths in `use` statements and
//! `pub(crate)` visibility changed.

pub mod client_config;
pub mod diagnostics;
pub mod dsh;
pub mod dsh_market;
pub mod dsh_onboarding;
pub mod launcher;
pub mod mcp;
pub mod provider;
pub mod proxy;
pub mod settings;
