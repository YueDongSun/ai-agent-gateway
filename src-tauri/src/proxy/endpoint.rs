//! Shared upstream base-URL normalization.
//!
//! Every consumer of a provider's configured `base_url` — the proxy providers
//! (`anthropic.rs` / `openai.rs`), the model-catalog probe
//! (`commands/proxy.rs::proxy_fetch_models`), and the DSH upstream exporter
//! (`services/dsh/upstreams.rs::endpoint_for_dsh`) — must agree on how a base
//! URL becomes a concrete request URL. Before this module each site re-derived
//! the path with its own string suffix check, which doubled the path (and
//! 404'd) whenever a base was stored as a full endpoint such as
//! `.../apps/anthropic/v1/messages`.
//!
//! The rule: a base may be (a) an API root (`https://host`),
//! (b) a versioned root (`https://host/v1`), or (c) a full endpoint
//! (`https://host/v1/messages`). Detection is anchored on parsed path
//! segments, not raw string suffixes, so a root that merely ends in a segment
//! named `messages` is not mistaken for a full endpoint unless it is the
//! protocol's terminal leaf. Query strings are preserved (Azure-style
//! `?api-version=` full endpoints); fragments are dropped.

use url::Url;

/// The upstream wire protocol a base URL serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    /// Anthropic Messages API — leaf `/messages` under `/v1`.
    Anthropic,
    /// OpenAI Chat Completions API — leaf `/chat/completions`.
    Openai,
    /// OpenAI Responses API — leaf `/responses`.
    Responses,
    /// Google Generative Language API — `models/{model}:generateContent`
    /// method calls under `/v1beta` (or `/v1`). Unlike the other two, the
    /// request URL embeds the model id, so URL building happens per request
    /// (see [`gemini_method_url`]).
    Gemini,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(non_camel_case_types)]
pub struct Endpoint_error(pub String);

impl std::fmt::Display for Endpoint_error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid base url: {}", self.0)
    }
}
impl std::error::Error for Endpoint_error {}

fn invalid(reason: &str) -> Endpoint_error {
    Endpoint_error(reason.to_string())
}

/// Trim trailing `/` and split the path into non-empty segments.
fn path_segments(url: &Url) -> Vec<&str> {
    url.path()
        .trim_end_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect()
}

/// A version segment like `v1` / `v2` / `v10` (lowercase `v` + digits).
fn is_version_segment(segment: &str) -> bool {
    let rest = match segment.strip_prefix('v') {
        Some(rest) => rest,
        None => return false,
    };
    !rest.is_empty() && rest.bytes().all(|byte| byte.is_ascii_digit())
}

impl Protocol {
    /// The terminal path segments that mark a stored base as a full endpoint.
    /// Anthropic: `.../messages`. OpenAI: `.../chat/completions`.
    fn endpoint_tail(self) -> &'static [&'static str] {
        match self {
            Protocol::Anthropic => &["messages"],
            Protocol::Openai => &["chat", "completions"],
            Protocol::Responses => &["responses"],
            // Gemini request URLs embed the model id — see `gemini_method_url`.
            Protocol::Gemini => &[],
        }
    }
    /// The path appended to an API root (no version segment present).
    fn full_path(self) -> &'static str {
        match self {
            Protocol::Anthropic => "/v1/messages",
            Protocol::Openai => "/v1/chat/completions",
            Protocol::Responses => "/v1/responses",
            Protocol::Gemini => "/v1beta/models",
        }
    }
    /// The leaf appended to a versioned root (`.../v1`).
    fn leaf(self) -> &'static str {
        match self {
            Protocol::Anthropic => "/messages",
            Protocol::Openai => "/chat/completions",
            Protocol::Responses => "/responses",
            Protocol::Gemini => "/models",
        }
    }
}

fn ends_with_segments(segments: &[&str], tail: &[&str]) -> bool {
    segments.len() >= tail.len() && segments[segments.len() - tail.len()..] == *tail
}

