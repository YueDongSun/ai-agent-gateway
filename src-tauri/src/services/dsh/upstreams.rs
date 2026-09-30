//! Resolve DSH model connections from AAG-owned provider snapshots.
//!
//! No live agent files are read or written here. Only opaque source ids and
//! model ids cross IPC; endpoint and credential resolution stays server-side.

use crate::database::dao::proxy_providers::ProxyProvider;
use crate::database::Database;
use crate::error::AppError;
use crate::provider::Provider;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use url::{Host, Url};

const API_KEY_ENV: &str = "AAG_DSH_API_KEY";
const AUTH_TOKEN_ENV: &str = "ANTHROPIC_AUTH_TOKEN";
const ANTHROPIC_PROTOCOL: &str = "anthropic-messages";
const OPENAI_PROTOCOL: &str = "openai-completions";
const MAX_IDENTIFIER_LENGTH: usize = 256;
const MAX_MODEL_LENGTH: usize = 256;
const MAX_LABEL_LENGTH: usize = 120;
const ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com";
const OPENAI_BASE_URL: &str = "https://api.openai.com/v1";

/// Redacted, read-only selection data; availability is local validation only.
#[allow(non_camel_case_types)]
#[derive(Clone, Debug, Serialize)]
pub struct Upstream_option {
    pub id: String,
    pub name: String,
    pub protocol: String,
    pub models: Vec<String>,
    pub source_kind: String,
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// One model from the actual running proxy snapshot, not current DB settings.
#[allow(non_camel_case_types)]
pub struct Gateway_route {
    pub alias: String,
    pub protocol: String,
}

/// Server-only snapshot supplied by the owner of the running proxy process.
/// Deliberately does not implement Debug or Serialize because it holds a key.
#[allow(non_camel_case_types)]
pub struct Gateway_snapshot {
    pub base_url: String,
    pub auth_key: String,
    pub routes: Vec<Gateway_route>,
}

/// Split persisted settings from process-only credentials. Never return this
/// value from IPC, print it, or include it in an operation receipt.
#[allow(non_camel_case_types)]
pub struct Resolved_upstream {
    pub llm_settings: Value,
    pub secret_env: Vec<(String, String)>,
}

#[allow(non_camel_case_types)]
struct Source_details {
    protocol: &'static str,
    base_url: String,
    credential: String,
    native_bearer: bool,
    models: BTreeMap<String, String>,
}

fn invalid(code: &'static str) -> AppError {
    AppError::InvalidInput(code.into())
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_IDENTIFIER_LENGTH
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn valid_model(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_MODEL_LENGTH
        && !value.contains("://")
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || matches!(character, '-' | '_' | '.' | '/' | ':' | '[' | ']')
        })
}

fn validate_key(key: &str) -> Result<(), AppError> {
    if key.is_empty() || key.trim().is_empty() {
        return Err(invalid("DSH_CREDENTIAL_UNAVAILABLE"));
    }
    if key
        .chars()
        .any(|character| character.is_control() || character.is_whitespace())
        || key.starts_with('$')
        || key.starts_with("env:")
        || key.chars().all(|character| character == '*')
    {
        return Err(invalid("DSH_CREDENTIAL_INVALID"));
    }
    Ok(())
}

