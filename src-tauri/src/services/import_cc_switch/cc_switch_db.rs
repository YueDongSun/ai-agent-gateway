//! Read-only access to the external cc-switch SQLite database.
//!
//! Everything in this module touches only cc-switch's own database file
//! (opened `SQLITE_OPEN_READ_ONLY`); it never reads or writes AAG state.
//! The row struct and app_type mapping live here because they define the
//! the external schema contract the reader depends on.
//!
//! The reader is strict about the real cc-switch schema:
//! - `providers.meta` is its own JSON column (not part of
//!   `settings_config`); `meta.apiFormat` and `meta.authBinding` are
//!   parsed from there.
//! - `provider_endpoints` carries the per-provider endpoint URL list and
//!   is read when the table exists.
//! - Source size bounds (row count, per-row blob sizes) are enforced here
//!   so a hostile or corrupt file is refused before it is parsed.

use crate::error::AppError;
use rusqlite::{Connection, OpenFlags};
use std::path::PathBuf;

/// Maximum number of `providers` rows accepted from one source file.
pub(crate) const MAX_SOURCE_ROWS: usize = 2_000;
/// Maximum size of one row's `settings_config` text accepted for parsing.
pub(crate) const MAX_SETTINGS_CONFIG_BYTES: usize = 512 * 1024;
/// Maximum size of one row's `meta` text accepted for parsing.
pub(crate) const MAX_META_BYTES: usize = 256 * 1024;
/// Maximum number of `provider_endpoints` URLs carried per provider row.
pub(crate) const MAX_ENDPOINT_URLS_PER_ROW: usize = 32;

