//! Narrow adapter for the public registry's versioned JSON, plus cache validation.
use super::{
    failure, Market_catalog, Market_entry, Market_privacy_scrub, MAX_CATALOG_BYTES, MAX_ENTRIES,
    MAX_FEED_BYTES, SOURCE_REGISTRY,
};
use crate::error::AppError;
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashSet},
    net::{Ipv4Addr, Ipv6Addr},
    sync::OnceLock,
};

const PRIVACY_FIELDS: [&str; 4] = ["description", "description_en", "description_zh", "license"];
const PRIVACY_REPLACEMENTS: [(&str, &str); 4] = [
    ("authenticated_url", "[redacted_authenticated_url]"),
    ("email", "[redacted_email]"),
    ("user_path", "[redacted_user_path]"),
    ("private_network", "[redacted_private_network]"),
];

#[derive(Deserialize)]
struct Rule {
    id: String,
    keywords: Vec<String>,
}
#[derive(Deserialize)]
struct Rules {
    version: String,
    modules: Vec<Rule>,
    categories: Vec<Rule>,
    use_cases: Vec<Rule>,
}
static RULES: OnceLock<Rules> = OnceLock::new();
fn rules() -> &'static Rules {
    RULES.get_or_init(|| {
        serde_json::from_str(include_str!(
            "../../../../scripts/dsh_market/classification_rules.json"
        ))
        .expect("checked-in marketplace rules must parse")
    })
}

pub(super) fn valid_sha(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn valid_full_name(value: &str) -> bool {
    let parts: Vec<_> = value.split('/').collect();
    parts.len() == 2
        && !parts[0].is_empty()
        && parts[0].len() <= 39
        && parts[0].as_bytes()[0].is_ascii_alphanumeric()
        && parts[0]
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        && !parts[1].is_empty()
        && parts[1].len() <= 100
        && ![".", ".."].contains(&parts[1])
        && parts[1]
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
}
fn valid_package(value: &str) -> bool {
    let identifier = |part: &str| {
        !part.is_empty()
            && (part.as_bytes()[0].is_ascii_lowercase() || part.as_bytes()[0].is_ascii_digit())
            && part.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
            })
    };
    if let Some(scoped) = value.strip_prefix('@') {
        let parts: Vec<_> = scoped.split('/').collect();
        parts.len() == 2 && identifier(parts[0]) && identifier(parts[1])
    } else {
        identifier(value)
    }
}
fn valid_version(value: &str) -> bool {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b".-+".contains(&byte))
    {
        return false;
    }
    let base = value.split(['-', '+']).next().unwrap_or("");
    let parts: Vec<_> = base.split('.').collect();
    if parts.len() != 3
        || !parts.iter().all(|part| {
            !part.is_empty()
                && part.bytes().all(|byte| byte.is_ascii_digit())
                && (part.len() == 1 || !part.starts_with('0'))
        })
    {
        return false;
    }
    let suffix = &value[base.len()..];
    if suffix.is_empty() {
        return true;
    }
    let valid_suffix = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b".-".contains(&byte))
    };
    if let Some(build) = suffix.strip_prefix('+') {
        return valid_suffix(build);
    }
    if let Some(pre) = suffix.strip_prefix('-') {
        let mut parts = pre.split('+');
        valid_suffix(parts.next().unwrap_or(""))
            && parts.next().map(valid_suffix).unwrap_or(true)
            && parts.next().is_none()
    } else {
        false
    }
}
fn valid_text(value: &str, limit: usize) -> bool {
    value.chars().count() <= limit
        && !value
            .chars()
            .any(|character| character.is_control() && !['\n', '\r', '\t'].contains(&character))
}
fn text<'a>(value: &'a Value, field: &str, limit: usize) -> Result<&'a str, AppError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| valid_text(value, limit))
        .ok_or_else(|| failure("invalid_text"))
}
fn text_any<'a>(value: &'a Value, fields: &[&str], limit: usize) -> Result<&'a str, AppError> {
    fields
        .iter()
        .find_map(|field| value.get(*field).map(|_| *field))
        .ok_or_else(|| failure("invalid_text"))
        .and_then(|field| text(value, field, limit))
}
fn optional_text(value: &Value, field: &str, limit: usize) -> Result<Option<String>, AppError> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(None),
        _ => Ok(Some(text(value, field, limit)?.into())),
    }
}
fn optional_text_any(
    value: &Value,
    fields: &[&str],
    limit: usize,
) -> Result<Option<String>, AppError> {
    for field in fields {
        if value.get(*field).is_some_and(|item| !item.is_null()) {
            return optional_text(value, field, limit);
        }
    }
    Ok(None)
}
fn valid_timestamp(value: &str) -> bool {
    chrono::DateTime::parse_from_rfc3339(value).is_ok()
}
fn timestamp<'a>(value: &'a Value, field: &str) -> Result<&'a str, AppError> {
    let result = text(value, field, 64)?;
    if !valid_timestamp(result) {
        return Err(failure("invalid_timestamp"));
    }
    Ok(result)
}
fn excerpt(value: &str) -> String {
    value.chars().take(400).collect()
}

