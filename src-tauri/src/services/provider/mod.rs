//! Provider service - live config writing per AI coding tool.
//!
//! Mirrors cc-switch's `services/provider/live.rs` `write_live_snapshot`
//! (`live.rs:998`). Each app writes its config in a different format:
//! - Claude Code: `~/.claude/settings.json` (JSON)
//! - Codex: `~/.codex/config.toml` (TOML) + `~/.codex/auth.json` (JSON)
//! - Others: stubbed for later phases.

pub mod antigravity;
pub mod claude_code;
pub mod claude_mirror;
pub mod client_config;
pub mod codex;
pub mod config_paths;
pub mod config_writes;
pub mod copilot;
pub mod gemini;
pub mod kilo;
pub mod opencode;
pub mod pi;
pub mod switching;
pub mod vscode_extension;
#[cfg(test)]
pub mod test_support;

use crate::error::AppError;
use crate::provider::Provider;
use serde::{Deserialize, Serialize};

/// The AI coding tools AAG manages. Mirrors cc-switch's `AppType`.
/// String values are the `app_type` column in the `providers` table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppType {
    ClaudeCode,
    ClaudeDesktop,
    Codex,
    Copilot,
    Pi,
    Antigravity,
    Gemini,
    OpenCode,
    /// Kilo CLI (fork of OpenCode; shares `~/.config/opencode/opencode.json`).
    KiloCli,
    /// Claude Code VS Code extension (`anthropic.claude-code`).
    ClaudeVsCode,
    /// Kilo Code VS Code extension (`kilocode.kilo-code`).
    KiloVsCode,
    OpenClaw,
    Hermes,
    GrokBuild,
}

impl AppType {
    /// Parse from the string stored in the `app_type` column.
    pub fn from_db_str(s: &str) -> Result<Self, AppError> {
        match s {
            "claude_code" => Ok(Self::ClaudeCode),
            "claude_desktop" => Ok(Self::ClaudeDesktop),
            "codex" => Ok(Self::Codex),
            "copilot" => Ok(Self::Copilot),
            "pi" => Ok(Self::Pi),
            "antigravity" => Ok(Self::Antigravity),
            "gemini" => Ok(Self::Gemini),
            "opencode" => Ok(Self::OpenCode),
            "kilo_cli" => Ok(Self::KiloCli),
            "claude_vscode" => Ok(Self::ClaudeVsCode),
            "kilo_vscode" => Ok(Self::KiloVsCode),
            "openclaw" => Ok(Self::OpenClaw),
            "hermes" => Ok(Self::Hermes),
            "grok_build" => Ok(Self::GrokBuild),
            other => Err(AppError::InvalidInput(format!(
                "unknown app_type: \"{other}\""
            ))),
        }
    }

    /// Convert to the string for the `app_type` column.
    #[allow(dead_code)]
    pub fn to_db_str(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude_code",
            Self::ClaudeDesktop => "claude_desktop",
            Self::Codex => "codex",
            Self::Copilot => "copilot",
            Self::Pi => "pi",
            Self::Antigravity => "antigravity",
            Self::Gemini => "gemini",
            Self::OpenCode => "opencode",
            Self::KiloCli => "kilo_cli",
            Self::ClaudeVsCode => "claude_vscode",
            Self::KiloVsCode => "kilo_vscode",
            Self::OpenClaw => "openclaw",
            Self::Hermes => "hermes",
            Self::GrokBuild => "grok_build",
        }
    }

    /// Human-readable display name.
    pub fn display_name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::ClaudeDesktop => "Claude Desktop",
            Self::Codex => "Codex",
            Self::Copilot => "Copilot CLI",
            Self::Pi => "Pi (MS Coding Agent)",
            Self::Antigravity => "Antigravity",
            Self::Gemini => "Gemini CLI",
            Self::OpenCode => "OpenCode",
            Self::KiloCli => "Kilo CLI",
            Self::ClaudeVsCode => "Claude Code (VS Code)",
            Self::KiloVsCode => "Kilo Code (VS Code)",
            Self::OpenClaw => "OpenClaw",
            Self::Hermes => "Hermes",
            Self::GrokBuild => "Grok Build",
        }
    }
}

