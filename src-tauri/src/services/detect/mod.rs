//! External-app coexistence detection.
//!
//! Currently detects [cc-switch](https://github.com/farion1231/cc-switch)
//! running alongside AAG. The two apps both write `ANTHROPIC_BASE_URL` /
//! `OPENAI_BASE_URL` into the same live agent config files
//! (`~/.claude/settings.json`, `~/.codex/config.toml`); whichever app
//! wrote last wins, so both running concurrently silently routes some
//! requests through the wrong proxy.
//!
//! See `cc_switch.rs` for the implementation. This `mod.rs` exists so the
//! public surface is grouped here as new external detectors land.

pub mod cc_switch;
