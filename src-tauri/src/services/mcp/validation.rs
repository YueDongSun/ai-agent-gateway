//! Bounded, conservative common subset of Claude and Codex MCP definitions.
use crate::error::AppError;
use serde_json::{Map, Value};

use super::failure;

pub(super) const MAX_CONFIG_BYTES: usize = 32 * 1024;
const MAX_VALUE_BYTES: usize = 4096;
pub(super) const REDACTED: &str = "[REDACTED]";

pub(super) fn validate_id(id: &str) -> Result<(), AppError> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
        || !id.as_bytes()[0].is_ascii_lowercase()
    {
        return Err(failure("invalid_server_id"));
    }
    Ok(())
}

/// Imported public identifiers retain their established spelling. Never apply
/// the naming policy for new AAG records as an implicit migration.
pub(super) fn validate_existing_id(id: &str) -> Result<(), AppError> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
    {
        return Err(failure("invalid_server_id"));
    }
    Ok(())
}

pub(super) fn validate_label(value: &str, limit: usize) -> Result<(), AppError> {
    if value.trim().is_empty() || value.len() > limit || value.chars().any(char::is_control) {
        return Err(failure("invalid_metadata"));
    }
    Ok(())
}

fn string_value(value: &Value) -> Result<&str, AppError> {
    let text = value
        .as_str()
        .ok_or_else(|| failure("invalid_string_value"))?;
    if text.len() > MAX_VALUE_BYTES || text.chars().any(char::is_control) || text.contains(REDACTED)
    {
        return Err(failure("invalid_or_redacted_value"));
    }
    Ok(text)
}

fn validate_map(value: &Value, is_env: bool) -> Result<(), AppError> {
    let values = value
        .as_object()
        .ok_or_else(|| failure("invalid_string_map"))?;
    if values.len() > 64 {
        return Err(failure("too_many_map_entries"));
    }
    for (key, value) in values {
        let valid_key = if is_env {
            !key.is_empty()
                && key.len() <= 128
                && (key.as_bytes()[0].is_ascii_alphabetic() || key.starts_with('_'))
                && key.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
        } else {
            !key.is_empty()
                && key.len() <= 128
                && key.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
        };
        if !valid_key {
            return Err(failure("invalid_map_key"));
        }
        string_value(value)?;
    }
    Ok(())
}

pub(super) fn validate_config(config: &Value) -> Result<Value, AppError> {
    if serde_json::to_vec(config)?.len() > MAX_CONFIG_BYTES {
        return Err(failure("definition_too_large"));
    }
    let mut object = config
        .as_object()
        .cloned()
        .ok_or_else(|| failure("definition_must_be_object"))?;
    let kind = match object.get("type") {
        Some(Value::String(kind)) => kind.as_str(),
        None if object.contains_key("url") => "http",
        None => "stdio",
        _ => return Err(failure("unsupported_transport")),
    };
    let kind = match kind {
        "stdio" => "stdio",
        "http" | "streamable-http" => "http",
        _ => return Err(failure("unsupported_transport")),
    };
    let allowed = if kind == "stdio" {
        &["type", "command", "args", "env"][..]
    } else {
        &["type", "url", "headers"][..]
    };
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(failure("unsupported_definition_field"));
    }
    if kind == "stdio" {
        let command = string_value(
            object
                .get("command")
                .ok_or_else(|| failure("command_required"))?,
        )?;
        let absolute = std::path::Path::new(command).is_absolute();
        if command.trim().is_empty()
            || command != command.trim()
            || command.contains([';', '|', '&', '`', '$', '<', '>', '\'', '"'])
            || (!absolute && command.contains([' ', '\t', '/', '\\']))
        {
            return Err(failure("command_must_be_executable"));
        }
        let executable = command
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(command)
            .to_ascii_lowercase();
        let executable = executable.trim_end_matches(".exe");
        if matches!(
            executable,
            "cmd"
                | "powershell"
                | "pwsh"
                | "sh"
                | "bash"
                | "zsh"
                | "fish"
                | "dash"
                | "wscript"
                | "cscript"
                | "mshta"
        ) {
            return Err(failure("shell_command_not_allowed"));
        }
        if let Some(args) = object.get("args") {
            let args = args
                .as_array()
                .ok_or_else(|| failure("args_must_be_array"))?;
            if args.len() > 64 {
                return Err(failure("too_many_arguments"));
            }
            for arg in args {
                string_value(arg)?;
            }
        }
        if let Some(env) = object.get("env") {
            validate_map(env, true)?;
        }
    } else {
        let address = string_value(object.get("url").ok_or_else(|| failure("url_required"))?)?;
        let parsed = url::Url::parse(address).map_err(|_| failure("invalid_server_url"))?;
        let loopback = matches!(
            parsed.host_str(),
            Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
        );
        if !(parsed.scheme() == "https" || (parsed.scheme() == "http" && loopback))
            || parsed.host_str().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.fragment().is_some()
        {
            return Err(failure("server_url_requires_https_or_loopback"));
        }
        if let Some(headers) = object.get("headers") {
            validate_map(headers, false)?;
        }
    }
    object.insert("type".into(), Value::String(kind.into()));
    Ok(Value::Object(object))
}

/// Fail closed for imported/unknown definitions. No arbitrary scalar is exposed.
pub(super) fn redact_config(config: &Value) -> Value {
    let Ok(valid) = validate_config(config) else {
        return serde_json::json!({"definition": REDACTED});
    };
    let mut out = Map::new();
    for (key, value) in valid.as_object().expect("validated object") {
        let redacted = match key.as_str() {
            "type" => value.clone(),
            "command" => {
                let command = value.as_str().unwrap_or_default();
                Value::String(
                    command
                        .rsplit(['/', '\\'])
                        .next()
                        .unwrap_or(command)
                        .to_string(),
                )
            }
            "args" => Value::Array(
                value
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|_| Value::String(REDACTED.into()))
                    .collect(),
            ),
            "env" | "headers" => Value::Object(
                value
                    .as_object()
                    .unwrap()
                    .keys()
                    .map(|key| (key.clone(), Value::String(REDACTED.into())))
                    .collect(),
            ),
            _ => Value::String(REDACTED.into()),
        };
        out.insert(key.clone(), redacted);
    }
    Value::Object(out)
}