/// Parse `base_url`, rejecting anything that is not an absolute http(s) URL
/// with a host and no embedded credentials.
fn parse_base(base_url: &str) -> Result<Url, Endpoint_error> {
    let trimmed = base_url.trim();
    if trimmed.is_empty() || trimmed.chars().any(char::is_control) {
        return Err(invalid("empty_or_control"));
    }
    let url = Url::parse(trimmed).map_err(|_| invalid("parse"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host().is_none() {
        return Err(invalid("scheme_or_host"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(invalid("credentials"));
    }
    Ok(url)
}

/// Serialize a URL preserving its query but dropping any fragment, with no
/// trailing slash left on the path.
fn render(mut url: Url) -> String {
    url.set_fragment(None);
    let mut out = url.as_str().to_string();
    // `url` keeps a trailing `/` on the path when the path is emptied; strip
    // only path-trailing slashes that precede the query.
    if let Some(query_at) = out.find('?') {
        let (path_part, query_part) = out.split_at(query_at);
        out = format!("{}{}", path_part.trim_end_matches('/'), query_part);
    } else {
        out = out.trim_end_matches('/').to_string();
    }
    out
}

/// Resolve the concrete request endpoint for a provider base URL.
///
/// - Full endpoint (path already ends in the protocol tail) → used as-is,
///   query preserved.
/// - Versioned root (`.../v1`) → append the leaf only.
/// - Bare root → append the full versioned path.
pub fn endpoint_url(base_url: &str, protocol: Protocol) -> Result<String, Endpoint_error> {
    if protocol == Protocol::Gemini {
        // Gemini request URLs embed the model id — there is no fixed
        // endpoint. Use `gemini_method_url` per request instead.
        return Err(invalid("gemini_needs_method_url"));
    }
    let mut url = parse_base(base_url)?;
    let segments = path_segments(&url);
    if ends_with_segments(&segments, protocol.endpoint_tail()) {
        // Already a full endpoint. Preserve query; drop fragment.
        return Ok(render(url));
    }
    let base_path = url.path().trim_end_matches('/');
    let appended = if segments.last().is_some_and(|s| is_version_segment(s)) {
        format!("{}{}", base_path, protocol.leaf())
    } else {
        format!("{}{}", base_path, protocol.full_path())
    };
    url.set_path(&appended);
    Ok(render(url))
}

/// Derive the Anthropic `count_tokens` endpoint from an already-resolved
/// Messages endpoint: the terminal `messages` leaf becomes
/// `messages/count_tokens`, preserving any query string. Used by the
/// `/v1/messages/count_tokens` local-forwarding surface.
pub fn anthropic_count_tokens_url(messages_endpoint: &str) -> Result<String, Endpoint_error> {
    let mut url = parse_base(messages_endpoint)?;
    let mut segments = path_segments(&url);
    if segments.last() != Some(&"messages") {
        return Err(invalid("not_a_messages_endpoint"));
    }
    segments.push("count_tokens");
    url.set_path(&format!("/{}", segments.join("/")));
    Ok(render(url))
}

/// Reduce a Gemini base URL to its version root: the origin plus an optional
/// `/v1beta` (or `/v1` / `/v1alpha`) prefix. Stored bases may be bare roots
/// (`https://generativelanguage.googleapis.com`), versioned roots
/// (`.../v1beta`), `/models` lists, or full method endpoints
/// (`.../v1beta/models/gemini-2.5-pro:generateContent`) — everything from the
/// first method-call or `models` segment on is dropped, so the method URL
/// never doubles the path. Mirrors cc-switch `gemini_url.rs`.
fn gemini_api_root(base_url: &str) -> Result<String, Endpoint_error> {
    let url = parse_base(base_url)?;
    let mut segments = path_segments(&url);
    // Drop everything from the first `models` or method-call segment on.
    if let Some(position) = segments
        .iter()
        .position(|segment| *segment == "models" || segment.contains(':'))
    {
        segments.truncate(position);
    }
    let mut out = render_prefix(&url);
    if !segments.is_empty() {
        out.push('/');
        out.push_str(&segments.join("/"));
    }
    Ok(out)
}

/// Is the trailing path segment a Gemini version marker?
fn is_gemini_version_root(root: &str) -> bool {
    root.rsplit('/')
        .next()
        .is_some_and(|segment| segment == "v1beta" || segment == "v1" || segment == "v1alpha")
}

/// Resolve the concrete Gemini method URL for one request:
/// `{root}/[v1beta/]models/{model}:{method}`. The model id is normalized
/// (leading `models/` stripped) and a root without a version segment gains
/// the canonical `/v1beta`.
pub fn gemini_method_url(
    base_url: &str,
    model: &str,
    method: &str,
) -> Result<String, Endpoint_error> {
    gemini_method_target(base_url, model, method).map(|(url, _)| url)
}

/// Metadata comes from the same normalized value embedded in the actual URL.
pub fn gemini_method_target(
    base_url: &str,
    model: &str,
    method: &str,
) -> Result<(String, String), Endpoint_error> {
    let root = gemini_api_root(base_url)?;
    let model = crate::proxy::gemini::normalize_gemini_model_id(model);
    if model.is_empty() {
        return Err(invalid("empty_model"));
    }
    if is_gemini_version_root(&root) {
        Ok((format!("{root}/models/{model}:{method}"), model.to_owned()))
    } else {
        Ok((format!("{root}/v1beta/models/{model}:{method}"), model.to_owned()))
    }
}

/// Resolve the canonical API *root* for SDKs that append the endpoint path
/// themselves (pi-ai's Anthropic SDK appends `/v1/messages`; its
/// openai-completions provider appends `/chat/completions`).
///
/// Strict: rejects query/fragment (SDK base URLs must be clean roots). Strips
/// a full-endpoint tail, then for Anthropic also strips a trailing version
/// segment, so the SDK's own append lands exactly once.
pub fn sdk_base_url(base_url: &str, protocol: Protocol) -> Result<String, Endpoint_error> {
    if protocol == Protocol::Gemini {
        // Gemini bases are already roots by contract; nothing to strip.
        return Ok(base_url.trim_end_matches('/').to_string());
    }
    let mut url = parse_base(base_url)?;
    if url.query().is_some() || url.fragment().is_some() {
        return Err(invalid("query_or_fragment"));
    }
    let mut segments: Vec<String> = path_segments(&url).iter().map(|s| s.to_string()).collect();
    if protocol == Protocol::Gemini {
        if let Some(position) = segments
            .iter()
            .position(|segment| segment == "models" || segment.contains(':'))
        {
            segments.truncate(position);
        }
    } else if ends_with_segments(
        &segments.iter().map(String::as_str).collect::<Vec<_>>(),
        protocol.endpoint_tail(),
    ) {
        let tail_len = protocol.endpoint_tail().len();
        segments.truncate(segments.len() - tail_len);
    }
    if protocol == Protocol::Anthropic && segments.last().is_some_and(|s| is_version_segment(s)) {
        segments.pop();
    }
    let path = format!("/{}", segments.join("/"));
    url.set_path(path.trim_end_matches('/'));
    Ok(render(url))
}

/// Ordered, deduplicated candidate URLs for the model-catalog probe
/// (`GET .../models`). Full-endpoint bases are reduced to their root before
/// the `/models` candidates are built, so a stored `.../v1/messages` base
/// probes `.../v1/models`, not `.../v1/messages/v1/models`.
pub fn models_urls(base_url: &str, protocol: Protocol) -> Vec<String> {
    let Ok(url) = parse_base(base_url) else {
        return Vec::new();
    };
    let mut segments: Vec<String> = path_segments(&url).iter().map(|s| s.to_string()).collect();
    if protocol == Protocol::Gemini {
        if let Some(position) = segments.iter().position(|segment| segment == "models" || segment.contains(':')) {
            segments.truncate(position);
        }
    } else if ends_with_segments(
        &segments.iter().map(String::as_str).collect::<Vec<_>>(),
        protocol.endpoint_tail(),
    ) {
        let tail_len = protocol.endpoint_tail().len();
        segments.truncate(segments.len() - tail_len);
    }
    let has_version = segments.last().is_some_and(|segment| {
        is_version_segment(segment)
            || (protocol == Protocol::Gemini && matches!(segment.as_str(), "v1beta" | "v1alpha"))
    });
    let prefix = render_prefix(&url);
    let prefix = prefix.trim_end_matches('/');
    let root = segments.join("/");
    let root = root.trim_end_matches('/');
    let join = |path: &str| {
        if root.is_empty() {
            format!("{prefix}{path}")
        } else {
            format!("{prefix}/{root}{path}")
        }
    };
    let mut out: Vec<String> = Vec::new();
    let mut push = |candidate: String| {
        if !out.contains(&candidate) {
            out.push(candidate);
        }
    };
    if has_version {
        // Versioned root: probe {root}/models, then the root minus the version
        // segment + /models as a fallback.
        push(join("/models"));
        let unversioned = segments[..segments.len() - 1].join("/");
        let unversioned = unversioned.trim_end_matches('/');
        let candidate = if unversioned.is_empty() {
            format!("{prefix}/models")
        } else {
            format!("{prefix}/{unversioned}/models")
        };
        push(candidate);
    } else {
        // Bare root: probe {root}/v1beta/models (Gemini) or {root}/v1/models
        // first, then the remaining versioned/plain candidates.
        if protocol == Protocol::Gemini {
            push(join("/v1beta/models"));
        }
        push(join("/v1/models"));
        push(join("/models"));
    }
    out
}

fn render_prefix(url: &Url) -> String {
    let mut prefix = url.clone();
    prefix.set_path("");
    prefix.set_query(None);
    prefix.set_fragment(None);
    prefix.as_str().trim_end_matches('/').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_catalog_urls_normalize_responses_and_gemini_endpoints() {
        assert_eq!(
            models_urls("https://example.invalid/api/v1/responses", Protocol::Responses),
            ["https://example.invalid/api/v1/models", "https://example.invalid/api/models"]
        );
        for suffix in ["/v1beta", "/v1beta/models", "/v1beta/models/gemini-pro:generateContent"] {
            assert_eq!(
                models_urls(&format!("https://example.invalid{suffix}"), Protocol::Gemini),
                ["https://example.invalid/v1beta/models", "https://example.invalid/models"]
            );
        }
    }

    #[test]
    fn anthropic_count_tokens_url_derives_from_every_endpoint_spelling() {
        for (messages_endpoint, expected) in [
            (
                "https://api.anthropic.com/v1/messages",
                "https://api.anthropic.com/v1/messages/count_tokens",
            ),
            (
                "https://relay.example.invalid/api/anthropic/v1/messages",
                "https://relay.example.invalid/api/anthropic/v1/messages/count_tokens",
            ),
            (
                "https://azure.example.invalid/anthropic/v1/messages?api-version=2023-06-01",
                "https://azure.example.invalid/anthropic/v1/messages/count_tokens?api-version=2023-06-01",
            ),
        ] {
            assert_eq!(
                anthropic_count_tokens_url(messages_endpoint).as_deref(),
                Ok(expected)
            );
        }
        // Only a resolved Messages endpoint is accepted — anything else
        // (a bare root, an OpenAI leaf) is refused instead of guessed.
        assert!(anthropic_count_tokens_url("https://api.anthropic.com").is_err());
        assert!(anthropic_count_tokens_url("https://api.anthropic.com/v1").is_err());
        assert!(anthropic_count_tokens_url("https://host/v1/chat/completions").is_err());
    }

    #[test]
    fn anthropic_endpoint_never_doubles_messages() {
        let cases = [
            (
                "https://api.minimaxi.com/anthropic",
                "https://api.minimaxi.com/anthropic/v1/messages",
            ),
            (
                "https://open.bigmodel.cn/api/anthropic/",
                "https://open.bigmodel.cn/api/anthropic/v1/messages",
            ),
            (
                "http://127.0.0.1:11434/v1/",
                "http://127.0.0.1:11434/v1/messages",
            ),
            (
                "http://127.0.0.1:11434/v1",
                "http://127.0.0.1:11434/v1/messages",
            ),
            (
                "https://token-plan.cn-beijing.maas.aliyuncs.com/apps/anthropic/v1/messages",
                "https://token-plan.cn-beijing.maas.aliyuncs.com/apps/anthropic/v1/messages",
            ),
            (
                "https://host.test/apps/anthropic/v1/messages/",
                "https://host.test/apps/anthropic/v1/messages",
            ),
            // Query-carrying full endpoint: query preserved, not doubled.
            (
                "https://host.test/apps/anthropic/v1/messages?tenant=abc",
                "https://host.test/apps/anthropic/v1/messages?tenant=abc",
            ),
            // Bare root gets the versioned path.
            (
                "https://api.anthropic.com",
                "https://api.anthropic.com/v1/messages",
            ),
        ];
        for (base, want) in cases {
            assert_eq!(
                endpoint_url(base, Protocol::Anthropic).unwrap(),
                want,
                "base={base}"
            );
        }
    }

    #[test]
    fn openai_endpoint_never_doubles_and_supplies_v1() {
        let cases = [
            ("https://api.openai.com/v1", "https://api.openai.com/v1/chat/completions"),
            ("https://api.deepseek.com/v1/", "https://api.deepseek.com/v1/chat/completions"),
            (
                "https://host.test/v1/chat/completions",
                "https://host.test/v1/chat/completions",
            ),
            (
                "https://host.test/v1/chat/completions/",
                "https://host.test/v1/chat/completions",
            ),
            // Bare root gains /v1 (parity with Anthropic).
            ("https://api.openai.com", "https://api.openai.com/v1/chat/completions"),
            // Azure-style full endpoint with a mandatory query.
            (
                "https://res.openai.azure.com/openai/deployments/gpt4/chat/completions?api-version=2024-08-01",
                "https://res.openai.azure.com/openai/deployments/gpt4/chat/completions?api-version=2024-08-01",
            ),
        ];
        for (base, want) in cases {
            assert_eq!(
                endpoint_url(base, Protocol::Openai).unwrap(),
                want,
                "base={base}"
            );
        }
    }

    #[test]
    fn responses_endpoint_never_doubles_and_supplies_v1() {
        for (base, expected) in [
            (
                "https://api.openai.com/v1",
                "https://api.openai.com/v1/responses",
            ),
            (
                "https://host.test/v1/responses/",
                "https://host.test/v1/responses",
            ),
            (
                "https://host.test/apps/openai/v1/responses?tenant=abc",
                "https://host.test/apps/openai/v1/responses?tenant=abc",
            ),
            ("https://api.openai.com", "https://api.openai.com/v1/responses"),
        ] {
            assert_eq!(
                endpoint_url(base, Protocol::Responses).unwrap(),
                expected,
                "base={base}"
            );
        }
    }

    #[test]
    fn sdk_base_strips_full_endpoint_for_anthropic() {
        // pi-ai appends /v1/messages itself; a stored full endpoint must be
        // reduced to the API root.
        assert_eq!(
            sdk_base_url(
                "https://token-plan.cn-beijing.maas.aliyuncs.com/apps/anthropic/v1/messages",
                Protocol::Anthropic
            )
            .unwrap(),
            "https://token-plan.cn-beijing.maas.aliyuncs.com/apps/anthropic"
        );
        // A versioned root also reduces to the bare root for Anthropic.
        assert_eq!(
            sdk_base_url(
                "https://open.bigmodel.cn/api/anthropic/v1",
                Protocol::Anthropic
            )
            .unwrap(),
            "https://open.bigmodel.cn/api/anthropic"
        );
        // Plain root is unchanged.
        assert_eq!(
            sdk_base_url("https://api.minimaxi.com/anthropic", Protocol::Anthropic).unwrap(),
            "https://api.minimaxi.com/anthropic"
        );
        // Query/fragment rejected for SDK roots.
        assert!(sdk_base_url("https://h.test/v1/messages?x=1", Protocol::Anthropic).is_err());
    }

    #[test]
    fn models_urls_reduce_full_endpoint_to_root() {
        let urls = models_urls(
            "https://token-plan.cn-beijing.maas.aliyuncs.com/apps/anthropic/v1/messages",
            Protocol::Anthropic,
        );
        assert_eq!(
            urls,
            vec![
                "https://token-plan.cn-beijing.maas.aliyuncs.com/apps/anthropic/v1/models"
                    .to_string(),
                "https://token-plan.cn-beijing.maas.aliyuncs.com/apps/anthropic/models".to_string(),
            ]
        );
        // Plain versioned root: probe {root}/models, then the versionless root.
        let urls = models_urls("https://api.openai.com/v1", Protocol::Openai);
        assert_eq!(
            urls,
            vec![
                "https://api.openai.com/v1/models".to_string(),
                "https://api.openai.com/models".to_string(),
            ]
        );
        // Bare root yields a /v1 candidate and a plain candidate.
        let urls = models_urls("https://api.openai.com", Protocol::Openai);
        assert_eq!(
            urls,
            vec![
                "https://api.openai.com/v1/models".to_string(),
                "https://api.openai.com/models".to_string(),
            ]
        );
    }

    #[test]
    fn invalid_bases_are_rejected() {
        assert!(endpoint_url("", Protocol::Anthropic).is_err());
        assert!(endpoint_url("not a url", Protocol::Anthropic).is_err());
        assert!(endpoint_url("ftp://h.test", Protocol::Anthropic).is_err());
        assert!(endpoint_url("https://user:pw@h.test", Protocol::Anthropic).is_err());
        assert!(models_urls("not a url", Protocol::Anthropic).is_empty());
    }
}
