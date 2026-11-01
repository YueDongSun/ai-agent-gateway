"""CLI behavior tests using temporary configs and synthetic environment values."""

import contextlib
import io
import json
import os
import sys
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest.mock import patch

from scripts.upstream_router import __main__
from scripts.upstream_router.test_router_config import BASE_CONFIG


class RouterCliTests(unittest.TestCase):
    def setUp(self):
        self.temp_directory = TemporaryDirectory()
        self.addCleanup(self.temp_directory.cleanup)
        self.directory = Path(self.temp_directory.name)
        self.config_path = self.directory / "router_config.json"
        self.config_path.write_text(json.dumps(BASE_CONFIG), encoding="utf-8")

    def _run_cli(self, arguments):
        stdout = io.StringIO()
        stderr = io.StringIO()
        with patch.object(sys, "argv", ["upstream_router", *map(str, arguments)]):
            with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                try:
                    exit_code = __main__.main()
                except SystemExit as error:
                    exit_code = error.code
        return exit_code, stdout.getvalue(), stderr.getvalue()

    def test_missing_or_conflicting_action_never_writes_output(self):
        output_path = self.directory / "litellm_config.json"
        missing_code, _, missing_error = self._run_cli(["--config", self.config_path])
        conflicting_code, _, conflicting_error = self._run_cli([
            "--config", self.config_path, "--check", "--output", output_path,
        ])
        self.assertEqual(missing_code, 2)
        self.assertEqual(conflicting_code, 2)
        self.assertIn("required", missing_error)
        self.assertIn("not allowed with argument", conflicting_error)
        self.assertFalse(output_path.exists())

    def test_check_flow_reports_readiness_without_writing(self):
        synthetic_environment = {
            "ROUTER_MASTER_KEY": "synthetic-master-key",
            "PROVIDER_A_KEY": "synthetic-provider-a",
            "PROVIDER_B_KEY": "synthetic-provider-b",
        }
        with patch.dict(os.environ, synthetic_environment, clear=True):
            with patch.object(__main__.importlib.util, "find_spec", return_value=None):
                exit_code, stdout, _ = self._run_cli(["--config", self.config_path, "--check"])
        self.assertEqual(exit_code, 0)
        self.assertIn("config=valid env_ready=true litellm_installed=false", stdout)
        self.assertNotIn("synthetic", stdout)
        self.assertEqual(list(self.directory.glob("*")), [self.config_path])

    def test_render_flow_creates_only_env_reference_config(self):
        output_path = self.directory / "litellm_config.json"
        with patch.object(__main__.importlib.util, "find_spec", return_value=None):
            exit_code, stdout, _ = self._run_cli([
                "--config", self.config_path, "--output", output_path,
            ])
        rendered = json.loads(output_path.read_text(encoding="utf-8"))
        self.assertEqual(exit_code, 0)
        self.assertIn("output=created command=python", stdout)
        self.assertEqual(rendered["model_list"][0]["litellm_params"]["api_key"], "os.environ/PROVIDER_A_KEY")
        self.assertEqual(rendered["general_settings"]["master_key"], "os.environ/ROUTER_MASTER_KEY")
        self.assertFalse(any("synthetic" in line for line in output_path.read_text(encoding="utf-8").splitlines()))


if __name__ == "__main__":
    unittest.main()