/// Site readme excerpts can end at a high UTF-16 surrogate. JavaScript JSON
/// parsers replace that unmatched code unit, while serde_json correctly rejects
/// it. Match the site's display behavior for malformed surrogate escapes only.
pub(super) fn replace_unpaired_surrogates(bytes: &[u8]) -> Vec<u8> {
    let mut result = Vec::with_capacity(bytes.len());
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes[cursor] != b'\\' {
            result.push(bytes[cursor]);
            cursor += 1;
            continue;
        }
        let run_start = cursor;
        while cursor < bytes.len() && bytes[cursor] == b'\\' {
            cursor += 1;
        }
        let slash_count = cursor - run_start;
        let has_unicode_escape = slash_count % 2 == 1
            && bytes.get(cursor) == Some(&b'u')
            && cursor + 5 < bytes.len()
            && bytes[cursor + 1..cursor + 5]
                .iter()
                .all(u8::is_ascii_hexdigit);
        if !has_unicode_escape {
            result.extend_from_slice(&bytes[run_start..cursor]);
            if cursor < bytes.len() {
                result.push(bytes[cursor]);
                cursor += 1;
            }
            continue;
        }
        result.extend_from_slice(&bytes[run_start..cursor - 1]);
        let escape_start = cursor - 1;
        let code_unit = u16::from_str_radix(
            std::str::from_utf8(&bytes[cursor + 1..cursor + 5]).unwrap_or(""),
            16,
        )
        .unwrap_or(0);
        if (0xd800..=0xdbff).contains(&code_unit) {
            let low_start = cursor + 5;
            let low_is_valid = bytes.get(low_start..low_start + 6).is_some_and(|escape| {
                escape[0..2] == *b"\\u"
                    && escape[2..6].iter().all(u8::is_ascii_hexdigit)
                    && u16::from_str_radix(std::str::from_utf8(&escape[2..6]).unwrap_or(""), 16)
                        .is_ok_and(|value| (0xdc00..=0xdfff).contains(&value))
            });
            if low_is_valid {
                result.extend_from_slice(&bytes[escape_start..low_start + 6]);
                cursor = low_start + 6;
                continue;
            }
        } else if !(0xdc00..=0xdfff).contains(&code_unit) {
            result.extend_from_slice(&bytes[escape_start..cursor + 5]);
            cursor += 5;
            continue;
        }
        result.extend_from_slice(b"\\uFFFD");
        cursor += 5;
    }
    result
}

fn is_private_ipv4(address: Ipv4Addr) -> bool {
    let octets = address.octets();
    address.is_private() || (octets[0] == 100 && (64..=127).contains(&octets[1]))
}

