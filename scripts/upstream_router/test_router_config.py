"""Focused configuration contract tests."""

import copy
import json
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory

from scripts.upstream_router.router_config import (
    RouterConfigError,
    load_router_config,
    render_litellm_config,
    validate_router_config,
)


BASE_CONFIG = {
    "schema_version": 1,
    "listener": {"host": "127.0.0.1", "port": 4000},
    "master_key_env": "ROUTER_MASTER_KEY",
    "models": [
        {"alias": "cc_auto", "model": "provider_a/model_a", "api_base": "https://api.example.test/v1",
         "api_key_env": "PROVIDER_A_KEY", "max_input_tokens": 32768},
        {"alias": "cc_deepseek", "model": "provider_b/model_b", "api_base": "http://localhost:9090/v1",
         "api_key_env": "PROVIDER_B_KEY", "max_input_tokens": 65536},
    ],
    "fallbacks": {"cc_auto": ["cc_deepseek"]},
    "context_window_fallbacks": {"cc_auto": ["cc_deepseek"]},
}


class RouterConfigTests(unittest.TestCase):
    def test_load_rejects_duplicate_root_and_nested_fields(self):
        duplicate_documents = [
            json.dumps(BASE_CONFIG).replace('"schema_version": 1',
                                            '"schema_version": 999, "schema_version": 1', 1),
            json.dumps(BASE_CONFIG).replace('"host": "127.0.0.1"',
                                            '"host": "0.0.0.0", "host": "127.0.0.1"', 1),
        ]
        with TemporaryDirectory() as temp_directory:
            config_path = Path(temp_directory) / "duplicate_config.json"
            for document in duplicate_documents:
                with self.subTest(document=document):
                    config_path.write_text(document, encoding="utf-8")
                    with self.assertRaises(RouterConfigError) as caught:
                        load_router_config(config_path)
                    self.assertEqual(str(caught.exception), "router JSON contains a duplicate field")
                    self.assertNotIn("0.0.0.0", str(caught.exception))

    def test_invalid_utf8_preserves_context_without_exposing_input(self):
        with TemporaryDirectory() as temp_directory:
            config_path = Path(temp_directory) / "invalid_config.json"
            config_path.write_bytes(bytes([255, 254, 255]))
            with self.assertRaises(RouterConfigError) as caught:
                load_router_config(config_path)
            self.assertIsInstance(caught.exception.__cause__, UnicodeError)
            self.assertNotIn(str(config_path), str(caught.exception))

    def test_render_uses_env_references_and_expected_defaults(self):
        rendered = render_litellm_config(BASE_CONFIG)
        self.assertEqual(rendered["model_list"][0]["litellm_params"]["api_key"], "os.environ/PROVIDER_A_KEY")
        self.assertEqual(rendered["model_list"][1]["model_info"]["max_input_tokens"], 65536)
        self.assertEqual(rendered["general_settings"]["master_key"], "os.environ/ROUTER_MASTER_KEY")
        settings = rendered["router_settings"]
        self.assertEqual(settings["fallbacks"], [{"cc_auto": ["cc_deepseek"]}])
        self.assertEqual(settings["num_retries"], 0)
        self.assertEqual(settings["allowed_fails"], 1)
        self.assertEqual(settings["cooldown_time"], 60)
        self.assertIs(settings["enable_pre_call_checks"], True)
        self.assertNotIn("drop_params", settings)
        self.assertIs(rendered["litellm_settings"]["drop_params"], True)
        self.assertNotIn("request_timeout", settings)
        self.assertEqual(rendered["litellm_settings"]["request_timeout"], 180)
        self.assertIs(rendered["litellm_settings"]["set_verbose"], False)

    def test_rejects_invalid_boundary_values(self):
        cases = []
        config = copy.deepcopy(BASE_CONFIG)
        config["listener"]["port"] = True
        cases.append(config)
        config = copy.deepcopy(BASE_CONFIG)
        config["models"][1]["alias"] = "cc_auto"
        cases.append(config)
        config = copy.deepcopy(BASE_CONFIG)
        config["models"][0]["api_base"] = "https://user:secret@example.test/v1"
        cases.append(config)
        config = copy.deepcopy(BASE_CONFIG)
        config["models"][0]["api_base"] = "https://example.test/v1?token=value"
        cases.append(config)
        config = copy.deepcopy(BASE_CONFIG)
        config["fallbacks"]["cc_auto"] = ["missing_alias"]
        cases.append(config)
        config = copy.deepcopy(BASE_CONFIG)
        config["fallbacks"]["cc_auto"] = ["cc_auto"]
        cases.append(config)
        config = copy.deepcopy(BASE_CONFIG)
        config["models"][0]["api_key_env"] = "NOT-AN-ENV-NAME"
        cases.append(config)
        config = copy.deepcopy(BASE_CONFIG)
        config["models"][0]["max_input_tokens"] = True
        cases.append(config)
        config = copy.deepcopy(BASE_CONFIG)
        config["listener"]["host"] = []
        cases.append(config)
        config = copy.deepcopy(BASE_CONFIG)
        config["models"][0]["api_base"] = "https://[invalid]/v1"
        cases.append(config)
        config = copy.deepcopy(BASE_CONFIG)
        config["unexpected"] = "credential-like"
        cases.append(config)
        for invalid in cases:
            with self.subTest(invalid=invalid):
                with self.assertRaises(RouterConfigError):
                    validate_router_config(invalid)


if __name__ == "__main__":
    unittest.main()