/// Resolve the cc-switch DB path under the user's home directory.
/// Honours `AAG_HOME_DIR` (for tests) and `CC_SWITCH_DB_PATH` (for users
/// who installed cc-switch to a non-default location).
pub fn cc_switch_db_path() -> PathBuf {
    if let Ok(custom) = std::env::var("CC_SWITCH_DB_PATH") {
        let trimmed = custom.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    crate::config::get_home_dir()
        .join(".cc-switch")
        .join("cc-switch.db")
}

/// Map cc-switch's `app_type` values onto AAG's. cc-switch uses short
/// hyphenated ids (`claude`, `claude-desktop`, `grokbuild`); AAG uses
/// snake_case (`claude_code`, `claude_desktop`, `grok_build`). Values
/// that already match pass through unchanged; unknown values pass
/// through as-is so the row still lands (switching it may fail with
/// "unknown app_type", but the data isn't lost).
pub(crate) fn map_app_type(cc_switch_app_type: &str) -> String {
    match cc_switch_app_type {
        "claude" => "claude_code".into(),
        "claude-desktop" => "claude_desktop".into(),
        "grokbuild" => "grok_build".into(),
        other => other.into(),
    }
}

/// Map one cc-switch `icon` value onto AAG's icon vocabulary.
///
/// cc-switch stores keys from its extracted brand-icon library
/// (`anthropic`, `openai`, `deepseek`, ...); AAG's `icon` field is a
/// designed glyph id or a single literal character. Known brand keys are
/// translated to the matching AAG family glyph (case-insensitive),
/// literal characters (emoji, a typed letter) pass through verbatim, and
/// unknown library keys are dropped so the tile falls back to a name
/// monogram instead of rendering a multi-character string as "text".
/// Rows without an icon map to `None` — the monogram fallback.
pub(crate) fn map_cc_switch_icon(icon: Option<&str>) -> Option<String> {
    let value = icon?.trim();
    if value.is_empty() {
        return None;
    }
    if let Some(glyph) = cc_switch_icon_glyph(value) {
        return Some(glyph.to_string());
    }
    // Icon-library keys are multi-character ASCII identifiers; anything
    // else (emoji, a typed letter) is already valid AAG icon vocabulary.
    let is_library_key = value.chars().count() > 1
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if is_library_key {
        None
    } else {
        Some(value.to_string())
    }
}

/// Trim and empty-normalise a cc-switch `icon_color` value. The color is
/// display-only and passes through as stored (hex strings from cc-switch
/// are valid CSS colors already).
pub(crate) fn map_cc_switch_icon_color(icon_color: Option<&str>) -> Option<String> {
    let value = icon_color?.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

/// The AAG family glyph for a known cc-switch brand-icon key. Exact
/// (case-insensitive) key match only — the stored value is a picker
/// selection, not a fuzzy name. Families AAG has no glyph for (Google,
/// Azure, ...) intentionally return `None` and monogram instead.
fn cc_switch_icon_glyph(icon_key: &str) -> Option<&'static str> {
    let key = icon_key.to_ascii_lowercase();
    Some(match key.as_str() {
        "anthropic" | "claude" | "claudeapi" | "claudecn" => "spark",
        "openai" | "chatgpt" => "knot",
        "deepseek" => "whale",
        "kimi" | "moonshot" => "moon",
        "zhipu" | "zhipuai" | "glm" | "zai" | "bigmodel" => "nodes",
        "minimax" => "waves",
        "baidu" | "qianfan" => "sail",
        "alibaba" | "qwen" | "dashscope" | "tongyi" => "gem",
        "tencent" | "hunyuan" => "cloud",
        "huawei" | "huaweicloud" => "petals",
        "huoshan" | "volcengine" | "volcano" | "bytedance" | "doubao" => "volcano",
        "xai" | "grok" => "cross",
        "stepfun" => "steps",
        "xiaomi" | "mimo" => "chip",
        "ollama" => "llama",
        "openrouter" => "branch",
        "aihubmix" => "flask",
        "pipellm" => "pipe",
        "302ai" => "robot",
        "cherryin" => "cherry",
        "opencode" | "opencodezen" => "lotus",
        "opencodego" => "motion",
        "commandcode" => "keyboard",
        "yylx" => "fish",
        "omlx" => "apple",
        "mlx" | "mlxserve" => "gear",
        _ => return None,
    })
}

/// Non-secret metadata parsed from a row's `meta` JSON column. Values are
/// kept raw (unknown tokens included) so callers can explain them instead
/// of silently guessing.
pub(crate) struct CcSwitchMeta {
    /// `meta.apiFormat` when it is a non-empty string. `None` means the
    /// row does not declare an API format (or the declaration is invalid —
    /// see `meta_issue`).
    pub(crate) api_format: Option<String>,
    /// `meta.authBinding.source`: `provider_config`, `managed_account`,
    /// or any unknown raw token. `None` when absent.
    pub(crate) auth_binding_kind: Option<String>,
    /// `meta.apiKeyField` (e.g. `ANTHROPIC_AUTH_TOKEN`) when present.
    pub(crate) api_key_field: Option<String>,
    /// `meta.isFullUrl`: the base URL is a complete endpoint.
    pub(crate) is_full_url: bool,
}

/// Internal: rows as read from cc-switch's DB. `app_type` already
/// mapped onto AAG's snake_case. The row never carries credentials
/// outside `settings_config`/`auth` JSON, which callers parse strictly.
pub(crate) struct CcSwitchRow {
    pub(crate) id: String,
    pub(crate) app_type: String,
    pub(crate) name: String,
    pub(crate) settings_config: String,
    pub(crate) website_url: Option<String>,
    pub(crate) category: Option<String>,
    pub(crate) created_at: Option<i64>,
    pub(crate) sort_index: Option<usize>,
    pub(crate) notes: Option<String>,
    pub(crate) icon: Option<String>,
    pub(crate) icon_color: Option<String>,
    pub(crate) is_current: bool,
    pub(crate) in_failover_queue: bool,
    /// Parsed from the standalone `meta` JSON column.
    pub(crate) meta: Option<CcSwitchMeta>,
    /// Raw `meta` column text, kept for source fingerprinting only.
    pub(crate) meta_json: String,
    /// Stable code when the `meta` column could not be trusted:
    /// `meta_invalid` (not a JSON object) or `meta_api_format_invalid`.
    pub(crate) meta_issue: Option<&'static str>,
    /// Endpoint URLs from `provider_endpoints`, in cc-switch order.
    pub(crate) endpoint_urls: Vec<String>,
    /// True when the row's endpoint URL list was capped at
    /// [`MAX_ENDPOINT_URLS_PER_ROW`].
    pub(crate) endpoints_capped: bool,
}

/// Read every row from the cc-switch `providers` table plus its endpoint
/// list. Pure I/O — no AAG DB touched. Returns an empty Vec if the DB
/// doesn't exist.
pub(crate) fn read_cc_switch_providers(
    path: &std::path::Path,
) -> Result<Vec<CcSwitchRow>, AppError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| AppError::Database(format!("failed to open cc-switch DB: {e}")))?;

    let exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='providers'",
            [],
            |r| r.get(0),
        )
        .map_err(|e| AppError::Database(format!("cc-switch DB probe failed: {e}")))?;
    if exists == 0 {
        return Err(AppError::Database(format!(
            "cc-switch DB at {} has no `providers` table — wrong file?",
            path.display()
        )));
    }

    let row_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM providers", [], |r| r.get(0))
        .map_err(|e| AppError::Database(format!("cc-switch DB row count failed: {e}")))?;
    if row_count < 0 || row_count as usize > MAX_SOURCE_ROWS {
        return Err(AppError::InvalidInput(
            "supplier_import_source_too_large".into(),
        ));
    }

    let endpoint_index = read_provider_endpoints(&conn)?;
    let mut stmt = conn
        .prepare(
            "SELECT id, app_type, name, settings_config, website_url, category,
                    created_at, sort_index, notes, icon, icon_color, meta,
                    is_current, in_failover_queue
             FROM providers
             ORDER BY is_current DESC, in_failover_queue DESC, sort_index ASC, id ASC",
        )
        .map_err(|e| AppError::Database(format!("cc-switch DB prepare failed: {e}")))?;
    let row_results = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                map_app_type(&r.get::<_, String>(1)?),
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<i64>>(6)?,
                r.get::<_, Option<usize>>(7)?,
                r.get::<_, Option<String>>(8)?,
                r.get::<_, Option<String>>(9)?,
                r.get::<_, Option<String>>(10)?,
                r.get::<_, String>(11)?,
                r.get::<_, i64>(12)? != 0,
                r.get::<_, i64>(13)? != 0,
            ))
        })
        .map_err(|e| AppError::Database(format!("cc-switch DB query failed: {e}")))?;
    let mut rows = Vec::new();
    for decoded in row_results {
        let (
            id,
            app_type,
            name,
            settings_config,
            website_url,
            category,
            created_at,
            sort_index,
            notes,
            icon,
            icon_color,
            meta_json,
            is_current,
            in_failover_queue,
        ) = decoded
            .map_err(|e| AppError::Database(format!("cc-switch DB row decode failed: {e}")))?;
        let (meta, meta_issue) = parse_meta_column(&meta_json);
        let (endpoint_urls, endpoints_capped) = endpoint_index
            .get(&(id.clone(), app_type.clone()))
            .map(|urls| {
                if urls.len() > MAX_ENDPOINT_URLS_PER_ROW {
                    (urls[..MAX_ENDPOINT_URLS_PER_ROW].to_vec(), true)
                } else {
                    (urls.clone(), false)
                }
            })
            .unwrap_or_default();
        rows.push(CcSwitchRow {
            id,
            app_type,
            name,
            settings_config,
            website_url,
            category,
            created_at,
            sort_index,
            notes,
            icon,
            icon_color,
            is_current,
            in_failover_queue,
            meta,
            meta_json,
            meta_issue,
            endpoint_urls,
            endpoints_capped,
        });
    }
    Ok(rows)
}