fn is_private_ipv6(address: Ipv6Addr) -> bool {
    if let Some(mapped) = address.to_ipv4_mapped() {
        return is_private_ipv4(mapped);
    }
    let first = address.segments()[0];
    first & 0xfe00 == 0xfc00 || first & 0xffc0 == 0xfe80
}

struct Privacy_patterns {
    authenticated_url: regex::Regex,
    email: regex::Regex,
    windows_user_path: regex::Regex,
    posix_user_path: regex::Regex,
    tailnet_name: regex::Regex,
    ipv6: regex::Regex,
    ipv4: regex::Regex,
}

fn privacy_patterns() -> &'static Privacy_patterns {
    static PATTERNS: OnceLock<Privacy_patterns> = OnceLock::new();
    PATTERNS.get_or_init(|| Privacy_patterns {
        authenticated_url: regex::Regex::new(r#"(?i)\bhttps?://[^\s/@:]+:[^\s/@]+@[^\s\"'<>]+"#)
            .expect("static authenticated URL regex"),
        email: regex::Regex::new(r"(?i)\b[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}\b")
            .expect("static email regex"),
        windows_user_path: regex::Regex::new(
            r#"(?i)\b[A-Z]:[\\/]Users[\\/][^\\/\s<>\"']+[\\/][^\s<>\"']+"#,
        )
        .expect("static Windows path regex"),
        posix_user_path: regex::Regex::new(r#"/(?:Users|home)/[^/\s<>\"']+/[^\s<>\"']+"#)
            .expect("static POSIX path regex"),
        tailnet_name: regex::Regex::new(r"(?i)\b(?:[a-z0-9-]+\.)+ts\.net\b")
            .expect("static Tailnet regex"),
        ipv6: regex::Regex::new(r"(?i)[0-9a-f:.]*:[0-9a-f:.]+")
            .expect("static IPv6 candidate regex"),
        ipv4: regex::Regex::new(r"\b\d{1,3}(?:\.\d{1,3}){3}\b").expect("static IPv4 regex"),
    })
}

fn replacement(kind: &str) -> &'static str {
    PRIVACY_REPLACEMENTS
        .iter()
        .find_map(|(key, value)| (*key == kind).then_some(*value))
        .expect("privacy replacement keys are static")
}

fn replace_private_matches<F>(
    value: String,
    pattern: &regex::Regex,
    kind: &str,
    counts: &mut BTreeMap<String, usize>,
    predicate: F,
) -> String
where
    F: Fn(&str) -> bool,
{
    let mut replaced = 0usize;
    let result = pattern
        .replace_all(&value, |captures: &regex::Captures<'_>| {
            let matched = captures.get(0).expect("full regex match").as_str();
            if predicate(matched) {
                replaced += 1;
                replacement(kind).to_string()
            } else {
                matched.to_string()
            }
        })
        .into_owned();
    *counts.get_mut(kind).expect("privacy counter key") += replaced;
    result
}

