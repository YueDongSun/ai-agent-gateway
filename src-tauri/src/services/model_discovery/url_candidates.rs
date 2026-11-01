//! Bounded, same-origin API metadata search and read-only route validation.
//! Catalog availability and route metadata never claim successful model inference.
use super::fetch::{discovery_headers, fetch_models_ids, FetchError, UpstreamModel};
use crate::database::dao::proxy_providers::ProxyProvider;
use crate::database::dao::suppliers::{legacy_provider_type, SUPPORTED_PROTOCOLS};
use crate::proxy::providers::AnthropicAuthScheme;
use reqwest::{header::ALLOW, Client, Url};
use serde_json::Value;
use std::collections::BTreeSet;
use std::time::Duration;

const SEARCH_TIMEOUT_SECS: u64 = 5;
const MAX_DOCUMENT_BYTES: usize = 512 * 1024;
const MAX_DOCUMENT_PATHS: usize = 512;
const MAX_CANDIDATES: usize = 16;
const NEGATIVE_CONTROL_PATH: &str = "__aag_missing_url_discovery_route__";

pub(super) struct UrlCandidate {
    pub protocol: &'static str,
    pub url: String,
}

#[derive(Default)]
pub(super) struct UrlSearch {
    pub candidates: Vec<UrlCandidate>,
    pub failures: Vec<(usize, String)>,
}

fn valid_url(url: &Url, source: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
        && url.origin() == source.origin()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && url.as_str().len() <= 2048
}

pub(super) fn parse_candidates(
    document: &Value,
    document_url: &Url,
) -> Result<Vec<UrlCandidate>, String> {
    if !document
        .get("openapi")
        .and_then(Value::as_str)
        .is_some_and(|version| version.starts_with("3."))
    {
        return Err("openapi_document_invalid".into());
    }
    let paths = document
        .get("paths")
        .and_then(Value::as_object)
        .ok_or("openapi_paths_missing")?;
    if paths.len() > MAX_DOCUMENT_PATHS {
        return Err("openapi_paths_limit".into());
    }
    let default_servers = serde_json::json!([{"url":"/"}]);
    let mut candidates = Vec::new();
    let mut seen = BTreeSet::new();
    for (path, item) in paths {
        let Some(operation) = item.get("post").filter(|operation| operation.is_object()) else {
            continue;
        };
        if !path.starts_with('/') || path.starts_with("//") || path.contains(['?', '#', '\\']) {
            continue;
        }
        let (protocol, path) = if path.ends_with("/chat/completions") {
            ("openai-chat", path.as_str())
        } else if path.ends_with("/responses") {
            ("openai-responses", path.as_str())
        } else if path.ends_with("/messages") {
            ("anthropic", path.as_str())
        } else if let Some(prefix) = path.strip_suffix(":generateContent") {
            let Some((root, parameter)) = prefix.rsplit_once("/models/") else {
                continue;
            };
            if parameter.len() < 3
                || !parameter.starts_with('{')
                || !parameter.ends_with('}')
                || parameter[1..parameter.len() - 1].contains(['{', '}', '/'])
            {
                continue;
            }
            ("gemini", root)
        } else {
            continue;
        };
        if path.contains(['{', '}']) {
            continue;
        }
        // OpenAPI server precedence is operation -> path -> document. Never
        // follow a remote $ref or contact a server outside the configured origin.
        let servers = operation
            .get("servers")
            .or_else(|| item.get("servers"))
            .or_else(|| document.get("servers"))
            .unwrap_or(&default_servers);
        let Some(servers) = servers.as_array() else {
            continue;
        };
        for server in servers.iter().take(MAX_CANDIDATES) {
            let Some(server_url) = server.get("url").and_then(Value::as_str) else {
                continue;
            };
            if server_url.contains(['{', '}']) {
                continue;
            }
            let Ok(mut url) = document_url.join(server_url) else {
                continue;
            };
            if !valid_url(&url, document_url) {
                continue;
            }
            url.set_path(&format!("{}{}", url.path().trim_end_matches('/'), path));
            if !valid_url(&url, document_url) {
                continue;
            }
            let rendered = url.to_string().trim_end_matches('/').to_string();
            if seen.insert((protocol, rendered.clone())) {
                if candidates.len() >= MAX_CANDIDATES {
                    return Err("openapi_candidates_limit".into());
                }
                candidates.push(UrlCandidate {
                    protocol,
                    url: rendered,
                });
            }
        }
    }
    Ok(candidates)
}

