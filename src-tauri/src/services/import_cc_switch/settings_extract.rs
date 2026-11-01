//! Pure settings_config parsing: turn a cc-switch provider's
//! `settings_config` JSON into upstream coordinates, extra headers,
//! and wire-protocol verdicts. No database access, no mutation —
//! every function here is a mapping over JSON/TOML.
//!
//! Parsing is strict and pure: the Codex `config` TOML is parsed with a
//! real TOML parser (`toml_edit`), protocol selection comes from
//! explicit declarations (`meta.apiFormat`, active Codex TOML
//! `wire_api`) rather than URL guessing, and unknown declaration
//! tokens are surfaced to the caller instead of being coerced.

use crate::services::import_cc_switch::cc_switch_db::CcSwitchMeta;

/// The four client wire protocols a supplier endpoint can speak.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WireProtocol {
    Anthropic,
    OpenaiChat,
    OpenaiResponses,
    Gemini,
}

impl WireProtocol {
    /// The canonical supplier endpoint protocol id.
    pub(crate) fn supplier_protocol(self) -> &'static str {
        match self {
            WireProtocol::Anthropic => crate::database::dao::suppliers::PROTOCOL_ANTHROPIC,
            WireProtocol::OpenaiChat => crate::database::dao::suppliers::PROTOCOL_OPENAI_CHAT,
            WireProtocol::OpenaiResponses => {
                crate::database::dao::suppliers::PROTOCOL_OPENAI_RESPONSES
            }
            WireProtocol::Gemini => crate::database::dao::suppliers::PROTOCOL_GEMINI,
        }
    }

    /// The legacy `proxy_providers.type` that can carry this protocol.
    /// The legacy pool has no Responses-native type; `None` means the
    /// legacy projection must not pretend to serve it.
    pub(crate) fn legacy_provider_type(self) -> Option<&'static str> {
        match self {
            WireProtocol::Anthropic => Some("anthropic"),
            // The legacy pool is Chat-shaped; a Responses-native upstream
            // has no faithful legacy projection. Callers that must keep
            // legacy import behaviour use `legacy_provider_type_lossy`.
            WireProtocol::OpenaiChat => Some("openai"),
            WireProtocol::OpenaiResponses => Some("openai-responses"),
            WireProtocol::Gemini => Some("gemini"),
        }
    }

    /// Legacy import compatibility: what the old `ccs-*` pool row used to
    /// claim. Responses-native rows still land as `openai` here because
    /// that historic path has no Responses adapter; the supplier import
    /// records the truthful `openai-responses` protocol instead.
    pub(crate) fn legacy_provider_type_lossy(self) -> &'static str {
        match self {
            WireProtocol::Anthropic => "anthropic",
            WireProtocol::OpenaiChat | WireProtocol::OpenaiResponses => "openai",
            WireProtocol::Gemini => "gemini",
        }
    }
}

/// Map a declared apiFormat / wire_api token onto a wire protocol using
/// the same alias sets cc-switch accepts. `None` = unrecognized token.
fn map_wire_token(token: &str) -> Option<WireProtocol> {
    match token.trim().to_ascii_lowercase().as_str() {
        "anthropic" | "anthropic_messages" | "anthropic-messages" | "claude" | "messages" => {
            Some(WireProtocol::Anthropic)
        }
        "chat"
        | "chat_completions"
        | "chat-completions"
        | "openai_chat"
        | "openai-chat"
        | "openai_chat_completions"
        | "openai" => Some(WireProtocol::OpenaiChat),
        "responses" | "response" | "openai_responses" | "openai-responses" | "openai_response" => {
            Some(WireProtocol::OpenaiResponses)
        }
        "gemini" | "google_gemini" | "google-gemini" => Some(WireProtocol::Gemini),
        _ => None,
    }
}

/// Protocol declared by `meta.apiFormat` or the active Codex TOML
/// `wire_api`. `Some(Err(token))` means a declaration exists but its
/// token is unknown — never guess a protocol for it.
pub(crate) fn declared_wire_protocol(
    api_format: Option<&str>,
    wire_api: Option<&str>,
) -> Option<Result<WireProtocol, String>> {
    if let Some(token) = api_format.map(str::trim).filter(|t| !t.is_empty()) {
        return Some(map_wire_token(token).ok_or_else(|| token.to_string()));
    }
    if let Some(token) = wire_api.map(str::trim).filter(|t| !t.is_empty()) {
        return Some(map_wire_token(token).ok_or_else(|| token.to_string()));
    }
    None
}