fn scrub_privacy_text(value: &str) -> (String, BTreeMap<String, usize>) {
    let mut counts = BTreeMap::from(PRIVACY_REPLACEMENTS.map(|(key, _)| (key.into(), 0usize)));
    let patterns = privacy_patterns();
    let mut output = value.to_string();
    output = replace_private_matches(
        output,
        &patterns.authenticated_url,
        "authenticated_url",
        &mut counts,
        |_| true,
    );
    output = replace_private_matches(output, &patterns.email, "email", &mut counts, |_| true);
    output = replace_private_matches(
        output,
        &patterns.windows_user_path,
        "user_path",
        &mut counts,
        |_| true,
    );
    output = replace_private_matches(
        output,
        &patterns.posix_user_path,
        "user_path",
        &mut counts,
        |_| true,
    );
    output = replace_private_matches(
        output,
        &patterns.tailnet_name,
        "private_network",
        &mut counts,
        |_| true,
    );
    output = replace_private_matches(
        output,
        &patterns.ipv6,
        "private_network",
        &mut counts,
        |candidate| {
            candidate
                .trim_matches('.')
                .parse::<Ipv6Addr>()
                .ok()
                .is_some_and(is_private_ipv6)
        },
    );
    output = replace_private_matches(
        output,
        &patterns.ipv4,
        "private_network",
        &mut counts,
        |candidate| {
            candidate
                .parse::<Ipv4Addr>()
                .ok()
                .is_some_and(is_private_ipv4)
        },
    );
    (output, counts)
}
fn has_chinese(value: &str) -> bool {
    value
        .chars()
        .any(|character| ('\u{3400}'..='\u{9fff}').contains(&character))
}
fn matches(value: &str, keyword: &str) -> bool {
    if keyword.len() <= 3
        && keyword
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    {
        value
            .split(|character: char| !character.is_ascii_lowercase() && !character.is_ascii_digit())
            .any(|token| token == keyword)
    } else {
        value.contains(keyword)
    }
}
fn classify(
    full_name: &str,
    package: Option<&str>,
    description: &str,
    topics: &[String],
) -> (Vec<String>, Vec<String>, Vec<String>) {
    let identity = format!("{full_name} {}", package.unwrap_or("")).to_lowercase();
    let topics = topics.join(" ").to_lowercase();
    let description = description.to_lowercase();
    let mut scored: Vec<_> = rules()
        .categories
        .iter()
        .enumerate()
        .map(|(index, rule)| {
            let score = usize::from(rule.keywords.iter().any(|word| matches(&topics, word))) * 5
                + usize::from(rule.keywords.iter().any(|word| matches(&identity, word))) * 3
                + usize::from(rule.keywords.iter().any(|word| matches(&description, word)));
            (score, index, rule.id.clone())
        })
        .filter(|row| row.0 > 0)
        .collect();
    scored.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    let mut categories: Vec<String> = scored.into_iter().take(3).map(|row| row.2).collect();
    if categories.is_empty() {
        categories.push("other".into());
    }
    let combined = format!("{identity} {topics} {description}");
    let mut use_cases: Vec<String> = rules()
        .use_cases
        .iter()
        .filter(|rule| rule.keywords.iter().any(|word| matches(&combined, word)))
        .map(|rule| rule.id.clone())
        .collect();
    if use_cases.is_empty() {
        use_cases.push("general".into());
    }
    let mut modules: Vec<String> = rules()
        .modules
        .iter()
        .filter(|rule| rule.keywords.iter().any(|word| matches(&combined, word)))
        .map(|rule| rule.id.clone())
        .collect();
    if modules.is_empty() {
        modules.push("general".into());
    }
    (categories, use_cases, modules)
}
fn expected_install_spec(entry: &Market_entry) -> Option<String> {
    match entry.install_method.as_str() {
        "npm" => match (&entry.package_name, &entry.version) {
            (Some(package), Some(version)) if valid_package(package) && valid_version(version) => {
                Some(format!("{package}@{version}"))
            }
            _ => None,
        },
        "github" => entry
            .source_commit
            .as_ref()
            .map(|commit| format!("github:{}#{commit}", entry.full_name)),
        _ => None,
    }
}

#[cfg(test)]
pub(super) fn normalize_feed(
    bytes: &[u8],
    registry_commit: &str,
    fetched_at: &str,
) -> Result<Market_catalog, AppError> {
    normalize_feed_source(bytes, Some(registry_commit), fetched_at, false)
}

pub(super) fn normalize_site_feed(
    bytes: &[u8],
    fetched_at: &str,
) -> Result<Market_catalog, AppError> {
    normalize_feed_source(bytes, None, fetched_at, true)
}

