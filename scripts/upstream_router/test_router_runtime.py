"""Focused caller-owned launch-spec tests."""

import unittest
import sys
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest.mock import patch

from scripts.upstream_router.router_runtime import (
    RouterReadinessError,
    build_litellm_command,
    build_litellm_environment,
    validate_environment,
    prepare_launch_spec,
    write_config_exclusive,
)
from scripts.upstream_router.test_router_config import BASE_CONFIG


class RouterRuntimeTests(unittest.TestCase):
    def test_command_uses_explicit_paths_and_foreground_cli(self):
        with TemporaryDirectory() as temp_dir:
            config_path = Path(temp_dir) / "router.json"
            interpreter = Path(sys.executable).resolve()
            command = build_litellm_command(interpreter, config_path, "127.0.0.1", 4000)
        self.assertEqual(command, [str(Path(sys.executable).resolve()), "-m", "litellm.proxy.proxy_cli", "--config",
                                   str(config_path), "--host", "127.0.0.1", "--port", "4000",
                                   "--telemetry", "False"])
        with self.assertRaises(ValueError):
            build_litellm_command(Path("python"), Path("router.json"), "127.0.0.1", 4000)

    def test_environment_replaces_malformed_bypass_values(self):
        source = {"NO_PROXY": "2001:db8::", "no_proxy": "10.0.0.0/8",
                  "HTTPS_PROXY": "http://proxy.example.test:8080"}
        result = build_litellm_environment(source, "127.0.0.1")
        self.assertEqual(source["NO_PROXY"], "2001:db8::")
        self.assertEqual(result["HTTPS_PROXY"], source["HTTPS_PROXY"])
        self.assertEqual(result["LITELLM_LOG"], "ERROR")
        self.assertEqual(result["NO_COLOR"], "1")
        self.assertEqual(result["NO_PROXY"], "127.0.0.1,localhost")
        self.assertEqual(result["no_proxy"], "127.0.0.1,localhost")
        self.assertNotIn("2001:db8::", result["NO_PROXY"])

    def test_environment_validation_does_not_disclose_values(self):
        environment = {"ROUTER_MASTER_KEY": "secret", "PROVIDER_A_KEY": "secret"}
        with self.assertRaises(RouterReadinessError) as caught:
            validate_environment(BASE_CONFIG, environment)
        self.assertNotIn("secret", str(caught.exception))

    def test_launch_spec_uses_validated_bind_and_supplied_environment(self):
        supplied = {"ROUTER_MASTER_KEY": "test-master", "PROVIDER_A_KEY": "test-a",
                    "PROVIDER_B_KEY": "test-b", "NO_PROXY": "malformed::entry"}
        with TemporaryDirectory() as temp_dir:
            spec = prepare_launch_spec(BASE_CONFIG, Path(temp_dir) / "router.json", supplied,
                                       Path(sys.executable).resolve())
        self.assertEqual(spec["host"], "127.0.0.1")
        self.assertEqual(spec["port"], 4000)
        self.assertEqual(spec["environment"]["NO_PROXY"], "127.0.0.1,localhost")
        self.assertEqual(spec["environment"]["ROUTER_MASTER_KEY"], "test-master")
        self.assertEqual(spec["command"][2], "litellm.proxy.proxy_cli")

    def test_exclusive_config_write_preserves_existing_and_cleans_failed_partial(self):
        with TemporaryDirectory() as temp_dir:
            output_path = Path(temp_dir) / "litellm.json"
            output_path.write_text("existing", encoding="utf-8")
            with self.assertRaises(FileExistsError):
                write_config_exclusive(output_path, "replacement")
            self.assertEqual(output_path.read_text(encoding="utf-8"), "existing")
            output_path.unlink()
            with patch("scripts.upstream_router.router_runtime.os.fsync", side_effect=OSError("synthetic")):
                with self.assertRaises(OSError):
                    write_config_exclusive(output_path, "partial")
            self.assertFalse(output_path.exists())
            with self.assertRaises(UnicodeEncodeError):
                write_config_exclusive(output_path, "invalid-\ud800")
            self.assertFalse(output_path.exists())


if __name__ == "__main__":
    unittest.main()
