//! Fixed request contracts from CC Switch's native coding_plan/balance services.
//! https://github.com/farion1231/cc-switch/blob/main/src-tauri/src/services/coding_plan.rs
//! https://github.com/farion1231/cc-switch/blob/main/src-tauri/src/services/balance.rs
//! Parsers intentionally reject missing quota facts rather than CC's UI defaults.
use super::{NativeAdapter, SourceReason};
use crate::services::provider_screening::SubscriptionState;
use serde_json::Value;

pub(super) struct NativeRequest {
    pub adapter: NativeAdapter,
    pub url: String,
    pub authorization: String,
}

pub(super) fn plan_request(
    template: &str,
    kind: Option<&str>,
    base: &str,
    key: &str,
) -> Result<NativeRequest, SourceReason> {
    if !matches!(template, "token_plan" | "balance") {
        return Err(SourceReason::UnsupportedTemplate);
    }
    if template == "token_plan" && !matches!(kind, Some("kimi" | "zhipu" | "minimax")) {
        return Err(SourceReason::UnsupportedProvider);
    }
    let url = url::Url::parse(base).map_err(|_| SourceReason::HostNotAllowed)?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some_and(|port| port != 443)
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(SourceReason::HostNotAllowed);
    }
    let host = url.host_str().ok_or(SourceReason::HostNotAllowed)?;
    let (adapter, destination, raw_auth) = match (template, kind, host) {
        ("token_plan", Some("kimi"), "api.kimi.com" | "api.kimi.ai")
            if url.path() == "/coding" || url.path().starts_with("/coding/") =>
        {
            (
                NativeAdapter::Kimi,
                format!("https://{host}/coding/v1/usages"),
                false,
            )
        }
        ("token_plan", Some("zhipu"), "open.bigmodel.cn" | "api.z.ai") => (
            NativeAdapter::Zhipu,
            format!("https://{host}/api/monitor/usage/quota/limit"),
            true,
        ),
        (
            "token_plan",
            Some("minimax"),
            "api.minimax.io" | "api.minimaxi.com" | "api.minimax.cn",
        ) => {
            let status_host = if host == "api.minimax.cn" {
                "api.minimaxi.com"
            } else {
                host
            };
            (
                NativeAdapter::MiniMax,
                format!("https://{status_host}/v1/api/openplatform/coding_plan/remains"),
                false,
            )
        }
        ("balance", _, "api.deepseek.com") => (
            NativeAdapter::DeepSeek,
            "https://api.deepseek.com/user/balance".into(),
            false,
        ),
        ("balance", _, "api.stepfun.ai" | "api.stepfun.com") => (
            NativeAdapter::StepFun,
            "https://api.stepfun.com/v1/accounts".into(),
            false,
        ),
        ("balance", _, "api.siliconflow.cn" | "api.siliconflow.com") => (
            NativeAdapter::SiliconFlow,
            format!("https://{host}/v1/user/info"),
            false,
        ),
        ("balance", _, "openrouter.ai") => (
            NativeAdapter::OpenRouter,
            "https://openrouter.ai/api/v1/credits".into(),
            false,
        ),
        ("balance", _, "api.novita.ai") => (
            NativeAdapter::Novita,
            "https://api.novita.ai/v3/user/balance".into(),
            false,
        ),
        _ => return Err(SourceReason::HostNotAllowed),
    };
    if key.trim().is_empty() {
        return Err(SourceReason::MissingCredential);
    }
    if key.chars().any(char::is_control) {
        return Err(SourceReason::InvalidSourceMetadata);
    }
    Ok(NativeRequest {
        adapter,
        url: destination,
        authorization: if raw_auth {
            key.into()
        } else {
            format!("Bearer {key}")
        },
    })
}

fn number(value: Option<&Value>) -> Option<f64> {
    value
        .and_then(|value| {
            value
                .as_f64()
                .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
        })
        .filter(|value| value.is_finite())
}
fn percentage(value: Option<&Value>) -> Option<f64> {
    number(value).filter(|value| (0.0..=100.0).contains(value))
}

