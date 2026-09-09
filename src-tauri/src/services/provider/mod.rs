//! Provider service - live config writing per AI coding tool.
//!
//! Mirrors cc-switch's `services/provider/live.rs` `write_live_snapshot`
//! (`live.rs:998`). Each app writes its config in a different format:
//! - Claude Code: `~/.claude/settings.json` (JSON)
//! - Codex: `~/.codex/config.toml` (TOML) + `~/.codex/auth.json` (JSON)
//! - Others: stubbed for later phases.

pub mod claude_code;
pub mod codex;

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
    Gemini,
    OpenCode,
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
            "gemini" => Ok(Self::Gemini),
            "opencode" => Ok(Self::OpenCode),
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
            Self::Gemini => "gemini",
            Self::OpenCode => "opencode",
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
            Self::Gemini => "Gemini CLI",
            Self::OpenCode => "OpenCode",
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
pub fn write_live_snapshot(
    app_type: AppType,
    provider: &Provider,
) -> Result<LiveWriteResult, AppError> {
    match app_type {
        AppType::ClaudeCode => claude_code::write_live(provider),
        AppType::Codex => codex::write_live(provider),
        AppType::ClaudeDesktop
        | AppType::Gemini
        | AppType::OpenCode
        | AppType::OpenClaw
        | AppType::Hermes
        | AppType::GrokBuild => Err(AppError::Message(format!(
            "live config writing for {} not yet implemented (Phase 1 covers Claude Code + Codex)",
            app_type.display_name()
        ))),
    }
}

/// Check whether an app appears to be installed (config dir/file exists).
#[allow(dead_code)]
pub fn detect_app(app_type: AppType) -> bool {
    match app_type {
        AppType::ClaudeCode => claude_code::detect(),
        AppType::Codex => codex::detect(),
        _ => false,
    }
}