fn normalize_feed_source(
    bytes: &[u8],
    registry_commit: Option<&str>,
    fetched_at: &str,
    is_site_feed: bool,
) -> Result<Market_catalog, AppError> {
    if bytes.len() > MAX_FEED_BYTES {
        return Err(failure("feed_too_large"));
    }
    let normalized_bytes = replace_unpaired_surrogates(bytes);
    let feed: Value =
        serde_json::from_slice(&normalized_bytes).map_err(|_| failure("invalid_json"))?;
    if !matches!(
        feed.get("schemaVersion").and_then(Value::as_u64),
        Some(1 | 2)
    ) {
        return Err(failure("invalid_feed"));
    }
    let raw_entries = feed
        .get("plugins")
        .and_then(Value::as_array)
        .ok_or_else(|| failure("invalid_feed"))?;
    if raw_entries.is_empty() || raw_entries.len() > MAX_ENTRIES {
        return Err(failure("invalid_entry_count"));
    }
    if registry_commit.is_some_and(|commit| !valid_sha(commit)) || !valid_timestamp(fetched_at) {
        return Err(failure("invalid_provenance"));
    }
    let registry_generated_at = timestamp(&feed, "generatedAt")?.into();
    let mut seen = HashSet::new();
    let mut entries = Vec::with_capacity(raw_entries.len());
    let mut privacy_replacements =
        BTreeMap::from(PRIVACY_REPLACEMENTS.map(|(key, _)| (key.to_string(), 0usize)));
    let mut privacy_modified_entries = 0usize;
    for raw in raw_entries {
        let full_name = text_any(raw, &["fullName", "full_name", "repo"], 140)?;
        if !valid_full_name(full_name) {
            return Err(failure("invalid_repository"));
        }
        let id = full_name.to_ascii_lowercase();
        if !seen.insert(id.clone()) {
            return Err(failure("duplicate_entry"));
        }
        let repo_url = format!("https://github.com/{full_name}");
        if raw.get("htmlUrl").or_else(|| raw.get("html_url")).is_some()
            && !text_any(raw, &["htmlUrl", "html_url"], 256)?.eq_ignore_ascii_case(&repo_url)
        {
            return Err(failure("invalid_repository_url"));
        }
        let (description, description_counts) = scrub_privacy_text(&excerpt(
            &optional_text(raw, "description", 16000)?.unwrap_or_default(),
        ));
        let package_name = optional_text_any(raw, &["packageName", "package_name"], 214)?;
        let version =
            optional_text_any(raw, &["version", "packageVersion", "package_version"], 128)?;
        if package_name
            .as_deref()
            .is_some_and(|value| !valid_package(value))
        {
            return Err(failure("invalid_package"));
        }
        if version
            .as_deref()
            .is_some_and(|value| scrub_privacy_text(value).0 != value)
        {
            return Err(failure("invalid_version_privacy"));
        }
        let source_commit = optional_text_any(
            raw,
            &[
                "verifiedCommit",
                "verified_commit",
                "sourceCommit",
                "source_commit",
            ],
            40,
        )?;
        if source_commit
            .as_ref()
            .map(|commit| !valid_sha(commit))
            .unwrap_or(false)
        {
            return Err(failure("invalid_commit"));
        }
        let topics = match raw.get("topics") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(values)) if values.len() <= 100 => values
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .filter(|value| valid_text(value, 128))
                        .map(String::from)
                        .ok_or_else(|| failure("invalid_topics"))
                })
                .collect::<Result<Vec<_>, _>>()?,
            _ => return Err(failure("invalid_topics")),
        };
        let stars = match raw.get("stars") {
            None => 0,
            Some(value) => value
                .as_u64()
                .filter(|value| *value <= 9_007_199_254_740_991)
                .ok_or_else(|| failure("invalid_stars"))?,
        };
        let has_client = match raw.get("hasClient").or_else(|| raw.get("has_client")) {
            None => false,
            Some(value) => value
                .as_bool()
                .ok_or_else(|| failure("invalid_client_flag"))?,
        };
        let install_source = raw
            .pointer("/install/source")
            .and_then(Value::as_str)
            .unwrap_or("");
        let install_method = if ["npm", "github", "manual"].contains(&install_source) {
            install_source
        } else if raw.pointer("/install/mode").and_then(Value::as_str) == Some("manual") {
            "manual"
        } else {
            "unknown"
        }
        .to_string();
        let description_en_source = optional_text_any(
            raw,
            &[
                "descriptionEn",
                "description_en",
                "descriptionEnglish",
                "description_english",
            ],
            16000,
        )?
        .map(|value| excerpt(&value));
        let (description_en, description_en_counts) = match description_en_source {
            Some(value) => {
                let (value, counts) = scrub_privacy_text(&value);
                (Some(value), counts)
            }
            None if !description.is_empty() && !has_chinese(&description) => (
                Some(description.clone()),
                BTreeMap::from(PRIVACY_REPLACEMENTS.map(|(key, _)| (key.into(), 0usize))),
            ),
            None => (
                None,
                BTreeMap::from(PRIVACY_REPLACEMENTS.map(|(key, _)| (key.into(), 0usize))),
            ),
        };
        let description_zh_source = optional_text_any(
            raw,
            &[
                "descriptionZh",
                "description_zh",
                "descriptionChinese",
                "description_chinese",
            ],
            16000,
        )?
        .map(|value| excerpt(&value));
        let (description_zh, description_zh_counts) = match description_zh_source {
            Some(value) => {
                let (value, counts) = scrub_privacy_text(&value);
                (Some(value), counts)
            }
            None if has_chinese(&description) => (
                Some(description.clone()),
                BTreeMap::from(PRIVACY_REPLACEMENTS.map(|(key, _)| (key.into(), 0usize))),
            ),
            None => (
                None,
                BTreeMap::from(PRIVACY_REPLACEMENTS.map(|(key, _)| (key.into(), 0usize))),
            ),
        };
        let (license, license_counts) = match optional_text(raw, "license", 256)? {
            Some(value) => {
                let (value, counts) = scrub_privacy_text(&value);
                (Some(value), counts)
            }
            None => (
                None,
                BTreeMap::from(PRIVACY_REPLACEMENTS.map(|(key, _)| (key.into(), 0usize))),
            ),
        };
        let mut entry_modified = false;
        for counts in [
            &description_counts,
            &description_en_counts,
            &description_zh_counts,
            &license_counts,
        ] {
            for (key, count) in counts {
                if *count > 0 {
                    entry_modified = true;
                    *privacy_replacements
                        .get_mut(key)
                        .expect("privacy keys match") += count;
                }
            }
        }
        if entry_modified {
            privacy_modified_entries += 1;
        }
        let updated_at = optional_text_any(raw, &["updatedAt", "updated_at"], 64)?;
        if updated_at
            .as_ref()
            .map(|value| !valid_timestamp(value))
            .unwrap_or(false)
        {
            return Err(failure("invalid_timestamp"));
        }
        let classification_description = format!(
            "{} {} {}",
            description,
            description_en.as_deref().unwrap_or_default(),
            description_zh.as_deref().unwrap_or_default()
        );
        let (categories, use_cases, modules) = classify(
            full_name,
            package_name.as_deref(),
            &classification_description,
            &topics,
        );
        let (author, name) = full_name
            .split_once('/')
            .ok_or_else(|| failure("invalid_repository"))?;
        let mut entry = Market_entry {
            id,
            name: name.into(),
            full_name: full_name.into(),
            description,
            description_en,
            description_zh,
            repo_url: repo_url.clone(),
            source_commit_url: source_commit
                .as_ref()
                .map(|commit| format!("{repo_url}/tree/{commit}")),
            source_commit,
            package_name,
            version,
            install_method,
            install_spec: None,
            categories,
            use_cases,
            modules,
            classification_basis: rules().version.clone(),
            author: author.into(),
            license,
            stars,
            updated_at,
            has_client,
            verification: "not_audited".into(),
        };
        entry.install_spec = expected_install_spec(&entry);
        entries.push(entry);
    }
    entries.sort_by(|left, right| left.id.cmp(&right.id));
    let catalog = Market_catalog {
        schema_version: 1,
        fetched_at: fetched_at.into(),
        registry_generated_at,
        source_registry: if is_site_feed {
            "dsh.market"
        } else {
            SOURCE_REGISTRY
        }
        .into(),
        registry_commit: registry_commit.unwrap_or_default().into(),
        registry_url: if is_site_feed {
            "https://dsh.market/plugins.json".into()
        } else {
            format!(
                "https://github.com/{SOURCE_REGISTRY}/blob/{}/registry/plugins.json",
                registry_commit.unwrap()
            )
        },
        collector_url: if is_site_feed {
            String::new()
        } else {
            format!(
                "https://github.com/{SOURCE_REGISTRY}/blob/{}/scripts/registry-scan.mjs",
                registry_commit.unwrap()
            )
        },
        entry_count: entries.len(),
        privacy_scrub: Market_privacy_scrub {
            schema_version: 1,
            fields: PRIVACY_FIELDS.map(String::from).into(),
            replacement_tokens: BTreeMap::from(
                PRIVACY_REPLACEMENTS.map(|(key, value)| (key.into(), value.into())),
            ),
            source_entry_count: raw_entries.len(),
            modified_entries: privacy_modified_entries,
            total_replacements: privacy_replacements.values().sum(),
            replacements: privacy_replacements,
        },
        entries,
        cache_status: "refreshed".into(),
        warning: None,
    };
    validate_catalog(&catalog)?;
    Ok(catalog)
}

