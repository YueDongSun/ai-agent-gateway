//! Authentication admission and account-safe supplier identity.
//! Secrets are compared only in memory and never returned in policy receipts.

use super::cc_switch_db::CcSwitchRow;
use super::settings_extract::{base_host_is_official_anthropic, base_host_is_official_openai};
use serde_json::Value;

pub(crate) fn excludes_account_auth(
    row: &CcSwitchRow,
    settings: &Value,
    base: &str,
    key: &str,
) -> bool {
    if row
        .meta
        .as_ref()
        .and_then(|m| m.auth_binding_kind.as_deref())
        .is_some_and(|kind| !kind.trim().is_empty() && kind != "provider_config")
    {
        return true;
    }
    if explicit_account_settings(&row.app_type, settings) {
        return true;
    }
    if key.trim().is_empty()
        && (row.category.as_deref() == Some("official")
            || base_host_is_official_anthropic(base)
            || base_host_is_official_openai(base))
    {
        return true;
    }
    let local = reqwest::Url::parse(base).ok().is_some_and(|url| {
        url.host_str().is_some_and(|host| {
            host == "localhost"
                || host == "::1"
                || host
                    .parse::<std::net::Ipv4Addr>()
                    .is_ok_and(|ip| ip.is_loopback())
        })
    });
    let known_label: String = row
        .name
        .to_ascii_lowercase()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();
    let placeholder_key = !is_static_key(key);
    local && known_label == "cliproxyapi" && placeholder_key
}

pub(super) fn is_static_key(key: &str) -> bool {
    !key.chars().any(char::is_control)
        && !matches!(
            key.trim().to_ascii_lowercase().as_str(),
            "" | "dummy" | "sk-local" | "cliproxyapi" | "not-needed" | "placeholder"
        )
}

pub(super) fn static_configuration(base: &str, key: &str) -> bool {
    is_static_key(key)
        && reqwest::Url::parse(base).is_ok_and(|url| {
            matches!(url.scheme(), "http" | "https")
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
        })
}

pub(super) fn explicit_account_settings(app_type: &str, settings: &Value) -> bool {
    if has_oauth_tokens(settings) {
        return true;
    }
    let auth = settings.get("auth").and_then(Value::as_object);
    let auth_method = settings
        .get("authMethod")
        .or_else(|| settings.get("auth_mode"))
        .or_else(|| auth.and_then(|auth| auth.get("type").or_else(|| auth.get("method"))))
        .and_then(Value::as_str);
    if auth_method.is_some_and(|method| {
        matches!(
            method.to_ascii_lowercase().as_str(),
            "oauth" | "oauth2" | "chatgpt" | "subscription" | "managed_account"
        )
    }) {
        return true;
    }
    if app_type != "codex" {
        return false;
    }
    settings
        .get("config")
        .and_then(Value::as_str)
        .and_then(|config| config.parse::<toml_edit::DocumentMut>().ok())
        .is_some_and(|doc| {
            doc.get("model_provider")
                .and_then(toml_edit::Item::as_str)
                .is_some_and(|name| {
                    name.to_ascii_lowercase()
                        .chars()
                        .filter(char::is_ascii_alphanumeric)
                        .collect::<String>()
                        == "cliproxyapi"
                })
        })
}

fn has_oauth_tokens(value: &Value) -> bool {
    match value {
        Value::Object(values) => values.iter().any(|(key, value)| {
            let key = key.to_ascii_lowercase();
            (matches!(
                key.as_str(),
                "refresh_token" | "refreshtoken" | "access_token" | "accesstoken" | "id_token"
            ) && value.as_str().is_some_and(|value| !value.is_empty()))
                || (matches!(key.as_str(), "auth" | "tokens" | "oauth" | "account")
                    && has_oauth_tokens(value))
        }),
        _ => false,
    }
}

/// A known vendor host supplies strong identity across its protocol paths.
/// Unknown vendors require the same normalized label and a matching credential.
pub(crate) fn vendor_identity(name: &str, base_url: &str) -> Option<String> {
    let url = reqwest::Url::parse(base_url).ok()?;
    let host = url.host_str()?.to_ascii_lowercase();
    let known = match host.as_str() {
        "open.bigmodel.cn" | "api.z.ai" => Some("glm"),
        "api.minimax.io" | "api.minimaxi.com" => Some("minimax"),
        "api.moonshot.cn" | "api.moonshot.ai" => Some("moonshot"),
        "api.deepseek.com" => Some("deepseek"),
        _ => None,
    };
    if let Some(known) = known {
        return Some(format!("vendor:{known}"));
    }
    let lowered = name.to_lowercase();
    let words: Vec<_> = lowered
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();
    let channels = [
        "claude",
        "codex",
        "anthropic",
        "response",
        "responses",
        "openai",
    ];
    let trimmed: Vec<_> = words
        .iter()
        .copied()
        .filter(|word| !channels.contains(word))
        .collect();
    let label = if !trimmed.is_empty() {
        trimmed.join("")
    } else {
        words.join("")
    };
    if label.is_empty() {
        return None;
    }
    Some(format!("label:{label}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::import_cc_switch::cc_switch_db::CcSwitchRow;
    fn row(name: &str) -> CcSwitchRow {
        CcSwitchRow {
            id: "fixture".into(),
            app_type: "codex".into(),
            name: name.into(),
            settings_config: "{}".into(),
            website_url: None,
            category: None,
            created_at: None,
            sort_index: None,
            notes: None,
            icon: None,
            icon_color: None,
            is_current: false,
            in_failover_queue: false,
            meta: None,
            meta_json: "{}".into(),
            meta_issue: None,
            endpoint_urls: vec![],
            endpoints_capped: false,
        }
    }
    #[test]
    fn declared_cliproxy_account_is_excluded_but_generic_local_api_is_not() {
        let settings = serde_json::json!({"config":"model_provider = \"cliproxyapi\"\n"});
        assert!(excludes_account_auth(
            &row("Account"),
            &settings,
            "http://127.0.0.1:8317",
            "proxy-key"
        ));
        assert!(!excludes_account_auth(
            &row("Local API"),
            &serde_json::json!({}),
            "http://127.0.0.1:8000",
            ""
        ));
        assert!(!excludes_account_auth(
            &row("cliProxyAPI"),
            &serde_json::json!({}),
            "https://api.example.invalid",
            "actual-static-key"
        ));
    }
    #[test]
    fn explicit_oauth_tokens_are_excluded_even_if_a_key_is_also_present() {
        assert!(excludes_account_auth(
            &row("Account"),
            &serde_json::json!({"auth":{"refresh_token":"fixture_refresh"}}),
            "https://api.example.invalid",
            "fixture-key"
        ));
    }
}
