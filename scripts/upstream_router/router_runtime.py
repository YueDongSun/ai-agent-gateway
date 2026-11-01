"""Prepare a caller-owned LiteLLM command and sanitized loopback environment."""

from __future__ import annotations

import stat
import os
import sys
from pathlib import Path
from typing import Mapping

from .router_config import validate_listener, validate_router_config


class RouterReadinessError(ValueError):
    """Raised when required env vars are missing, without disclosing values."""


def required_env_names(config: dict) -> set[str]:
    checked = validate_router_config(config)
    return {checked["master_key_env"], *(model["api_key_env"] for model in checked["models"])}


def validate_environment(config: dict, environment: Mapping[str, str]) -> None:
    missing = sorted(name for name in required_env_names(config) if not environment.get(name))
    if missing:
        raise RouterReadinessError("required environment variables are missing")


def build_litellm_command(interpreter: Path, config_path: Path, host: str, port: int) -> list[str]:
    """Return an explicit foreground command; the caller owns process lifecycle."""
    if not interpreter.is_absolute() or not config_path.is_absolute():
        raise ValueError("interpreter and config paths must be absolute")
    checked_listener = validate_listener(host, port)
    return [str(interpreter), "-m", "litellm.proxy.proxy_cli", "--config", str(config_path),
            "--host", checked_listener["host"], "--port", str(checked_listener["port"]),
            "--telemetry", "False"]


def build_litellm_environment(environment: Mapping[str, str], host: str) -> dict[str, str]:
    """Copy caller environment and replace proxy bypass with loopback-only values."""
    result = dict(environment)
    validate_listener(host, 1)
    bypass = "127.0.0.1,localhost,[::1]" if host == "::1" else "127.0.0.1,localhost"
    result["LITELLM_LOG"] = "ERROR"
    result["NO_COLOR"] = "1"
    result["NO_PROXY"] = bypass
    result["no_proxy"] = bypass
    return result


def prepare_launch_spec(
    config: dict,
    config_path: Path,
    environment: Mapping[str, str],
    interpreter: Path | None = None,
) -> dict:
    """Prepare caller-owned command and environment; never starts a process.

    The returned environment can contain credentials and must not be printed,
    serialized, or sent to another process except by its owner.
    """
    checked = validate_router_config(config)
    validate_environment(checked, environment)
    listener = checked["listener"]
    return {
        "command": build_litellm_command(interpreter or Path(sys.executable), config_path,
                                         listener["host"], listener["port"]),
        "environment": build_litellm_environment(environment, listener["host"]),
        "host": listener["host"],
        "port": listener["port"],
    }


def write_config_exclusive(path: Path, content: str) -> None:
    """Create a config without overwrite, mode 0600 on POSIX, cleaning own partials."""
    if not path.is_absolute():
        raise ValueError("output path must be absolute")
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, stat.S_IRUSR | stat.S_IWUSR)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8", newline="\n") as output_file:
            descriptor = -1
            output_file.write(content)
            output_file.flush()
            os.fsync(output_file.fileno())
    except BaseException as error:
        if descriptor >= 0:
            try:
                os.close(descriptor)
            except OSError:
                error.add_note("failed to close the created router config file")
        try:
            path.unlink(missing_ok=True)
        except OSError:
            error.add_note("failed to remove the partial router config file")
        raise
