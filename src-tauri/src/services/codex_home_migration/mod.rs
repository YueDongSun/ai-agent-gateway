//! Recoverable whole-home Codex migration. No platform backend or live entry
//! point is installed here; the parent owns writer exclusion and DB/session
//! cutover. Platform operations stay behind the admitted storage contract.

mod contract;
mod engine;
mod facade;
pub(crate) mod native_storage;
mod plan;
#[cfg(test)]
mod tests;

pub(crate) use contract::*;
pub(crate) use engine::{advance, begin, commit, recover, rollback};
pub(crate) use plan::prepare_plan;
