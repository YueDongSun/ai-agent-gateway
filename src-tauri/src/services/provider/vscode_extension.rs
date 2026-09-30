//! VS Code extension adapter for Claude Code + Kilo Code.
//!
//! Both extensions transparently route through AAG when VS Code is
//! launched with the AAG proxy env vars injected — see
//! `commands::launcher::vscode_open_with_extension`:
//!
//! - **Claude Code** (`anthropic.claude-code`): the extension runs the
//!   Claude Code CLI in-process, which honors `ANTHROPIC_BASE_URL` /
//!   `ANTHROPIC_AUTH_TOKEN`. Per the claude_code adapter's contract, AAG
//!   does **not** write `~/.claude/settings.json` (cc-switch owns it);
//!   provider switching happens inside AAG (failover chain / pinned
//!   provider) while the extension keeps pointing at
//!   `http://127.0.0.1:<port>`.
//! - **Kilo Code** (`kilocode.kilo-code`): an OpenCode fork — it reads
//!   the OpenCode-compatible JSONC projection at
//!   `~/.config/kilo/{kilo,opencode}.json(c)` / `~/.kilo/config.json`
//!   (the same five paths as the Kilo CLI; see the `tests/adapters.test.ts`
//!   comments referring to the projection "consumed by Kilo VS Code").
//!   Switching delegates to [`crate::services::provider::kilo::write_live`]
//!   and the launcher additionally injects `OPENAI_BASE_URL` as belt and
//!   braces.
//!
//! This module therefore implements **AAG as the unified upstream**
//! ("auto-forward"): the provider that AAG's route/failover system picks
//! serves every request from the extension, regardless of whether the
//! extension supports hot-switching itself. No writes target VS Code's
//! `User/settings.json` — the JSON key namespaces there are not
//! source-verifiable (cc-switch removed its VS Code settings.json write
//! path in 3.4.x), so this adapter never touches it.

use crate::error::AppError;
use crate::provider::Provider;
use crate::services::provider::{kilo, LiveWriteResult};

/// Which VS Code extension a switch targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VsCodeApp {
    ClaudeCode,
    KiloCode,
}

impl VsCodeApp {
    fn app_type_str(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude_vscode",
            Self::KiloCode => "kilo_vscode",
        }
    }

    fn display_name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code (VS Code)",
            Self::KiloCode => "Kilo Code (VS Code)",
        }
    }
}

/// Is VS Code installed? (User settings dir or settings.json exists)
#[allow(dead_code)]
pub fn detect() -> bool {
    let path = crate::config::get_vscode_user_settings_path();
    path.exists() || path.parent().is_some_and(|p| p.exists())
}

/// Write the provider into the extension's live config.
///
/// - `ClaudeCode` → no live write (returns an empty `LiveWriteResult`
///   like `claude_code`; the launcher's `ANTHROPIC_BASE_URL` injection +
///   AAG-side switching is the unified upstream). Errors are never
///   swallowed: an invalid `settings_config` still fails the switch.
/// - `KiloCode` → delegates to [`kilo::write_live`] (the OpenCode JSONC
///   projection Kilo Code reads), relabeling the result.
pub fn write_live(app: VsCodeApp, provider: &Provider) -> Result<LiveWriteResult, AppError> {
    match app {
        VsCodeApp::ClaudeCode => {
            // Validate the same shape the claude_code chain expects so a
            // malformed row fails at the boundary instead of silently
            // producing an unusable provider entry.
            let env = provider.settings_config.get("env");
            let has_base = env
                .and_then(|e| e.get("ANTHROPIC_BASE_URL"))
                .and_then(|v| v.as_str())
                .is_some_and(|s| !s.trim().is_empty());
            let flat_base = provider
                .settings_config
                .get("baseUrl")
                .and_then(|v| v.as_str())
                .is_some_and(|s| !s.trim().is_empty());
            if !has_base && !flat_base {
                return Err(AppError::Config(format!(
                    "{} provider requires env.ANTHROPIC_BASE_URL (or flat baseUrl) — the launcher injects it into VS Code on open",
                    app.display_name()
                )));
            }
            Ok(LiveWriteResult {
                app_type: app.app_type_str().into(),
                provider_id: provider.id.clone(),
                written: vec![],
                backup: None,
            })
        }
        VsCodeApp::KiloCode => {
            let mut result = kilo::write_live(provider)?;
            result.app_type = String::from("kilo_vscode");
            Ok(result)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::provider::test_support::isolated_home;
    use serde_json::json;
    use std::fs;

    #[test]
    fn claude_code_no_live_write_but_validates_base_url() {
        let _home = isolated_home("vscode2");

        // Valid claude-shaped provider: empty write result (no file
        // touched), mirroring the claude_code switch contract.
        let ok = Provider::new(
            "cc".into(),
            "AAG".into(),
            json!({ "env": { "ANTHROPIC_BASE_URL": "http://127.0.0.1:8989", "ANTHROPIC_AUTH_TOKEN": "t" } }),
        );
        let result = write_live(VsCodeApp::ClaudeCode, &ok).expect("claude switch");
        assert_eq!(result.app_type, "claude_vscode");
        assert!(result.written.is_empty(), "claude_vscode writes nothing");
        assert!(result.backup.is_none());

        // Malformed provider fails at the boundary.
        let bad = Provider::new("bad".into(), "Bad".into(), json!({ "model": "x" }));
        let err = write_live(VsCodeApp::ClaudeCode, &bad).expect_err("must fail closed");
        assert!(err.to_string().contains("ANTHROPIC_BASE_URL"), "got: {err}");
    }

    #[test]
    fn kilo_code_delegates_to_kilo_jsonc_write() {
        let home = isolated_home("vscode2");

        let p = Provider::new(
            "kilo-gw".into(),
            "AAG".into(),
            json!({
                "options": { "baseURL": "http://127.0.0.1:8989/v1", "apiKey": "sk-x" },
                "models": { "claude-sonnet-5": { "name": "claude-sonnet-5" } }
            }),
        );
        let result = write_live(VsCodeApp::KiloCode, &p).expect("kilo vscode switch");
        assert_eq!(result.app_type, "kilo_vscode");
        assert_eq!(result.written.len(), 1);

        // The Kilo JSONC projection exists with the AAG provider entry.
        let path = home.root.join(".config").join("kilo").join("kilo.jsonc");
        let value: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            value["provider"]["kilo-gw"]["options"]["baseURL"],
            "http://127.0.0.1:8989/v1"
        );
        assert_eq!(value["model"], "kilo-gw/claude-sonnet-5");
    }

    #[test]
    fn kilo_code_forwards_validation_errors() {
        let _home = isolated_home("vscode2");
        let bad = Provider::new("bad".into(), "Bad".into(), json!({ "options": {} }));
        let err = write_live(VsCodeApp::KiloCode, &bad).expect_err("must fail closed");
        assert!(err.to_string().contains("baseURL"), "got: {err}");
    }
}
