//! Deep-link URL parser (Phase 9 Step 4).
//!
//! v1 grammar: `aag://<action>/<id>` with optional `?app=<app_type>` query.
//!
//! Supported actions:
//! - `aag://switch/<provider_id>`     — switch active provider to `<id>`
//!   (optional `?app=claude_code` query picks the app, defaults to `claude_code`)
//! - `aag://provider/<provider_id>`   — open the providers page focused on `<id>`
//! - `aag://mcp/<server_id>`          — open the MCP page focused on `<id>`
//! - `aag://skill/<skill_id>`         — open the Skills page focused on `<id>`
//! - `aag://deeplink`                 — open the Deep links page (no id)
//!
//! Unknown action or unparseable URL → `DeeplinkAction::Unknown { raw }` so
//! the frontend can render it without crashing.
//!
//! Reference: cc-switch's `deeplink/parser.rs:15` (`parse_deeplink_url`).
//! AAG's grammar is simpler because we don't have an "import" workflow —
//! just route to the corresponding local action.

use url::Url;

/// All deep-link action variants the AAG app knows how to interpret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeeplinkAction {
    /// Switch the active provider to `provider_id` for `app_type` (default claude_code).
    SwitchProvider {
        provider_id: String,
        app_type: String,
    },
    /// Open the Providers page and focus `provider_id`.
    FocusProvider { provider_id: String },
    /// Open the MCP page and focus `server_id`.
    FocusMcp { server_id: String },
    /// Open the Skills page and focus `skill_id`.
    FocusSkill { skill_id: String },
    /// Open the Deep links page.
    ShowDeeplinks,
    /// URL didn't match any known action — keep the raw text for the UI.
    Unknown { raw: String, reason: String },
}

impl DeeplinkAction {
    /// Stable kind label (for the UI to group/filter).
    #[allow(dead_code)] // Used by the unit tests in the same file; not yet by the lib's binary surface.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::SwitchProvider { .. } => "switch",
            Self::FocusProvider { .. } => "provider",
            Self::FocusMcp { .. } => "mcp",
            Self::FocusSkill { .. } => "skill",
            Self::ShowDeeplinks => "deeplinks",
            Self::Unknown { .. } => "unknown",
        }
    }
}

/// Parse a single `aag://` URL into a `DeeplinkAction`. Returns
/// `DeeplinkAction::Unknown` for parse errors or unknown actions — never
/// panics, never errors.
pub fn parse(url_str: &str) -> DeeplinkAction {
    let trimmed = url_str.trim();
    if !trimmed.starts_with("aag://") {
        return DeeplinkAction::Unknown {
            raw: trimmed.to_string(),
            reason: "scheme must be aag://".into(),
        };
    }

    let parsed = match Url::parse(trimmed) {
        Ok(u) => u,
        Err(e) => {
            return DeeplinkAction::Unknown {
                raw: trimmed.to_string(),
                reason: format!("URL parse failed: {e}"),
            };
        }
    };

    // For `aag://switch/foo`, the `url` crate puts "switch" in the host slot
    // (because there's no //<host>/<path> split — `aag://switch/foo` has
    // host="switch", path="/foo"). Read it that way.
    let host = match parsed.host_str() {
        Some(h) if !h.is_empty() => h.to_string(),
        _ => {
            // Fallback: parse the URL by hand for the "no host" form
            // (e.g. `aag:something`).
            return DeeplinkAction::Unknown {
                raw: trimmed.to_string(),
                reason: "missing action segment".into(),
            };
        }
    };

    // Strip leading "/" from path to get the id segment.
    let id = parsed.path().trim_start_matches('/');
    let id_opt = if id.is_empty() { None } else { Some(id.to_string()) };

    // Optional ?app=<app_type> query.
    let mut app_type: Option<String> = None;
    for (k, v) in parsed.query_pairs() {
        if k == "app" {
            app_type = Some(v.into_owned());
        }
    }

    match host.as_str() {
        "switch" => {
            let provider_id = id_opt.unwrap_or_else(|| "".to_string());
            if provider_id.is_empty() {
                DeeplinkAction::Unknown {
                    raw: trimmed.to_string(),
                    reason: "switch action requires /<provider_id>".into(),
                }
            } else {
                DeeplinkAction::SwitchProvider {
                    provider_id,
                    app_type: app_type.unwrap_or_else(|| "claude_code".to_string()),
                }
            }
        }
        "provider" => id_opt
            .map(|provider_id| DeeplinkAction::FocusProvider { provider_id })
            .unwrap_or_else(|| DeeplinkAction::Unknown {
                raw: trimmed.to_string(),
                reason: "provider action requires /<provider_id>".into(),
            }),
        "mcp" => id_opt
            .map(|server_id| DeeplinkAction::FocusMcp { server_id })
            .unwrap_or_else(|| DeeplinkAction::Unknown {
                raw: trimmed.to_string(),
                reason: "mcp action requires /<server_id>".into(),
            }),
        "skill" => id_opt
            .map(|skill_id| DeeplinkAction::FocusSkill { skill_id })
            .unwrap_or_else(|| DeeplinkAction::Unknown {
                raw: trimmed.to_string(),
                reason: "skill action requires /<skill_id>".into(),
            }),
        "deeplink" | "deeplinks" => DeeplinkAction::ShowDeeplinks,
        other => DeeplinkAction::Unknown {
            raw: trimmed.to_string(),
            reason: format!("unknown action: {other}"),
        },
    }
}