/// Per-app default protocol when nothing is declared. `None` for app
/// types the importer does not model (their rows are reported, never
/// guessed).
pub(crate) fn default_wire_protocol(
    app_type: &str,
    cfg: &serde_json::Value,
) -> Option<WireProtocol> {
    match app_type {
        "claude_code" | "claude" | "claude_desktop" => Some(WireProtocol::Anthropic),
        "codex" => Some(WireProtocol::OpenaiResponses),
        "gemini" => Some(WireProtocol::Gemini),
        "opencode" => {
            let npm = cfg
                .get("npm")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            if npm.contains("anthropic") {
                Some(WireProtocol::Anthropic)
            } else {
                Some(WireProtocol::OpenaiChat)
            }
        }
        "pi" => match cfg
            .get("api")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
        {
            "anthropic-messages" => Some(WireProtocol::Anthropic),
            _ => Some(WireProtocol::OpenaiChat),
        },
        "copilot" => {
            let wire = settings_env(cfg)
                .get("COPILOT_PROVIDER_TYPE")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("openai");
            if wire == "anthropic" {
                Some(WireProtocol::Anthropic)
            } else {
                Some(WireProtocol::OpenaiChat)
            }
        }
        _ => None,
    }
}

/// Borrow a settings_config's `env` object. A missing or non-object env
/// yields a shared empty map, so callers can read keys uniformly without
/// cloning the block per row.
pub(crate) fn settings_env(cfg: &serde_json::Value) -> &serde_json::Map<String, serde_json::Value> {
    static EMPTY: std::sync::OnceLock<serde_json::Map<String, serde_json::Value>> =
        std::sync::OnceLock::new();
    cfg.get("env")
        .and_then(serde_json::Value::as_object)
        .unwrap_or_else(|| EMPTY.get_or_init(Default::default))
}