pub(super) async fn search(
    source: &ProxyProvider,
    auth_scheme: AnthropicAuthScheme,
) -> Result<UrlSearch, String> {
    tokio::time::timeout(Duration::from_secs(SEARCH_TIMEOUT_SECS), async {
        let source_url = Url::parse(&source.base_url).map_err(|_| "supplier_url_invalid")?;
        if !valid_url(&source_url, &source_url) {
            return Err("supplier_url_discovery_unsupported".into());
        }
        let client = Client::builder()
            .timeout(Duration::from_secs(SEARCH_TIMEOUT_SECS))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| "url_http_client_failed")?;
        let headers = discovery_headers(source, auth_scheme).map_err(|error| error.to_string())?;
        let protocol = match source.provider_type.as_str() {
            "anthropic" => crate::proxy::endpoint::Protocol::Anthropic,
            "gemini" => crate::proxy::endpoint::Protocol::Gemini,
            "openai-responses" => crate::proxy::endpoint::Protocol::Responses,
            _ => crate::proxy::endpoint::Protocol::Openai,
        };
        let mut documents = Vec::new();
        if let Some(catalog) =
            crate::proxy::endpoint::models_urls(&source.base_url, protocol).first()
        {
            if let Ok(mut url) = Url::parse(catalog) {
                if let Some(root) = url.path().strip_suffix("/models") {
                    url.set_path(&format!("{root}/openapi.json"));
                    documents.push(url.to_string());
                }
            }
        }
        let origin_document = source_url
            .join("/openapi.json")
            .map_err(|_| "supplier_url_invalid")?
            .to_string();
        if !documents.contains(&origin_document) {
            documents.push(origin_document);
        }
        let mut outcome = UrlSearch::default();
        let mut seen = BTreeSet::new();
        let mut remaining_bytes = MAX_DOCUMENT_BYTES;
        for (index, document) in documents.into_iter().enumerate() {
            let document_url = Url::parse(&document).map_err(|_| "supplier_url_invalid")?;
            let result = read_openapi(&client, &document_url, &headers, &mut remaining_bytes)
                .await
                .and_then(|document| parse_candidates(&document, &document_url));
            match result {
                Ok(candidates) => {
                    for candidate in candidates {
                        if seen.insert((candidate.protocol, candidate.url.clone())) {
                            if outcome.candidates.len() == MAX_CANDIDATES {
                                return Err("openapi_candidates_limit".into());
                            }
                            outcome.candidates.push(candidate);
                        }
                    }
                }
                Err(error) => outcome.failures.push((index + 1, error)),
            }
        }
        Ok(outcome)
    })
    .await
    .map_err(|_| "url_search_timeout".to_string())?
}

async fn read_openapi(
    client: &Client,
    url: &Url,
    headers: &reqwest::header::HeaderMap,
    remaining_bytes: &mut usize,
) -> Result<Value, String> {
    let mut response = client
        .get(url.clone())
        .headers(headers.clone())
        .send()
        .await
        .map_err(transport_error)?;
    if !response.status().is_success() {
        return Err(format!("openapi_http_{}", response.status().as_u16()));
    }
    if response
        .content_length()
        .is_some_and(|size| size > *remaining_bytes as u64)
    {
        return Err("openapi_size_limit".into());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
        if chunk.len() > *remaining_bytes {
            return Err("openapi_size_limit".into());
        }
        *remaining_bytes -= chunk.len();
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| "openapi_document_invalid".into())
}

fn transport_error(error: reqwest::Error) -> String {
    if error.is_timeout() {
        "url_request_timeout".into()
    } else {
        "url_transport_failed".into()
    }
}