// ─── Tests ───────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_switch_with_default_app() {
        let a = parse("aag://switch/claude-1");
        assert_eq!(
            a,
            DeeplinkAction::SwitchProvider {
                provider_id: "claude-1".into(),
                app_type: "claude_code".into(),
            }
        );
        assert_eq!(a.kind(), "switch");
    }

    #[test]
    fn parse_switch_with_explicit_app() {
        let a = parse("aag://switch/codex-default?app=codex");
        assert_eq!(
            a,
            DeeplinkAction::SwitchProvider {
                provider_id: "codex-default".into(),
                app_type: "codex".into(),
            }
        );
    }

    #[test]
    fn parse_provider_focus() {
        let a = parse("aag://provider/claude-1");
        assert_eq!(
            a,
            DeeplinkAction::FocusProvider {
                provider_id: "claude-1".into()
            }
        );
        assert_eq!(a.kind(), "provider");
    }

    #[test]
    fn parse_mcp_focus() {
        let a = parse("aag://mcp/server-2");
        assert_eq!(
            a,
            DeeplinkAction::FocusMcp {
                server_id: "server-2".into()
            }
        );
        assert_eq!(a.kind(), "mcp");
    }

    #[test]
    fn parse_skill_focus() {
        let a = parse("aag://skill/python-lint");
        assert_eq!(
            a,
            DeeplinkAction::FocusSkill {
                skill_id: "python-lint".into()
            }
        );
        assert_eq!(a.kind(), "skill");
    }

    #[test]
    fn parse_deeplinks_self() {
        let a = parse("aag://deeplink");
        assert_eq!(a, DeeplinkAction::ShowDeeplinks);
        assert_eq!(a.kind(), "deeplinks");
    }

    #[test]
    fn parse_unknown_action_returns_unknown() {
        let a = parse("aag://whatever/foo");
        assert!(matches!(a, DeeplinkAction::Unknown { .. }));
        assert_eq!(a.kind(), "unknown");
    }

    #[test]
    fn parse_wrong_scheme_returns_unknown() {
        let a = parse("https://example.com");
        assert!(matches!(a, DeeplinkAction::Unknown { .. }));
    }

    #[test]
    fn parse_switch_without_id_returns_unknown() {
        let a = parse("aag://switch");
        assert!(matches!(a, DeeplinkAction::Unknown { .. }));
    }

    #[test]
    fn parse_garbage_returns_unknown() {
        let a = parse("not a url at all");
        assert!(matches!(a, DeeplinkAction::Unknown { .. }));
    }

    #[test]
    fn parse_trims_whitespace() {
        let a = parse("  aag://switch/claude-1  ");
        assert_eq!(
            a,
            DeeplinkAction::SwitchProvider {
                provider_id: "claude-1".into(),
                app_type: "claude_code".into(),
            }
        );
    }
}