/// Result of writing a provider's config to the live file.
#[derive(Debug, Serialize)]
pub struct LiveWriteResult {
    pub app_type: String,
    pub provider_id: String,
    /// Paths of files written.
    pub written: Vec<String>,
    /// Path of the backup file created (if any).
    pub backup: Option<String>,
}

/// Write a provider's `settings_config` to the live config file(s) for `app_type`.
/// Creates a backup of the existing file(s) first. Mirrors cc-switch's
/// `write_live_snapshot` (`services/provider/live.rs:998`).
///
/// `codex_cli_home` optionally overrides the Codex config directory (isolated
/// CODEX_HOME); only used for `AppType::Codex`. `None` = default `~/.codex`.
///
/// VS Code extensions (`claude_vscode`, `kilo_vscode`) use AAG as the
/// unified upstream — see [`crate::services::provider::vscode_extension`].
#[allow(dead_code)] // live-config write path retained for tests/history; not called in the app since Phase 15.9
pub fn write_live_snapshot(
    app_type: AppType,
    provider: &Provider,
    codex_cli_home: Option<&str>,
) -> Result<LiveWriteResult, AppError> {
    match app_type {
        AppType::ClaudeCode => claude_code::write_live(provider),
        AppType::Codex => codex::write_live(provider, codex_cli_home),
        AppType::Copilot => copilot::write_live(provider),
        AppType::Gemini => gemini::write_live(provider),
        AppType::Pi => pi::write_live(provider),
        AppType::Antigravity => antigravity::write_live(provider),
        AppType::OpenCode => opencode::write_live(provider),
        AppType::KiloCli => kilo::write_live(provider),
        AppType::ClaudeVsCode => {
            vscode_extension::write_live(vscode_extension::VsCodeApp::ClaudeCode, provider)
        }
        AppType::KiloVsCode => {
            vscode_extension::write_live(vscode_extension::VsCodeApp::KiloCode, provider)
        }
        AppType::ClaudeDesktop | AppType::OpenClaw | AppType::Hermes | AppType::GrokBuild => {
            Err(AppError::Message(format!(
            "live config writing for {} not yet implemented (Phase 1 covers Claude Code + Codex)",
            app_type.display_name()
        )))
        }
    }
}

/// Check whether an app appears to be installed (config dir/file exists).
#[allow(dead_code)]
pub fn detect_app(app_type: AppType) -> bool {
    match app_type {
        AppType::ClaudeCode => claude_code::detect(),
        AppType::Codex => codex::detect(),
        AppType::Copilot => copilot::detect(),
        AppType::Gemini => gemini::detect(),
        AppType::Pi => pi::detect(),
        AppType::Antigravity => antigravity::detect(),
        AppType::OpenCode => opencode::detect(),
        AppType::KiloCli => kilo::detect(),
        AppType::ClaudeVsCode | AppType::KiloVsCode => vscode_extension::detect(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_type_roundtrip_all_known_values() {
        for (s, expected) in [
            ("claude_code", AppType::ClaudeCode),
            ("claude_desktop", AppType::ClaudeDesktop),
            ("codex", AppType::Codex),
            ("copilot", AppType::Copilot),
            ("pi", AppType::Pi),
            ("antigravity", AppType::Antigravity),
            ("gemini", AppType::Gemini),
            ("opencode", AppType::OpenCode),
            ("kilo_cli", AppType::KiloCli),
            ("claude_vscode", AppType::ClaudeVsCode),
            ("kilo_vscode", AppType::KiloVsCode),
            ("openclaw", AppType::OpenClaw),
            ("hermes", AppType::Hermes),
            ("grok_build", AppType::GrokBuild),
        ] {
            assert_eq!(AppType::from_db_str(s).unwrap(), expected, "{s}");
            assert_eq!(expected.to_db_str(), s, "roundtrip for {s}");
        }
        assert!(AppType::from_db_str("bogus").is_err());
    }

    #[test]
    fn antigravity_dispatch_returns_disabled() {
        let p = crate::provider::Provider::new("x".into(), "X".into(), serde_json::json!({}));
        let err = write_live_snapshot(AppType::Antigravity, &p, None)
            .expect_err("Antigravity switching disabled");
        assert!(err.to_string().contains("not supported yet"));
    }
}