pub(super) fn parse_catalog(bytes: &[u8]) -> Result<Market_catalog, AppError> {
    if bytes.len() > MAX_CATALOG_BYTES {
        return Err(failure("catalog_too_large"));
    }
    let catalog: Market_catalog =
        serde_json::from_slice(bytes).map_err(|_| failure("invalid_catalog"))?;
    validate_catalog(&catalog)?;
    Ok(catalog)
}
pub(super) fn validate_catalog(catalog: &Market_catalog) -> Result<(), AppError> {
    if catalog.schema_version != 1
        || catalog.entries.is_empty()
        || catalog.entries.len() > MAX_ENTRIES
        || catalog.entry_count != catalog.entries.len()
    {
        return Err(failure("invalid_entry_count"));
    }
    let expected_tokens = BTreeMap::from(
        PRIVACY_REPLACEMENTS.map(|(key, value)| (key.to_string(), value.to_string())),
    );
    let expected_keys: HashSet<_> = expected_tokens.keys().cloned().collect();
    if catalog.privacy_scrub.schema_version != 1
        || catalog.privacy_scrub.fields != PRIVACY_FIELDS.map(String::from)
        || catalog.privacy_scrub.replacement_tokens != expected_tokens
        || catalog.privacy_scrub.source_entry_count != catalog.entries.len()
        || catalog.privacy_scrub.modified_entries > catalog.entries.len()
        || catalog
            .privacy_scrub
            .replacements
            .keys()
            .cloned()
            .collect::<HashSet<_>>()
            != expected_keys
        || catalog.privacy_scrub.total_replacements
            != catalog.privacy_scrub.replacements.values().sum::<usize>()
    {
        return Err(failure("invalid_privacy_scrub"));
    }
    if ![SOURCE_REGISTRY, "dsh.market"].contains(&catalog.source_registry.as_str())
        || (catalog.source_registry == SOURCE_REGISTRY && !valid_sha(&catalog.registry_commit))
        || (catalog.source_registry == "dsh.market"
            && (!catalog.registry_commit.is_empty()
                || catalog.registry_url != "https://dsh.market/plugins.json"
                || !catalog.collector_url.is_empty()))
        || !valid_timestamp(&catalog.fetched_at)
        || !valid_timestamp(&catalog.registry_generated_at)
        || (catalog.source_registry == SOURCE_REGISTRY
            && catalog.registry_url
                != format!(
                    "https://github.com/{SOURCE_REGISTRY}/blob/{}/registry/plugins.json",
                    catalog.registry_commit
                ))
        || (catalog.source_registry == SOURCE_REGISTRY
            && catalog.collector_url
                != format!(
                    "https://github.com/{SOURCE_REGISTRY}/blob/{}/scripts/registry-scan.mjs",
                    catalog.registry_commit
                ))
        || !["cached", "refreshed"].contains(&catalog.cache_status.as_str())
    {
        return Err(failure("invalid_provenance"));
    }
    let mut seen = HashSet::new();
    for entry in &catalog.entries {
        if !valid_full_name(&entry.full_name)
            || entry.id != entry.full_name.to_ascii_lowercase()
            || !seen.insert(&entry.id)
        {
            return Err(failure("invalid_repository"));
        }
        let (author, name) = entry.full_name.split_once('/').unwrap();
        if entry.author != author
            || entry.name != name
            || entry.repo_url != format!("https://github.com/{}", entry.full_name)
            || entry
                .source_commit
                .as_ref()
                .map(|value| !valid_sha(value))
                .unwrap_or(false)
            || entry.source_commit_url
                != entry
                    .source_commit
                    .as_ref()
                    .map(|commit| format!("{}/tree/{commit}", entry.repo_url))
        {
            return Err(failure("invalid_repository_url"));
        }
        if !valid_text(&entry.description, 400)
            || [&entry.description_en, &entry.description_zh]
                .iter()
                .any(|value| {
                    value
                        .as_ref()
                        .map(|text| !valid_text(text, 400))
                        .unwrap_or(false)
                })
            || entry
                .package_name
                .as_ref()
                .map(|text| !valid_text(text, 214) || !valid_package(text))
                .unwrap_or(false)
            || entry
                .version
                .as_ref()
                .map(|text| !valid_text(text, 128) || scrub_privacy_text(text).0 != *text)
                .unwrap_or(false)
            || entry
                .license
                .as_ref()
                .map(|text| !valid_text(text, 256))
                .unwrap_or(false)
            || entry
                .updated_at
                .as_ref()
                .map(|value| !valid_timestamp(value))
                .unwrap_or(false)
            || entry.stars > 9_007_199_254_740_991
        {
            return Err(failure("invalid_text"));
        }
        if scrub_privacy_text(&entry.description).0 != entry.description
            || [&entry.description_en, &entry.description_zh]
                .iter()
                .any(|value| {
                    value
                        .as_ref()
                        .map(|text| scrub_privacy_text(text).0 != *text)
                        .unwrap_or(false)
                })
            || entry
                .license
                .as_ref()
                .map(|text| scrub_privacy_text(text).0 != *text)
                .unwrap_or(false)
        {
            return Err(failure("catalog_privacy_unscrubbed"));
        }
        if !["npm", "github", "manual", "unknown"].contains(&entry.install_method.as_str())
            || entry.install_spec != expected_install_spec(entry)
        {
            return Err(failure("invalid_install_spec"));
        }
        let valid_ids = |values: &[String], allowed: &[Rule], fallback: &str, limit: usize| {
            !values.is_empty()
                && values.len() <= limit
                && values.iter().collect::<HashSet<_>>().len() == values.len()
                && values
                    .iter()
                    .all(|value| value == fallback || allowed.iter().any(|rule| rule.id == *value))
        };
        if !valid_ids(&entry.categories, &rules().categories, "other", 3)
            || !valid_ids(&entry.use_cases, &rules().use_cases, "general", 12)
            || !valid_ids(&entry.modules, &rules().modules, "general", 8)
            || entry.classification_basis != rules().version
            || entry.verification != "not_audited"
        {
            return Err(failure("invalid_classification"));
        }
    }
    Ok(())
}
