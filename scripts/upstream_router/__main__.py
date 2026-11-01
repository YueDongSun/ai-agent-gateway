"""Validate or safely render reusable upstream-router configuration."""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
import sys
from pathlib import Path

from .router_config import RouterConfigError, load_router_config, render_litellm_config
from .router_runtime import RouterReadinessError, validate_environment, write_config_exclusive


def main() -> int:
    parser = argparse.ArgumentParser(description="Validate reusable upstream router configuration")
    parser.add_argument("--config", required=True, type=Path)
    action_group = parser.add_mutually_exclusive_group(required=True)
    action_group.add_argument("--check", action="store_true", help="validate configuration and report readiness")
    action_group.add_argument("--output", type=Path, help="create an env-reference-only LiteLLM JSON file")
    args = parser.parse_args()
    try:
        config = load_router_config(args.config)
        env_ready = True
        try:
            validate_environment(config, os.environ)
        except RouterReadinessError:
            env_ready = False
        litellm_ready = importlib.util.find_spec("litellm") is not None
        if args.output:
            if not args.output.is_absolute():
                raise RouterConfigError("output path must be absolute")
            payload = json.dumps(render_litellm_config(config), indent=2) + "\n"
            write_config_exclusive(args.output, payload)
        if args.check or args.output:
            print(f"config=valid env_ready={str(env_ready).lower()} litellm_installed={str(litellm_ready).lower()}")
            if args.output:
                host = config["listener"]["host"]
                port = config["listener"]["port"]
                print(f"output=created command=python -B -m litellm.proxy.proxy_cli --config <output> --host {host} --port {port} --telemetry False")
            return 0 if (not args.check or env_ready) else 2
    except (RouterConfigError, OSError) as error:
        # Do not print exception text; it can include local paths or config data.
        print(f"router_error={type(error).__name__}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
