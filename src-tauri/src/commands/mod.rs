//! Tauri command modules, grouped by responsibility.
//!
//! Extracted from `lib.rs` in the file-structure refactor - the command
//! bodies are moved verbatim; only module paths in `use` statements and
//! `pub(crate)` visibility changed.

pub mod launcher;
pub mod provider;
pub mod proxy;
pub mod settings;
