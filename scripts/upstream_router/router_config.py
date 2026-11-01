"""Validate router inputs and render an environment-reference-only LiteLLM config."""

from __future__ import annotations

import json
import re
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit


ENV_NAME_PATTERN = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")
ALIAS_PATTERN = re.compile(r"^[A-Za-z0-9_]+$")
LITELLM_DEFAULTS = {
    "num_retries": 0,
    "allowed_fails": 1,
    "cooldown_time": 60,
    "enable_pre_call_checks": True,
    "drop_params": True,
    "request_timeout": 180,
    "set_verbose": False,
}


class RouterConfigError(ValueError):
    """Raised when the router config is invalid."""


def _reject_duplicate_object_pairs(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    """Build JSON objects while rejecting repeated keys at every nesting level."""
    result = {}
    for key, value in pairs:
        if key in result:
            raise RouterConfigError("router JSON contains a duplicate field")
        result[key] = value
    return result


def _require_object(value: Any, label: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise RouterConfigError(f"{label} must be an object")
    return value


def _require_env_name(value: Any, label: str) -> str:
    if not isinstance(value, str) or not ENV_NAME_PATTERN.fullmatch(value):
        raise RouterConfigError(f"{label} must be an environment variable name")
    return value


def _validate_base_url(value: Any, label: str) -> str:
    if not isinstance(value, str) or any(character.isspace() or ord(character) < 32 for character in value):
        raise RouterConfigError(f"{label} must be an http(s) URL")
    try:
        parts = urlsplit(value)
    except ValueError as error:
        raise RouterConfigError(f"{label} must be an http(s) URL") from error
    if (parts.scheme not in {"http", "https"} or not parts.hostname
            or parts.username is not None or parts.password is not None
            or parts.query or parts.fragment):
        raise RouterConfigError(f"{label} must be an http(s) URL without userinfo, query, or fragment")
    try:
        parts.port
    except ValueError as error:
        raise RouterConfigError(f"{label} has an invalid port") from error
    return value.rstrip("/")


def validate_listener(host: Any, port: Any) -> dict[str, Any]:
    """Validate the explicit loopback bind target."""
    if not isinstance(host, str) or host not in {"127.0.0.1", "::1", "localhost"}:
        raise RouterConfigError("listener.host must be loopback")
    if type(port) is not int or not 1 <= port <= 65535:
        raise RouterConfigError("listener.port must be an integer from 1 to 65535")
    return {"host": host, "port": port}


def load_router_config(path: Path) -> dict[str, Any]:
    """Read JSON configuration without reading any external provider state."""
    try:
        value = json.loads(path.read_text(encoding="utf-8"), object_pairs_hook=_reject_duplicate_object_pairs)
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise RouterConfigError("unable to read router JSON config") from error
    return validate_router_config(value)


def validate_router_config(value: Any) -> dict[str, Any]:
    """Validate schema version, listener, provider aliases, and fallback references."""
    config = _require_object(value, "config")
    allowed_config_keys = {"schema_version", "listener", "master_key_env", "models", "fallbacks",
                           "context_window_fallbacks"}
    if set(config) - allowed_config_keys:
        raise RouterConfigError("config contains unknown fields")
    if type(config.get("schema_version")) is not int or config["schema_version"] != 1:
        raise RouterConfigError("schema_version must be integer 1")
    listener = _require_object(config.get("listener"), "listener")
    if set(listener) != {"host", "port"}:
        raise RouterConfigError("listener contains unknown or missing fields")
    validated_listener = validate_listener(listener.get("host"), listener.get("port"))
    master_key_env = _require_env_name(config.get("master_key_env"), "master_key_env")
    models = config.get("models")
    if not isinstance(models, list) or not models:
        raise RouterConfigError("models must be a non-empty array")
    aliases: set[str] = set()
    validated_models = []
    for index, raw_model in enumerate(models):
        model = _require_object(raw_model, f"models[{index}]")
        allowed_model_keys = {"alias", "model", "model_info_id", "api_base", "api_key_env", "max_input_tokens"}
        if set(model) - allowed_model_keys:
            raise RouterConfigError(f"models[{index}] contains unknown fields")
        alias = model.get("alias")
        if not isinstance(alias, str) or not ALIAS_PATTERN.fullmatch(alias):
            raise RouterConfigError(f"models[{index}].alias must use letters, digits, or underscores")
        if alias in aliases:
            raise RouterConfigError("model aliases must be unique")
        aliases.add(alias)
        model_id = model.get("model")
        if not isinstance(model_id, str) or not model_id.strip():
            raise RouterConfigError(f"models[{index}].model must be a non-empty string")
        model_info_id = model.get("model_info_id", alias)
        if not isinstance(model_info_id, str) or not model_info_id.strip():
            raise RouterConfigError(f"models[{index}].model_info_id must be a non-empty string")
        max_tokens = model.get("max_input_tokens")
        if type(max_tokens) is not int or max_tokens < 1:
            raise RouterConfigError(f"models[{index}].max_input_tokens must be a positive integer")
        validated_models.append({
            "alias": alias,
            "model": model_id,
            "model_info_id": model_info_id,
            "api_base": _validate_base_url(model.get("api_base"), f"models[{index}].api_base"),
            "api_key_env": _require_env_name(model.get("api_key_env"), f"models[{index}].api_key_env"),
            "max_input_tokens": max_tokens,
        })
    fallback_maps = {}
    for key in ("fallbacks", "context_window_fallbacks"):
        raw_map = _require_object(config.get(key, {}), key)
        checked_map = {}
        for alias, targets in raw_map.items():
            if alias not in aliases:
                raise RouterConfigError(f"{key} contains an unknown source alias")
            if not isinstance(targets, list) or not targets:
                raise RouterConfigError(f"{key} targets must be non-empty arrays")
            if any(not isinstance(target, str) or target not in aliases for target in targets):
                raise RouterConfigError(f"{key} contains an unknown target alias")
            if alias in targets:
                raise RouterConfigError(f"{key} cannot reference itself")
            if len(targets) != len(set(targets)):
                raise RouterConfigError(f"{key} cannot contain duplicate targets")
            checked_map[alias] = list(targets)
        fallback_maps[key] = checked_map
    return {
        "schema_version": 1,
        "listener": validated_listener,
        "master_key_env": master_key_env,
        "models": validated_models,
        **fallback_maps,
    }


def render_litellm_config(config: dict[str, Any]) -> dict[str, Any]:
    """Build a LiteLLM-compatible JSON object using env references only."""
    checked = validate_router_config(config)
    return {
        "model_list": [
            {
                "model_name": model["alias"],
                "litellm_params": {
                    "model": model["model"],
                    "api_base": model["api_base"],
                    "api_key": f"os.environ/{model['api_key_env']}",
                },
                "model_info": {
                    "id": model["model_info_id"],
                    "max_input_tokens": model["max_input_tokens"],
                },
            }
            for model in checked["models"]
        ],
        "router_settings": {
            **{key: LITELLM_DEFAULTS[key] for key in (
                "num_retries", "allowed_fails", "cooldown_time", "enable_pre_call_checks"
            )},
            "fallbacks": [{alias: targets} for alias, targets in checked["fallbacks"].items()],
            "context_window_fallbacks": [
                {alias: targets} for alias, targets in checked["context_window_fallbacks"].items()
            ],
        },
        "litellm_settings": {
            **{key: LITELLM_DEFAULTS[key] for key in ("drop_params", "request_timeout", "set_verbose")},
        },
        "general_settings": {
            "master_key": f"os.environ/{checked['master_key_env']}",
        },
    }
