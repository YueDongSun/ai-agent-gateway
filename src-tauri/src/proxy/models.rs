//! Official Claude model catalog.
//!
//! The gateway advertises these ids on `/v1/models` so Anthropic-native
//! clients (Claude Code / Claude Desktop style integrations, the Anthropic
//! SDKs) see the same model list they would get from api.anthropic.com.
//! Each id is seeded as a failover route in `database/seed.rs`, so a request
//! for an official name flows through the hop chain while the response
//! echoes the requested name back to the client.

/// An official Anthropic model entry.
pub struct OfficialModel {
    pub id: &'static str,
    pub display_name: &'static str,
}

/// Current official Claude model ids (tier order).
pub const OFFICIAL_MODELS: &[OfficialModel] = &[
    OfficialModel {
        id: "claude-fable-5",
        display_name: "Claude Fable 5",
    },
    OfficialModel {
        id: "claude-opus-4-8",
        display_name: "Claude Opus 4.8",
    },
    OfficialModel {
        id: "claude-opus-4-7",
        display_name: "Claude Opus 4.7",
    },
    OfficialModel {
        id: "claude-opus-4-6",
        display_name: "Claude Opus 4.6",
    },
    OfficialModel {
        id: "claude-sonnet-5",
        display_name: "Claude Sonnet 5",
    },
    OfficialModel {
        id: "claude-sonnet-4-6",
        display_name: "Claude Sonnet 4.6",
    },
    OfficialModel {
        id: "claude-haiku-4-5",
        display_name: "Claude Haiku 4.5",
    },
];

/// Default Codex model entry (client-facing id + display name).
pub struct CodexModel {
    pub id: &'static str,
    pub display_name: &'static str,
}

/// Default Codex models that clients request. Each gets a seeded failover
/// route; the upstream model is configured per-provider in the route editor.
pub const CODEX_MODELS: &[CodexModel] = &[
    CodexModel {
        id: "codex-5-6-sol",
        display_name: "5.6Sol",
    },
    CodexModel {
        id: "codex-5-6-luna",
        display_name: "5.6Luna",
    },
    CodexModel {
        id: "codex-5-6-terra",
        display_name: "5.6Terra",
    },
    CodexModel {
        id: "codex-5-5",
        display_name: "5.5",
    },
];

/// Look up the official display name for a model id.
pub fn display_name_for(id: &str) -> Option<&'static str> {
    OFFICIAL_MODELS
        .iter()
        .find(|m| m.id == id)
        .map(|m| m.display_name)
}

/// Vendor-token spellings for derived display names.
fn pretty_token(token: &str) -> String {
    match token {
        "gpt" => "GPT".into(),
        "glm" => "GLM".into(),
        "ai" => "AI".into(),
        "minimax" => "MiniMax".into(),
        "deepseek" => "DeepSeek".into(),
        "qwen" => "Qwen".into(),
        "kimi" => "Kimi".into(),
        "gemini" => "Gemini".into(),
        "claude" => "Claude".into(),
        "codex" => "Codex".into(),
        "grok" => "Grok".into(),
        "llama" => "Llama".into(),
        "mistral" => "Mistral".into(),
        "openai" => "OpenAI".into(),
        other => {
            let mut c = other.chars();
            match c.next() {
                Some(first) => first.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        }
    }
}

/// Derive a human-readable label for a model id not present in the static
/// catalogs: `minimax-m3.1` -> "MiniMax M3.1", `gpt-6astsa` -> "GPT 6astsa".
/// Version dots stay inside their token; ids remain the source of truth.
pub fn pretty_display_name(id: &str) -> String {
    crate::proxy::strip_one_m_suffix(id)
        .split(['-', '_', '/', ' '])
        .filter(|t| !t.is_empty())
        .map(pretty_token)
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_models_are_unique_and_prefixed() {
        let mut seen = std::collections::HashSet::new();
        for m in OFFICIAL_MODELS {
            assert!(
                m.id.starts_with("claude-"),
                "id {} must start with claude-",
                m.id
            );
            assert!(!m.display_name.is_empty());
            assert!(seen.insert(m.id), "duplicate id {}", m.id);
        }
    }

    #[test]
    fn codex_models_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for m in CODEX_MODELS {
            assert!(
                m.id.starts_with("codex-"),
                "id {} must start with codex-",
                m.id
            );
            assert!(!m.display_name.is_empty());
            assert!(seen.insert(m.id), "duplicate id {}", m.id);
        }
    }

    #[test]
    fn display_name_lookup() {
        assert_eq!(display_name_for("claude-fable-5"), Some("Claude Fable 5"));
        assert_eq!(
            display_name_for("claude-haiku-4-5"),
            Some("Claude Haiku 4.5")
        );
        assert_eq!(display_name_for("gpt-4o"), None);
    }

    #[test]
    fn pretty_display_name_keeps_versions_and_vendor_spelling() {
        assert_eq!(pretty_display_name("minimax-m3.1"), "MiniMax M3.1");
        assert_eq!(pretty_display_name("gpt-6astsa"), "GPT 6astsa");
        assert_eq!(pretty_display_name("gemini-2.5-pro"), "Gemini 2.5 Pro");
        assert_eq!(pretty_display_name("deepseek-chat"), "DeepSeek Chat");
        assert_eq!(pretty_display_name("claude-sonnet-4-5[1m]"), "Claude Sonnet 4 5");
        assert_eq!(pretty_display_name(""), "");
    }
}