fn advertises_post(response: &reqwest::Response) -> bool {
    response.status() == reqwest::StatusCode::METHOD_NOT_ALLOWED
        && response
            .headers()
            .get_all(ALLOW)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .any(|value| {
                value
                    .split(',')
                    .any(|method| method.trim().eq_ignore_ascii_case("POST"))
            })
}

pub(super) async fn validate(
    source: &ProxyProvider,
    candidate: &UrlCandidate,
) -> Result<(ProxyProvider, Vec<UpstreamModel>), String> {
    if !SUPPORTED_PROTOCOLS.contains(&candidate.protocol) {
        return Err("supplier_protocol_unsupported".into());
    }
    if candidate.protocol == "gemini" {
        // The parser retained the exact prefix before /models/{model}. Ensure
        // runtime normalization would not insert a different version segment.
        let declared = format!(
            "{}/models/aag-discovery-route:generateContent",
            candidate.url
        );
        let runtime = crate::proxy::endpoint::gemini_method_url(
            &candidate.url,
            "aag-discovery-route",
            "generateContent",
        )
        .map_err(|_| "supplier_url_invalid")?;
        if runtime != declared {
            return Err("url_validation_runtime_path_mismatch".into());
        }
    }
    let mut provider = source.clone();
    provider.provider_type = legacy_provider_type(candidate.protocol)
        .ok_or("supplier_protocol_unsupported")?
        .into();
    provider.base_url = candidate.url.clone();
    let headers = discovery_headers(&provider, AnthropicAuthScheme::ApiKey)
        .map_err(|error| error.to_string())?;
    let client = Client::builder()
        .timeout(Duration::from_secs(SEARCH_TIMEOUT_SECS))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "url_http_client_failed")?;
    let models = if candidate.protocol == "gemini" {
        Some(
            fetch_models_ids(&provider, AnthropicAuthScheme::ApiKey)
                .await
                .map_err(|error| error.to_string())?,
        )
    } else {
        None
    };
    let probe_url = if let Some(models) = &models {
        if models.iter().any(|model| {
            let slot = crate::proxy::gemini::normalize_gemini_model_id(&model.id);
            slot.is_empty()
                || matches!(slot, "." | "..")
                || slot.contains(['/', '\\', '?', '#', '%'])
        }) {
            return Err("url_validation_model_path_invalid".into());
        }
        let model = models.first().ok_or("url_validation_empty_catalog")?;
        let runtime =
            crate::proxy::endpoint::gemini_method_url(&candidate.url, &model.id, "generateContent")
                .map_err(|_| "supplier_url_invalid")?;
        let mut declared = Url::parse(&format!("{}/models/", candidate.url))
            .map_err(|_| "supplier_url_invalid")?;
        declared
            .path_segments_mut()
            .map_err(|_| "supplier_url_invalid")?
            .pop_if_empty()
            .push(&format!(
                "{}:generateContent",
                crate::proxy::gemini::normalize_gemini_model_id(&model.id)
            ));
        if Url::parse(&runtime).ok().as_ref() != Some(&declared) {
            return Err("url_validation_model_path_invalid".into());
        }
        runtime
    } else {
        candidate.url.clone()
    };
    let response = client
        .get(&probe_url)
        .headers(headers.clone())
        .send()
        .await
        .map_err(transport_error)?;
    if !advertises_post(&response) {
        return Err(format!(
            "url_validation_http_{}",
            response.status().as_u16()
        ));
    }
    // A global or API-prefix 405/Allow handler is not route-existence evidence.
    let control = Url::parse(&probe_url)
        .map_err(|_| "supplier_url_invalid")?
        .join(NEGATIVE_CONTROL_PATH)
        .map_err(|_| "supplier_url_invalid")?;
    let response = client
        .get(control)
        .headers(headers)
        .send()
        .await
        .map_err(transport_error)?;
    if response.status() != reqwest::StatusCode::NOT_FOUND {
        return Err("url_validation_route_unproven".into());
    }
    let models = match models {
        Some(models) => models,
        None => fetch_models_ids(&provider, AnthropicAuthScheme::ApiKey)
            .await
            .map_err(|error: FetchError| error.to_string())?,
    };
    Ok((provider, models))
}