pub(super) fn interpret(
    adapter: NativeAdapter,
    status: u16,
    body: &Value,
) -> (SubscriptionState, bool, SourceReason) {
    if matches!(status, 401 | 403) {
        return (
            SubscriptionState::Inactive,
            false,
            SourceReason::CredentialRejected,
        );
    }
    if status == 429 {
        return (
            SubscriptionState::Unknown,
            false,
            SourceReason::StatusRateLimited,
        );
    }
    if !(200..300).contains(&status) {
        return (
            SubscriptionState::Unknown,
            false,
            SourceReason::StatusUnavailable,
        );
    }
    if adapter == NativeAdapter::Zhipu
        && body.get("success").and_then(Value::as_bool) == Some(false)
    {
        let message = body
            .get("msg")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase();
        let compact: String = message
            .chars()
            .filter(|character| !character.is_whitespace() && !matches!(character, '_' | '-'))
            .collect();
        if compact.contains("codingplan")
            && [
                "不存在",
                "没有",
                "未开通",
                "not exist",
                "not found",
                "not have",
                "doesn't have",
                "no coding plan",
                "not subscribed",
            ]
            .iter()
            .any(|text| message.contains(text))
        {
            return (
                SubscriptionState::Inactive,
                false,
                SourceReason::NoSubscription,
            );
        }
    }
    if !body.is_object()
        || body.get("error").is_some_and(|error| !error.is_null())
        || body
            .get("success")
            .is_some_and(|success| success.as_bool() != Some(true))
    {
        return (
            SubscriptionState::Unknown,
            false,
            SourceReason::MalformedStatus,
        );
    }
    let available = match adapter {
        NativeAdapter::Kimi => kimi_available(body),
        NativeAdapter::Zhipu => zhipu_available(body),
        NativeAdapter::MiniMax => minimax_available(body),
        NativeAdapter::DeepSeek => {
            let values = body
                .get("balance_infos")
                .and_then(Value::as_array)
                .and_then(|items| {
                    items
                        .iter()
                        .map(|item| number(item.get("total_balance")))
                        .collect::<Option<Vec<_>>>()
                });
            match (body.get("is_available").and_then(Value::as_bool), values) {
                (Some(available), Some(values)) if !values.is_empty() => {
                    Some(available && values.iter().any(|value| *value > 0.0))
                }
                _ => None,
            }
        }
        NativeAdapter::StepFun => number(body.get("balance")).map(|balance| balance > 0.0),
        NativeAdapter::SiliconFlow => {
            if body
                .get("code")
                .is_some_and(|code| !matches!(code.as_i64(), Some(0 | 20000)))
            {
                None
            } else {
                number(body.pointer("/data/totalBalance")).map(|balance| balance > 0.0)
            }
        }
        NativeAdapter::OpenRouter => {
            let data = body.get("data").unwrap_or(body);
            match (
                number(data.get("total_credits")),
                number(data.get("total_usage")),
            ) {
                (Some(total), Some(used)) if total >= 0.0 && used >= 0.0 => Some(total > used),
                _ => None,
            }
        }
        NativeAdapter::Novita => number(body.get("availableBalance")).map(|balance| balance > 0.0),
    };
    match available {
        Some(true) => (
            SubscriptionState::Active,
            true,
            SourceReason::VerifiedAllowance,
        ),
        Some(false) => (
            SubscriptionState::Active,
            false,
            if matches!(
                adapter,
                NativeAdapter::Kimi | NativeAdapter::Zhipu | NativeAdapter::MiniMax
            ) {
                SourceReason::QuotaExhausted
            } else {
                SourceReason::BalanceUnavailable
            },
        ),
        None => (
            SubscriptionState::Unknown,
            false,
            SourceReason::MalformedStatus,
        ),
    }
}

fn kimi_available(body: &Value) -> Option<bool> {
    let mut limits = Vec::new();
    if let Some(usage) = body.get("usage") {
        limits.push(usage);
    }
    if let Some(items) = body.get("limits") {
        for item in items.as_array()? {
            limits.push(item.get("detail")?);
        }
    }
    if limits.is_empty() {
        return None;
    }
    let mut available = true;
    for item in limits {
        let limit = number(item.get("limit"))?;
        let remaining = number(item.get("remaining"))?;
        if limit <= 0.0 || remaining < 0.0 || remaining > limit {
            return None;
        }
        available &= remaining > 0.0;
    }
    Some(available)
}

fn zhipu_available(body: &Value) -> Option<bool> {
    if body.get("success").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let limits = body.pointer("/data/limits")?.as_array()?;
    let mut percentages = Vec::new();
    for item in limits {
        if item
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|kind| {
                kind.eq_ignore_ascii_case("TOKENS_LIMIT")
                    || kind.eq_ignore_ascii_case("CREDIT_LIMIT")
            })
        {
            percentages.push(percentage(item.get("percentage"))?);
        }
    }
    (!percentages.is_empty()).then(|| percentages.iter().all(|used| *used < 100.0))
}

fn minimax_available(body: &Value) -> Option<bool> {
    if body
        .pointer("/base_resp/status_code")
        .and_then(Value::as_i64)
        != Some(0)
    {
        return None;
    }
    let item = body
        .get("model_remains")?
        .as_array()?
        .iter()
        .find(|item| item.get("model_name").and_then(Value::as_str) == Some("general"))?;
    let interval = percentage(item.get("current_interval_remaining_percent"))?;
    let mut available = interval > 0.0;
    if let Some(status) = item.get("current_interval_status") {
        match status.as_i64()? {
            1 => {}
            2 => available = false,
            _ => return None,
        }
    }
    match item.get("current_weekly_status").and_then(Value::as_i64) {
        Some(3) => {}
        Some(1 | 2) => {
            let weekly = percentage(item.get("current_weekly_remaining_percent"))?;
            available &= weekly > 0.0 && item["current_weekly_status"] != 2;
        }
        None if item.get("current_weekly_status").is_none()
            && item.get("current_weekly_remaining_percent").is_none() => {}
        _ => return None,
    }
    Some(available)
}