/// Keep saved labels useful without returning embedded credentials or URLs.
fn safe_label(name: &str, key: &str) -> String {
    let redacted = if key.is_empty() {
        name.to_owned()
    } else {
        name.replace(key, "[credential]")
    };
    redacted
        .split_whitespace()
        .map(|part| {
            if part.contains("://") {
                "[endpoint]"
            } else {
                part
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|character| !character.is_control())
        .take(MAX_LABEL_LENGTH)
        .collect()
}

/// pi-ai's Anthropic SDK appends `/v1/messages` and its openai-completions
/// provider appends `/chat/completions`, so DSH needs the canonical API *root*
/// for the configured base URL. Delegate to the shared proxy normalizer so a
/// base stored as a full endpoint (`.../v1/messages`) is reduced exactly once
/// here too — matching how the proxy itself now treats the same row.
fn endpoint_for_dsh(base_url: &str, protocol: &str) -> Result<String, AppError> {
    let normalized = match protocol {
        ANTHROPIC_PROTOCOL => crate::proxy::endpoint::Protocol::Anthropic,
        _ => crate::proxy::endpoint::Protocol::Openai,
    };
    crate::proxy::endpoint::sdk_base_url(base_url, normalized)
        .map_err(|_| invalid("DSH_UPSTREAM_URL_INVALID"))
}

fn loopback_host(url: &Url) -> bool {
    match url.host() {
        Some(Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    }
}

/// A saved provider that actually points back at AAG must use gateway mode:
/// that mode owns the live credential and route aliases, not the saved row.
fn reject_gateway_endpoint(
    base_url: &str,
    gateway: Option<&Gateway_snapshot>,
) -> Result<(), AppError> {
    if let Some(gateway) = gateway {
        let source = Url::parse(base_url).map_err(|_| invalid("DSH_UPSTREAM_URL_INVALID"))?;
        let target =
            Url::parse(&gateway.base_url).map_err(|_| invalid("DSH_GATEWAY_SNAPSHOT_INVALID"))?;
        if source.port_or_known_default() == target.port_or_known_default()
            && (source.host() == target.host()
                || (loopback_host(&source) && loopback_host(&target)))
        {
            return Err(invalid("DSH_GATEWAY_SOURCE_REQUIRED"));
        }
    }
    Ok(())
}

fn native_env<'a>(provider: &'a Provider) -> Result<&'a serde_json::Map<String, Value>, AppError> {
    provider
        .settings_config
        .get("env")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid("DSH_CLAUDE_CONFIG_INVALID"))
}

fn env_text<'a>(
    env: &'a serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<&'a str>, AppError> {
    match env.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if value.is_empty() => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        Some(_) => Err(invalid("DSH_CLAUDE_CONFIG_INVALID")),
    }
}

fn native_models(provider: &Provider) -> Result<BTreeMap<String, String>, AppError> {
    let env = native_env(provider)?;
    let mut models = BTreeMap::new();
    // These aliases are interpreted by Claude Code, not by Anthropic HTTP.
    // They are usable here only when this snapshot supplies their mapping.
    let alias_fields = [
        ("haiku", "ANTHROPIC_DEFAULT_HAIKU_MODEL"),
        ("sonnet", "ANTHROPIC_DEFAULT_SONNET_MODEL"),
        ("opus", "ANTHROPIC_DEFAULT_OPUS_MODEL"),
        ("fable", "ANTHROPIC_DEFAULT_FABLE_MODEL"),
    ];
    for (alias, field) in alias_fields {
        if let Some(value) = env_text(env, field)? {
            insert_model(&mut models, value, value)?;
            insert_model(&mut models, alias, value)?;
        }
    }
    for field in [
        "ANTHROPIC_MODEL",
        "ANTHROPIC_DEFAULT_MODEL",
        "ANTHROPIC_SMALL_FAST_MODEL",
    ] {
        if let Some(value) = env_text(env, field)? {
            let normalized = crate::proxy::strip_one_m_suffix(value);
            if !["haiku", "sonnet", "opus", "fable"].contains(&normalized.as_str()) {
                insert_model(&mut models, value, value)?;
            }
        }
    }
    if let Some(value) = provider.settings_config.get("model") {
        let value = value
            .as_str()
            .ok_or_else(|| invalid("DSH_CLAUDE_CONFIG_INVALID"))?;
        let normalized = crate::proxy::strip_one_m_suffix(value);
        if !["haiku", "sonnet", "opus", "fable"].contains(&normalized.as_str()) {
            insert_model(&mut models, value, value)?;
        }
    }
    Ok(models)
}

fn insert_model(
    models: &mut BTreeMap<String, String>,
    alias: &str,
    wire_model: &str,
) -> Result<(), AppError> {
    let wire_model = crate::proxy::strip_one_m_suffix(wire_model);
    if !valid_model(alias) || !valid_model(&wire_model) {
        return Err(invalid("DSH_MODEL_INVALID"));
    }
    models.insert(alias.to_owned(), wire_model);
    Ok(())
}