/// Collect a codex settings_config's `modelCatalog.models[].model` list
/// (non-empty strings only). Shared by the route-input collector and the
/// pool-row builder so both always see the same catalog.
pub(crate) fn codex_catalog_models(cfg: &serde_json::Value) -> Vec<String> {
    cfg.get("modelCatalog")
        .and_then(|catalog| catalog.get("models"))
        .and_then(serde_json::Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.get("model").and_then(serde_json::Value::as_str))
                .filter(|model| !model.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

/// One Codex `config.toml` snapshot: the active provider's (or the only
/// provider's) `base_url` / `wire_api`, plus the top-level fallbacks.
/// Parse failure is an error — a config AAG cannot parse must surface,
/// not be line-scanned into a plausible-looking guess.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct CodexTomlInfo {
    pub(crate) base_url: Option<String>,
    pub(crate) wire_api: Option<String>,
}

/// Strictly parse the Codex `config` TOML text. Mirrors cc-switch's
/// resolution order: the entry named by top-level `model_provider`,
/// else the first `model_providers` entry, else the top-level keys.
pub(crate) fn parse_codex_toml(config_text: &str) -> Result<CodexTomlInfo, String> {
    if config_text.trim().is_empty() {
        return Ok(CodexTomlInfo::default());
    }
    let doc = config_text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| format!("{error}"))?;
    let active = doc
        .get("model_provider")
        .and_then(toml_edit::Item::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let providers = doc
        .get("model_providers")
        .and_then(toml_edit::Item::as_table_like);
    let entry = providers.and_then(|table| {
        if let Some(name) = active.as_deref() {
            if let Some(item) = table.get(name) {
                return item.as_table_like();
            }
        }
        table
            .iter()
            .next()
            .and_then(|(_, item)| item.as_table_like())
    });
    let pick = |key: &str| -> Option<String> {
        entry
            .and_then(|table| table.get(key))
            .and_then(toml_edit::Item::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    let top = |key: &str| -> Option<String> {
        doc.get(key)
            .and_then(toml_edit::Item::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    Ok(CodexTomlInfo {
        base_url: pick("base_url").or_else(|| top("base_url")),
        wire_api: pick("wire_api").or_else(|| top("wire_api")),
    })
}

/// True when a base URL's host is the official Anthropic API. Used to
/// detect cc-switch subscription rows (official base + empty key): those
/// rows belong to a ChatGPT-style subscription that will authenticate
/// with the user's OAuth credential, not a static key, and are imported
/// as `anthropic_passthrough` pool rows instead.
///
/// Exact host comparison via URL parse — an unbounded prefix match here
/// would classify `api.anthropic.com.evil.tld` as official and silently
/// relay client OAuth tokens to the attacker host. A non-default port is
/// also rejected (the official endpoint never carries one).
pub(crate) fn base_host_is_official_anthropic(base_url: &str) -> bool {
    let trimmed = base_url.trim();
    let candidate = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };
    match url::Url::parse(&candidate) {
        Ok(parsed) => {
            parsed.port().is_none()
                && parsed
                    .host_str()
                    .is_some_and(|host| host.eq_ignore_ascii_case("api.anthropic.com"))
        }
        Err(_) => false,
    }
}

/// True when a base URL's host is the official OpenAI API. Same exact
/// host rule as [`base_host_is_official_anthropic`].
pub(crate) fn base_host_is_official_openai(base_url: &str) -> bool {
    let trimmed = base_url.trim();
    let candidate = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };
    match url::Url::parse(&candidate) {
        Ok(parsed) => {
            parsed.port().is_none()
                && parsed
                    .host_str()
                    .is_some_and(|host| host.eq_ignore_ascii_case("api.openai.com"))
        }
        Err(_) => false,
    }
}

/// Legacy entry point kept for the old `ccs-*` import path: returns
/// `(base_url, api_key, legacy_provider_type)`. Unrecognized or missing
/// declarations fall back to the historic per-app behaviour so old
/// imports keep their shape; new code must use
/// [`extract_upstream_coordinates`].
pub(crate) fn extract_upstream_from_settings(
    app_type: &str,
    cfg: &serde_json::Value,
    meta: Option<&CcSwitchMeta>,
) -> Option<(String, String, String)> {
    let api_format = meta.and_then(|meta| meta.api_format.as_deref());
    let wire_api = codex_wire_api(cfg);
    let declared = declared_wire_protocol(api_format, wire_api.as_deref());
    let (base_url, api_key) = extract_upstream_pair(app_type, cfg, meta)?;
    if base_url.is_empty() {
        return None;
    }
    let provider_type = match declared {
        Some(Ok(protocol)) => protocol.legacy_provider_type_lossy(),
        Some(Err(_)) | None => legacy_url_heuristic(app_type, cfg, &base_url)?,
    };
    Some((base_url, api_key, provider_type.to_string()))
}

/// The active Codex TOML `wire_api` (strict parse; malformed TOML yields
/// no declaration rather than a line-scan guess).
pub(crate) fn codex_wire_api(cfg: &serde_json::Value) -> Option<String> {
    cfg.get("config")
        .and_then(serde_json::Value::as_str)
        .and_then(|toml| parse_codex_toml(toml).ok())
        .and_then(|info| info.wire_api)
}

/// Historic provider-type fallback for the legacy import path only.
/// Mirrors the old heuristics exactly so existing imports keep shape.
fn legacy_url_heuristic(
    app_type: &str,
    cfg: &serde_json::Value,
    base_url: &str,
) -> Option<&'static str> {
    match app_type {
        "claude_code" | "claude" => Some("anthropic"),
        "codex" => Some(if base_url.contains("anthropic") {
            "anthropic"
        } else {
            "openai"
        }),
        "opencode" => {
            let npm = cfg
                .get("npm")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            Some(if npm.contains("anthropic") {
                "anthropic"
            } else {
                "openai"
            })
        }
        "copilot" => {
            let wire = settings_env(cfg)
                .get("COPILOT_PROVIDER_TYPE")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("openai");
            Some(if wire == "anthropic" {
                "anthropic"
            } else {
                "openai"
            })
        }
        "pi" => Some(
            match cfg
                .get("api")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
            {
                "anthropic-messages" => "anthropic",
                _ => "openai",
            },
        ),
        "gemini" => Some("gemini"),
        _ => None,
    }
}

/// Base URL + api key extraction per app type. `apiKeyField` from
/// `meta` selects the Claude credential env key; Codex TOML `base_url`
/// comes from the strict parser.
pub(crate) fn extract_upstream_pair(
    app_type: &str,
    cfg: &serde_json::Value,
    meta: Option<&CcSwitchMeta>,
) -> Option<(String, String)> {
    let env = settings_env(cfg);
    match app_type {
        "claude_code" | "claude" | "claude_desktop" => {
            let base = env.get("ANTHROPIC_BASE_URL")?.as_str()?.to_string();
            if base.is_empty() {
                return None;
            }
            let key_field = meta
                .and_then(|meta| meta.api_key_field.as_deref())
                .filter(|field| !field.is_empty());
            let key = key_field
                .and_then(|field| env.get(field))
                .and_then(serde_json::Value::as_str)
                .or_else(|| {
                    env.get("ANTHROPIC_AUTH_TOKEN")
                        .and_then(serde_json::Value::as_str)
                })
                .or_else(|| {
                    env.get("ANTHROPIC_API_KEY")
                        .and_then(serde_json::Value::as_str)
                })
                .unwrap_or("")
                .to_string();
            Some((base, key))
        }
        "codex" => {
            // cc-switch stores the key under `auth.OPENAI_API_KEY` (a
            // top-level `apiKey` is unusual). ChatGPT-login providers
            // (codex-official / default) have a null key — the empty
            // string is how the importer detects them.
            let toml_info = cfg
                .get("config")
                .and_then(serde_json::Value::as_str)
                .and_then(|toml| parse_codex_toml(toml).ok())
                .unwrap_or_default();
            let base = cfg
                .get("baseUrl")
                .and_then(serde_json::Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .or(toml_info.base_url)?;
            let key = cfg
                .get("auth")
                .and_then(|a| a.get("OPENAI_API_KEY"))
                .and_then(serde_json::Value::as_str)
                .filter(|s| !s.is_empty())
                .or_else(|| cfg.get("apiKey").and_then(serde_json::Value::as_str))
                .unwrap_or("")
                .to_string();
            Some((base, key))
        }
        "opencode" => {
            let options = cfg.get("options").and_then(serde_json::Value::as_object)?;
            let base = options
                .get("baseURL")
                .and_then(serde_json::Value::as_str)
                .filter(|s| !s.is_empty())?
                .to_string();
            let key = options
                .get("apiKey")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string();
            Some((base, key))
        }
        "copilot" => {
            let base = env.get("COPILOT_PROVIDER_BASE_URL")?.as_str()?.to_string();
            if base.is_empty() {
                return None;
            }
            let key = env
                .get("COPILOT_PROVIDER_API_KEY")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string();
            Some((base, key))
        }
        "pi" => {
            let base = cfg
                .get("baseUrl")
                .and_then(serde_json::Value::as_str)
                .filter(|s| !s.is_empty())?
                .to_string();
            let key = cfg
                .get("apiKey")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string();
            Some((base, key))
        }
        "gemini" => {
            // cc-switch gemini providers keep the upstream in
            // env.GOOGLE_GEMINI_BASE_URL + env.GEMINI_API_KEY and select
            // the wire format via the meta column's apiFormat.
            let base = env
                .get("GOOGLE_GEMINI_BASE_URL")
                .and_then(serde_json::Value::as_str)?
                .to_string();
            if base.is_empty() {
                return None;
            }
            let key = env
                .get("GEMINI_API_KEY")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string();
            Some((base, key))
        }
        _ => None,
    }
}

/// Provider-mandated custom request headers declared in a cc-switch
/// settings snapshot: `options.headers` (OpenCode/Kilo) or the Claude
/// `env.ANTHROPIC_CUSTOM_HEADERS` JSON-object string. Pure: only
/// declared values are returned — no session header is invented here
/// (the legacy pool filler may still inject one).
pub(crate) fn extract_declared_headers(
    app_type: &str,
    cfg: &serde_json::Value,
) -> std::collections::HashMap<String, String> {
    match app_type {
        "opencode" => cfg
            .get("options")
            .and_then(|o| o.get("headers"))
            .and_then(|h| h.as_object())
            .map(|obj| {
                obj.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default(),
        "claude_code" | "claude" | "claude_desktop" => cfg
            .get("env")
            .and_then(|e| e.get("ANTHROPIC_CUSTOM_HEADERS"))
            .and_then(|v| v.as_str())
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
            .and_then(|v| v.as_object().cloned())
            .map(|obj| {
                obj.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default(),
        _ => std::collections::HashMap::new(),
    }
}

/// Extract provider-mandated custom request headers from a cc-switch settings
/// snapshot. cc-switch carries them in `options.headers` (OpenCode/Kilo) or in
/// the Claude `env.ANTHROPIC_CUSTOM_HEADERS` value (a JSON object string).
/// Unknown/absent shapes yield an empty map — never a hard failure, since a
/// malformed header block must not abort an otherwise-importable provider.
///
/// When the resolved `base_url` host is `opencode.ai` and no session header is
/// present, inject a stable `x-opencode-session` so the imported provider works
/// out of the box (OpenCode's Go API rejects non-native clients without it).
pub(crate) fn extract_extra_headers_from_settings(
    app_type: &str,
    cfg: &serde_json::Value,
    base_url: &str,
) -> std::collections::HashMap<String, String> {
    let mut headers = extract_declared_headers(app_type, cfg);
    let is_opencode_host = base_url
        .split("://")
        .nth(1)
        .map(|rest| rest.split('/').next().unwrap_or(""))
        .map(|host| host.eq_ignore_ascii_case("opencode.ai") || host.ends_with(".opencode.ai"))
        .unwrap_or(false);
    if is_opencode_host
        && !headers
            .keys()
            .any(|k| k.eq_ignore_ascii_case("x-opencode-session"))
    {
        // Stable random session id via the crate's existing RNG (no uuid dep).
        // Any opaque value satisfies OpenCode's session requirement.
        let mut nonce = [0u8; 16];
        let session = match getrandom::getrandom(&mut nonce) {
            Ok(()) => format!(
                "aag-{}",
                nonce.iter().map(|b| format!("{b:02x}")).collect::<String>()
            ),
            Err(_) => format!(
                "aag-{}",
                chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
            ),
        };
        headers.insert("x-opencode-session".to_string(), session);
    }
    headers
}