/// Read the `provider_endpoints` table when present: `(provider_id,
/// mapped app_type) -> [url, ...]` in cc-switch's stable order. An old
/// database without the table yields an empty index; the URLs are
/// supplementary metadata, never a reason to reject the whole file.
fn read_provider_endpoints(
    conn: &Connection,
) -> Result<std::collections::HashMap<(String, String), Vec<String>>, AppError> {
    let mut index = std::collections::HashMap::new();
    let exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='provider_endpoints'",
            [],
            |r| r.get(0),
        )
        .map_err(|e| AppError::Database(format!("cc-switch DB probe failed: {e}")))?;
    if exists == 0 {
        return Ok(index);
    }
    let mut stmt = conn
        .prepare(
            "SELECT provider_id, app_type, url FROM provider_endpoints
             ORDER BY added_at ASC, url ASC",
        )
        .map_err(|e| AppError::Database(format!("cc-switch DB endpoint query failed: {e}")))?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .map_err(|e| AppError::Database(format!("cc-switch DB endpoint query failed: {e}")))?;
    for row in rows {
        let (provider_id, app_type, url) =
            row.map_err(|e| AppError::Database(format!("cc-switch DB row decode failed: {e}")))?;
        index
            .entry((provider_id, map_app_type(&app_type)))
            .or_insert_with(Vec::new)
            .push(url);
    }
    Ok(index)
}

/// Strict parse of the standalone `meta` JSON column. Unknown shapes are
/// reported through the returned issue code instead of being guessed at.
fn parse_meta_column(meta_json: &str) -> (Option<CcSwitchMeta>, Option<&'static str>) {
    if meta_json.trim().is_empty() {
        return (None, None);
    }
    if meta_json.len() > MAX_META_BYTES {
        return (None, Some("meta_too_large"));
    }
    let value: serde_json::Value = match serde_json::from_str(meta_json) {
        Ok(value) => value,
        Err(_) => return (None, Some("meta_invalid")),
    };
    let Some(object) = value.as_object() else {
        return (None, Some("meta_invalid"));
    };
    let api_format = match object.get("apiFormat") {
        None => None,
        Some(serde_json::Value::String(value)) if !value.trim().is_empty() => {
            Some(value.trim().to_string())
        }
        Some(_) => return (None, Some("meta_api_format_invalid")),
    };
    let auth_binding_kind = object
        .get("authBinding")
        .and_then(serde_json::Value::as_object)
        .and_then(|binding| binding.get("source"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let api_key_field = object
        .get("apiKeyField")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let is_full_url = object
        .get("isFullUrl")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    (
        Some(CcSwitchMeta {
            api_format,
            auth_binding_kind,
            api_key_field,
            is_full_url,
        }),
        None,
    )
}