fn native_details(provider: &Provider) -> Result<Source_details, AppError> {
    let env = native_env(provider)?;
    for field in [
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_USE_FOUNDRY",
    ] {
        if env_text(env, field)?.is_some_and(|value| value != "0" && value != "false") {
            return Err(invalid("DSH_NATIVE_AUTH_UNSUPPORTED"));
        }
    }
    if env_text(env, "ANTHROPIC_CUSTOM_HEADERS")?.is_some() {
        return Err(invalid("DSH_NATIVE_HEADERS_UNSUPPORTED"));
    }
    // Verified in pi-ai 0.82.1 providers/anthropic.js and DSH rc2 routeAuth:
    // the catalog Anthropic provider reads this process-only environment
    // token as Bearer auth. A hand-declared provider's apiKeyEnv instead
    // becomes x-api-key, which would change the saved Claude auth contract.
    let token = env_text(env, AUTH_TOKEN_ENV)?;
    let credential = match token {
        Some(token) => token,
        None => env_text(env, "ANTHROPIC_API_KEY")?.unwrap_or(""),
    }
    .to_owned();
    validate_key(&credential)?;
    let base_url = endpoint_for_dsh(
        env_text(env, "ANTHROPIC_BASE_URL")?.unwrap_or(ANTHROPIC_BASE_URL),
        ANTHROPIC_PROTOCOL,
    )?;
    let models = native_models(provider)?;
    if models.is_empty() {
        return Err(invalid("DSH_MODELS_UNAVAILABLE"));
    }
    Ok(Source_details {
        protocol: ANTHROPIC_PROTOCOL,
        base_url,
        credential,
        native_bearer: token.is_some(),
        models,
    })
}

fn proxy_models(provider: &ProxyProvider) -> Result<BTreeMap<String, String>, AppError> {
    let mut models = BTreeMap::new();
    for model in &provider.models {
        insert_model(&mut models, model, model)?;
    }
    for (alias, model) in &provider.model_mapping {
        insert_model(&mut models, alias, model)?;
    }
    Ok(models)
}

fn proxy_protocol(provider: &ProxyProvider) -> Result<&'static str, AppError> {
    match provider.provider_type.as_str() {
        "anthropic" => Ok(ANTHROPIC_PROTOCOL),
        "openai" => Ok(OPENAI_PROTOCOL),
        _ => Err(invalid("DSH_PROTOCOL_UNSUPPORTED")),
    }
}

fn proxy_details(provider: &ProxyProvider) -> Result<Source_details, AppError> {
    if !provider.enabled {
        return Err(invalid("DSH_UPSTREAM_DISABLED"));
    }
    let protocol = proxy_protocol(provider)?;
    validate_key(&provider.api_key)?;
    let base_url = if provider.base_url.is_empty() {
        if protocol == ANTHROPIC_PROTOCOL {
            ANTHROPIC_BASE_URL
        } else {
            OPENAI_BASE_URL
        }
    } else {
        &provider.base_url
    };
    let base_url = endpoint_for_dsh(base_url, protocol)?;
    let models = proxy_models(provider)?;
    if models.is_empty() {
        return Err(invalid("DSH_MODELS_UNAVAILABLE"));
    }
    Ok(Source_details {
        protocol,
        base_url,
        credential: provider.api_key.clone(),
        native_bearer: false,
        models,
    })
}

fn availability(
    details: &Result<Source_details, AppError>,
    gateway: Option<&Gateway_snapshot>,
) -> Option<String> {
    match details {
        Ok(details) => reject_gateway_endpoint(&details.base_url, gateway)
            .err()
            .map(|error| error.to_string()),
        Err(error) => Some(error.to_string()),
    }
}

/// Return locally selectable saved sources. No credential, endpoint, model
/// discovery request, live config write, or proxy lifecycle action crosses IPC.
pub fn list_upstreams(
    db: &Database,
    gateway: Option<&Gateway_snapshot>,
) -> Result<Vec<Upstream_option>, AppError> {
    let mut options = Vec::new();
    for (_, provider) in db.get_all_providers("claude_code")? {
        let details = native_details(&provider);
        let reason = availability(&details, gateway);
        let env = provider.settings_config.get("env");
        let key = env
            .and_then(|value| value.get("ANTHROPIC_API_KEY"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let token = env
            .and_then(|value| value.get("ANTHROPIC_AUTH_TOKEN"))
            .and_then(Value::as_str)
            .unwrap_or("");
        options.push(Upstream_option {
            id: format!("claude:{}", provider.id),
            name: safe_label(&safe_label(&provider.name, key), token),
            protocol: ANTHROPIC_PROTOCOL.into(),
            models: native_models(&provider)
                .map(|models| models.into_keys().collect())
                .unwrap_or_default(),
            source_kind: "claude_native".into(),
            available: reason.is_none(),
            reason,
        });
    }
    for provider in db.get_all_proxy_providers()? {
        let details = proxy_details(&provider);
        let reason = availability(&details, gateway);
        options.push(Upstream_option {
            id: format!("proxy:{}", provider.id),
            name: safe_label(&provider.name, &provider.api_key),
            protocol: proxy_protocol(&provider).unwrap_or("unsupported").into(),
            models: proxy_models(&provider)
                .map(|models| models.into_keys().collect())
                .unwrap_or_default(),
            source_kind: "proxy_pool".into(),
            available: reason.is_none(),
            reason,
        });
    }
    // A stopped proxy can show persisted route names as unavailable choices;
    // a running proxy must use only its immutable runtime model snapshot.
    let gateway_models = match gateway {
        Some(snapshot) => snapshot
            .routes
            .iter()
            .map(|route| route.alias.clone())
            .collect(),
        None => db
            .get_all_routes()?
            .into_iter()
            .map(|route| route.alias)
            .collect(),
    };
    let reason = gateway_details(gateway)
        .err()
        .map(|error| error.to_string());
    options.push(Upstream_option {
        id: "gateway".into(),
        name: "AAG gateway / Anthropic wire; upstream routes may convert".into(),
        protocol: ANTHROPIC_PROTOCOL.into(),
        models: gateway_models,
        source_kind: "gateway".into(),
        available: reason.is_none(),
        reason,
    });
    Ok(options)
}

fn gateway_details(gateway: Option<&Gateway_snapshot>) -> Result<Source_details, AppError> {
    let gateway = gateway.ok_or_else(|| invalid("DSH_GATEWAY_UNAVAILABLE"))?;
    validate_key(&gateway.auth_key)?;
    let mut models = BTreeMap::new();
    for route in &gateway.routes {
        if route.protocol != ANTHROPIC_PROTOCOL {
            return Err(invalid("DSH_GATEWAY_PROTOCOL_UNSUPPORTED"));
        }
        // Gateway alias is the exact wire model; its own router owns mapping.
        if !valid_model(&route.alias) {
            return Err(invalid("DSH_MODEL_INVALID"));
        }
        models.insert(route.alias.clone(), route.alias.clone());
    }
    if models.is_empty() {
        return Err(invalid("DSH_MODELS_UNAVAILABLE"));
    }
    let base_url = endpoint_for_dsh(&gateway.base_url, ANTHROPIC_PROTOCOL)?;
    let url = Url::parse(&base_url).map_err(|_| invalid("DSH_GATEWAY_SNAPSHOT_INVALID"))?;
    if !loopback_host(&url) {
        return Err(invalid("DSH_GATEWAY_NOT_LOOPBACK"));
    }
    Ok(Source_details {
        protocol: ANTHROPIC_PROTOCOL,
        base_url,
        credential: gateway.auth_key.clone(),
        native_bearer: false,
        models,
    })
}

/// Resolve again immediately before a start/apply action so deleted sources,
/// disabled providers, changed credentials, and stale models fail explicitly.
pub fn resolve_upstream(
    db: &Database,
    source_id: &str,
    model: &str,
    gateway: Option<&Gateway_snapshot>,
) -> Result<Resolved_upstream, AppError> {
    if !valid_identifier(source_id) || !valid_model(model) {
        return Err(invalid("DSH_SELECTION_INVALID"));
    }
    let details = if source_id == "gateway" {
        gateway_details(gateway)?
    } else if let Some(id) = source_id.strip_prefix("claude:") {
        if !valid_identifier(id) {
            return Err(invalid("DSH_SELECTION_INVALID"));
        }
        let provider = db
            .get_provider_by_id(id, "claude_code")?
            .ok_or_else(|| invalid("DSH_UPSTREAM_NOT_FOUND"))?;
        native_details(&provider)?
    } else if let Some(id) = source_id.strip_prefix("proxy:") {
        if !valid_identifier(id) {
            return Err(invalid("DSH_SELECTION_INVALID"));
        }
        let provider = db
            .get_all_proxy_providers()?
            .into_iter()
            .find(|provider| provider.id == id)
            .ok_or_else(|| invalid("DSH_UPSTREAM_NOT_FOUND"))?;
        proxy_details(&provider)?
    } else {
        return Err(invalid("DSH_SOURCE_KIND_INVALID"));
    };
    if source_id != "gateway" {
        reject_gateway_endpoint(&details.base_url, gateway)?;
    }
    let wire_model = details
        .models
        .get(model)
        .ok_or_else(|| invalid("DSH_MODEL_NOT_FOUND"))?;
    let provider_key = if details.native_bearer {
        "anthropic"
    } else {
        "aag"
    };
    let credential_env = if details.native_bearer {
        AUTH_TOKEN_ENV
    } else {
        API_KEY_ENV
    };
    let mut profile = json!({
        "displayName": "AAG selected upstream",
        "api": details.protocol,
        "baseURL": details.base_url,
        "models": [{"id": wire_model}]
    });
    // Native Bearer must reach the catalog's ambient auth resolver without
    // an API-key override. The lifecycle owner must reject a pre-existing
    // managed DSH credential store, whose values outrank either environment
    // route; it must also launch with a cleared, explicitly populated env.
    if !details.native_bearer {
        profile["apiKeyEnv"] = json!(API_KEY_ENV);
    }
    let llm_settings = json!({
        "llm-pi-ai": {
            "providers": {
                (provider_key): profile
            }
        },
        "agent-default-model": {"provider": provider_key, "model": wire_model}
    });
    Ok(Resolved_upstream {
        llm_settings,
        secret_env: vec![(credential_env.into(), details.credential)],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::dao::proxy_providers::CircuitBreakerConfig;
    use crate::database::dao::routes::{Route, RouteHop};

    fn native_provider() -> Provider {
        Provider::new(
            "native".into(),
            "Native Claude".into(),
            json!({
                "env": {
                    "ANTHROPIC_BASE_URL": "https://native.example/anthropic/v1/",
                    "ANTHROPIC_API_KEY": "fixture_native_key",
                    "ANTHROPIC_MODEL": "sonnet",
                    "ANTHROPIC_DEFAULT_SONNET_MODEL": "claude_fixture[1M]"
                }
            }),
        )
    }

    fn pool_provider() -> ProxyProvider {
        ProxyProvider {
            id: "native".into(),
            name: "Pool Claude".into(),
            provider_type: "anthropic".into(),
            api_key: "fixture_pool_key".into(),
            base_url: "https://pool.example/anthropic".into(),
            models: vec!["pool_fixture".into()],
            enabled: true,
            timeout_ms: 60_000,
            weight: 1,
            circuit_breaker: CircuitBreakerConfig::default(),
            model_mapping: Default::default(),
            extra_headers: Default::default(),
        }
    }

    fn gateway() -> Gateway_snapshot {
        Gateway_snapshot {
            base_url: "http://127.0.0.1:54321".into(),
            auth_key: "fixture_gateway_key".into(),
            routes: vec![Gateway_route {
                alias: "gateway_alias[1M]".into(),
                protocol: ANTHROPIC_PROTOCOL.into(),
            }],
        }
    }

    fn error_code(result: Result<Resolved_upstream, AppError>) -> String {
        match result {
            Ok(_) => panic!("expected failure"),
            Err(error) => error.to_string(),
        }
    }

    #[test]
    fn native_anthropic_reuses_saved_snapshot_and_preserves_mapping() {
        let db = Database::memory().unwrap();
        db.save_provider("claude_code", &native_provider()).unwrap();
        let resolved = resolve_upstream(&db, "claude:native", "sonnet", None).unwrap();
        let profile = &resolved.llm_settings["llm-pi-ai"]["providers"]["aag"];
        assert_eq!(profile["api"], ANTHROPIC_PROTOCOL);
        assert_eq!(profile["baseURL"], "https://native.example/anthropic");
        assert_eq!(profile["apiKeyEnv"], API_KEY_ENV);
        assert_eq!(profile["models"][0]["id"], "claude_fixture");
        assert_eq!(
            resolved.llm_settings["agent-default-model"]["model"],
            "claude_fixture"
        );
        assert!(!resolved
            .llm_settings
            .to_string()
            .contains("fixture_native_key"));
        assert_eq!(
            resolved.secret_env,
            vec![(API_KEY_ENV.into(), "fixture_native_key".into())]
        );
        assert_eq!(
            db.get_provider_by_id("native", "claude_code")
                .unwrap()
                .unwrap()
                .settings_config,
            native_provider().settings_config
        );
    }

    #[test]
    fn listing_redacts_credentials_endpoints_and_distinguishes_source_ids() {
        let db = Database::memory().unwrap();
        let mut native = native_provider();
        native.name = "Native fixture_native_key https://private.example".into();
        db.save_provider("claude_code", &native).unwrap();
        db.save_proxy_provider(&pool_provider()).unwrap();
        let options = list_upstreams(&db, None).unwrap();
        let encoded = serde_json::to_string(&options).unwrap();
        for forbidden in [
            "fixture_native_key",
            "fixture_pool_key",
            "native.example",
            "pool.example",
            "private.example",
            "baseURL",
            "apiKey",
        ] {
            assert!(!encoded.contains(forbidden), "unexpected private field");
        }
        assert!(options
            .iter()
            .any(|option| option.id == "claude:native" && option.available));
        assert!(options
            .iter()
            .any(|option| option.id == "proxy:native" && option.available));
        assert!(options
            .iter()
            .any(|option| option.id == "gateway" && !option.available));
    }

    #[test]
    fn missing_or_disabled_sources_and_stale_models_fail_without_fallback() {
        let db = Database::memory().unwrap();
        let mut provider = pool_provider();
        provider.enabled = false;
        db.save_proxy_provider(&provider).unwrap();
        assert!(
            error_code(resolve_upstream(&db, "proxy:native", "pool_fixture", None))
                .contains("DSH_UPSTREAM_DISABLED")
        );
        provider.enabled = true;
        provider.api_key.clear();
        db.save_proxy_provider(&provider).unwrap();
        assert!(
            error_code(resolve_upstream(&db, "proxy:native", "pool_fixture", None))
                .contains("DSH_CREDENTIAL_UNAVAILABLE")
        );
        db.save_proxy_provider(&pool_provider()).unwrap();
        assert!(
            error_code(resolve_upstream(&db, "proxy:native", "stale", None))
                .contains("DSH_MODEL_NOT_FOUND")
        );
        assert!(
            error_code(resolve_upstream(&db, "proxy:deleted", "pool_fixture", None))
                .contains("DSH_UPSTREAM_NOT_FOUND")
        );
        assert!(error_code(resolve_upstream(
            &db,
            "https://arbitrary.example",
            "model",
            None
        ))
        .contains("DSH_SOURCE_KIND_INVALID"));
        assert!(
            error_code(resolve_upstream(&db, "proxy:native", "model\nheader", None))
                .contains("DSH_SELECTION_INVALID")
        );
    }

    #[test]
    fn native_bearer_uses_catalog_auth_without_persisting_or_reinterpreting_token() {
        let db = Database::memory().unwrap();
        let mut provider = native_provider();
        provider.settings_config["env"]["ANTHROPIC_AUTH_TOKEN"] = json!("fixture_bearer_token");
        db.save_provider("claude_code", &provider).unwrap();
        let resolved = resolve_upstream(&db, "claude:native", "sonnet", None).unwrap();
        let providers = &resolved.llm_settings["llm-pi-ai"]["providers"];
        assert!(providers.get("aag").is_none());
        assert_eq!(providers["anthropic"]["api"], ANTHROPIC_PROTOCOL);
        assert!(providers["anthropic"].get("apiKeyEnv").is_none());
        assert!(providers["anthropic"].get("headers").is_none());
        assert_eq!(
            resolved.llm_settings["agent-default-model"]["provider"],
            "anthropic"
        );
        assert_eq!(
            resolved.secret_env,
            vec![(AUTH_TOKEN_ENV.into(), "fixture_bearer_token".into())]
        );
        assert!(!resolved
            .llm_settings
            .to_string()
            .contains("fixture_bearer_token"));
        let options = list_upstreams(&db, None).unwrap();
        assert!(options[0].available);
        assert!(!serde_json::to_string(&options)
            .unwrap()
            .contains("fixture_bearer_token"));
    }

    #[test]
    fn gateway_uses_actual_runtime_snapshot_and_keeps_wire_alias() {
        let db = Database::memory().unwrap();
        db.save_route(&Route {
            alias: "new_db_alias".into(),
            description: String::new(),
            hops: vec![RouteHop {
                provider: "native".into(),
                model: "pool_fixture".into(),
            }],
        })
        .unwrap();
        let running = gateway();
        let resolved =
            resolve_upstream(&db, "gateway", "gateway_alias[1M]", Some(&running)).unwrap();
        let profile = &resolved.llm_settings["llm-pi-ai"]["providers"]["aag"];
        assert_eq!(profile["baseURL"], running.base_url);
        assert_eq!(profile["models"][0]["id"], "gateway_alias[1M]");
        assert_eq!(resolved.secret_env[0].1, "fixture_gateway_key");
        assert!(error_code(resolve_upstream(
            &db,
            "gateway",
            "new_db_alias",
            Some(&running)
        ))
        .contains("DSH_MODEL_NOT_FOUND"));
        assert!(
            error_code(resolve_upstream(&db, "gateway", "gateway_alias[1M]", None))
                .contains("DSH_GATEWAY_UNAVAILABLE")
        );
        let options = list_upstreams(&db, Some(&running)).unwrap();
        assert_eq!(options.last().unwrap().models, vec!["gateway_alias[1M]"]);
    }

    #[test]
    fn direct_source_cannot_reuse_aag_endpoint_or_persist_url_credentials() {
        let db = Database::memory().unwrap();
        let running = gateway();
        let mut provider = pool_provider();
        provider.base_url = "http://localhost:54321/v1".into();
        db.save_proxy_provider(&provider).unwrap();
        assert!(error_code(resolve_upstream(
            &db,
            "proxy:native",
            "pool_fixture",
            Some(&running)
        ))
        .contains("DSH_GATEWAY_SOURCE_REQUIRED"));
        for base_url in [
            "https://user:secret@upstream.example",
            "https://upstream.example?key=secret",
            "file:///local",
            "https://upstream.example/#secret",
        ] {
            provider.base_url = base_url.into();
            db.save_proxy_provider(&provider).unwrap();
            let error = error_code(resolve_upstream(&db, "proxy:native", "pool_fixture", None));
            assert!(error.contains("DSH_UPSTREAM_URL_INVALID"));
            assert!(!error.contains("secret"));
        }
    }

    #[test]
    fn openai_api_root_and_explicit_mapping_are_preserved() {
        let db = Database::memory().unwrap();
        let mut provider = pool_provider();
        provider.provider_type = "openai".into();
        provider.base_url = "https://pool.example/custom/v2".into();
        provider
            .model_mapping
            .insert("my_alias".into(), "actual_model".into());
        db.save_proxy_provider(&provider).unwrap();
        let resolved = resolve_upstream(&db, "proxy:native", "my_alias", None).unwrap();
        let profile = &resolved.llm_settings["llm-pi-ai"]["providers"]["aag"];
        assert_eq!(profile["api"], OPENAI_PROTOCOL);
        assert_eq!(profile["baseURL"], provider.base_url);
        assert_eq!(profile["models"][0]["id"], "actual_model");
    }

    #[test]
    fn invalid_native_types_and_unresolved_aliases_remain_unavailable() {
        let db = Database::memory().unwrap();
        let mut provider = native_provider();
        provider.settings_config["env"]["ANTHROPIC_API_KEY"] = json!(123);
        db.save_provider("claude_code", &provider).unwrap();
        assert!(
            error_code(resolve_upstream(&db, "claude:native", "sonnet", None))
                .contains("DSH_CLAUDE_CONFIG_INVALID")
        );
        let unknown = Provider::new(
            "unknown".into(),
            "Unknown catalog".into(),
            json!({
                "env": {"ANTHROPIC_API_KEY":"fixture_key", "ANTHROPIC_MODEL":"sonnet"}
            }),
        );
        db.save_provider("claude_code", &unknown).unwrap();
        assert!(
            error_code(resolve_upstream(&db, "claude:unknown", "sonnet", None))
                .contains("DSH_MODELS_UNAVAILABLE")
        );
    }
}
