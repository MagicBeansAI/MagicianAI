from __future__ import annotations

import base64
import contextlib
from email.parser import BytesParser
from email.policy import default
import importlib.machinery
import importlib.util
import io
import http.client
import json
import os
from pathlib import Path
import tempfile
import subprocess
import sys
from types import SimpleNamespace
import unittest
from unittest import mock
import urllib.error

from PIL import Image
import yaml


PACKAGE = Path(__file__).resolve().parents[1]
loader = importlib.machinery.SourceFileLoader("openai_image_adapter", str(PACKAGE / "bin/openai-image"))
spec = importlib.util.spec_from_loader(loader.name, loader)
adapter = importlib.util.module_from_spec(spec)
loader.exec_module(adapter)


def png(size=(8, 8), mode="RGBA"):
    output = io.BytesIO()
    Image.new(mode, size).save(output, format="PNG")
    return output.getvalue()


USAGE = {"input_tokens": 30, "output_tokens": 40, "total_tokens": 70,
         "input_tokens_details": {"text_tokens": 10, "image_tokens": 20}}


class Response:
    headers = {"x-request-id": "req-image-fixture"}

    def __init__(self, **overrides):
        payload = {"data": [{"b64_json": base64.b64encode(png()).decode()}], "usage": USAGE}
        payload.update(overrides)
        self.body = json.dumps(payload).encode()

    def __enter__(self):
        return self

    def __exit__(self, *_args):
        return False

    def read(self, limit):
        return self.body[:limit]


class OpenAIImageTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.cwd = Path.cwd()
        os.chdir(self.temp.name)
        self.addCleanup(self.temp.cleanup)
        self.addCleanup(os.chdir, self.cwd)

    def args(self, action="generate", **overrides):
        values = {"prompt": "A forest trail", "output_path": "result.png"}
        if action == "edit":
            Path("source.png").write_bytes(png())
            values["image_path"] = "source.png"
        values.update(overrides)
        return adapter.parse_input(action, json.dumps(values).encode())

    def test_generate_saves_real_artifact_and_reports_provider_usage_and_tariff(self):
        opener = mock.Mock(return_value=Response())
        result = adapter.perform("generate", self.args(), "fixture-secret", opener)
        request = opener.call_args.args[0]
        self.assertEqual(request.full_url, "https://api.openai.com/v1/images/generations")
        self.assertEqual(request.get_header("Authorization"), "Bearer fixture-secret")
        body = json.loads(request.data)
        self.assertEqual(body["model"], "gpt-image-2.5-flare")
        self.assertEqual(body["n"], 1)
        self.assertNotIn("fixture-secret", request.data.decode())
        self.assertEqual(opener.call_args.kwargs["timeout"], 300)
        self.assertEqual(Path(result["images"][0]["path"]).read_bytes(), png())
        self.assertEqual(result["images"][0]["media_type"], "image/png")
        self.assertEqual(result["images"][0]["width"], 8)
        self.assertEqual(result["usage"], USAGE)
        self.assertEqual(result["cost_microusd"], 1410)
        self.assertEqual(result["request_id"], "req-image-fixture")
        self.assertEqual(result["pricing_version"], adapter.PRICING_VERSION)
        self.assertEqual(Path("result.png").stat().st_mode & 0o777, 0o600)

    def test_edit_uploads_source_and_matching_mask_without_altering_source(self):
        args = self.args("edit", mask_path="mask.png")
        original = Path("source.png").read_bytes()
        Path("mask.png").write_bytes(png())
        opener = mock.Mock(return_value=Response())
        result = adapter.perform("edit", args, "fixture-secret", opener)
        request = opener.call_args.args[0]
        self.assertEqual(request.full_url, "https://api.openai.com/v1/images/edits")
        message = BytesParser(policy=default).parsebytes(
            ("Content-Type: " + request.get_header("Content-type") + "\r\n\r\n").encode() + request.data)
        parts = {part.get_param("name", header="content-disposition"): part for part in message.iter_parts()}
        self.assertEqual(parts["image[]"].get_payload(decode=True), original)
        self.assertEqual(parts["mask"].get_payload(decode=True), png())
        self.assertEqual(parts["n"].get_content().strip(), "1")
        self.assertEqual(parts["model"].get_content().strip(), "gpt-image-2.5-sunburst")
        self.assertNotIn("input_fidelity", parts)
        self.assertEqual(Path("source.png").read_bytes(), original)
        self.assertTrue(result["images"])
        self.assertEqual(opener.call_count, 1)

    def test_invalid_options_and_sizes_fail_before_provider_dispatch(self):
        for options in [{"size": "2049x2048"}, {"size": "4096x2048"}, {"size": "512x512"},
                        {"size": "2048x256"}, {"size": 1024}, {"model": "unpriced-model"},
                        {"output_format": "jpeg", "output_path": "result.jpg", "background": "transparent"},
                        {"output_format": "jpeg"}, {"api_key": "caller-key"}, {"base_url": "https://other.invalid"},
                        {"n": 10}, {"image_path": "source.png"},
                        {"model": "gpt-image-2"}, {"model": "gpt-image-2-2026-04-21"}, {"quality": "ultra"}]:
            with self.subTest(options=options), self.assertRaises(ValueError):
                self.args(**options)
        for size in ["1024x1024", "1536x864", "3840x2160", "2160x3840", "auto"]:
            self.assertEqual(self.args(size=size)["size"], size)
        for quality in ["auto", "low", "medium", "high", "xhigh", "max"]:
            self.assertEqual(self.args(quality=quality)["quality"], quality)
        for model in sorted(adapter.MODELS):
            self.assertEqual(self.args(model=model)["model"], model)

    def test_each_action_defaults_to_the_variant_openai_recommends(self):
        self.assertEqual(self.args()["model"], "gpt-image-2.5-flare")
        self.assertEqual(self.args("edit")["model"], "gpt-image-2.5-sunburst")
        self.assertEqual(self.args("edit", model="gpt-image-2.5-flare")["model"], "gpt-image-2.5-flare")

    def test_workspace_confinement_and_new_file_check_precede_paid_request(self):
        opener = mock.Mock(return_value=Response())
        Path("taken.png").write_bytes(b"keep")
        for path in ["taken.png", "../escape.png", "/tmp/escape.png"]:
            with self.subTest(path=path), self.assertRaises((ValueError, OSError)):
                adapter.perform("generate", self.args(output_path=path), "key", opener)
        Path("outside").symlink_to(self.cwd, target_is_directory=True)
        with self.assertRaises(OSError):
            adapter.perform("generate", self.args(output_path="outside/escape.png"), "key", opener)
        self.assertEqual(Path("taken.png").read_bytes(), b"keep")
        opener.assert_not_called()

    def test_symlink_and_non_image_inputs_do_not_reach_provider(self):
        args = self.args("edit")
        opener = mock.Mock(return_value=Response())
        Path("linked.png").symlink_to("source.png")
        with self.assertRaises(OSError):
            adapter.perform("edit", {**args, "image_path": "linked.png"}, "key", opener)
        Path("source.png").write_bytes(b"not an image")
        with self.assertRaises(OSError):
            adapter.perform("edit", args, "key", opener)
        opener.assert_not_called()

    def test_mask_size_transparency_and_byte_limits_are_checked_before_upload(self):
        args = self.args("edit", mask_path="mask.png")
        opener = mock.Mock(return_value=Response())
        for data in [png((9, 8)), png(mode="RGB")]:
            Path("mask.png").write_bytes(data)
            with self.assertRaises(ValueError):
                adapter.perform("edit", args, "key", opener)
        with mock.patch.object(adapter, "MAX_IMAGE_BYTES", 2), self.assertRaises(ValueError):
            adapter.perform("edit", args, "key", opener)
        opener.assert_not_called()

    def test_missing_or_inconsistent_usage_never_becomes_a_zero_charge(self):
        cases = [None, {}, {**USAGE, "total_tokens": 1}, {**USAGE, "input_tokens": True},
                 {**USAGE, "output_tokens": -1}, {**USAGE, "input_tokens_details": {"text_tokens": 30}},
                 {**USAGE, "output_tokens_details": {"image_tokens": 39, "text_tokens": 1}}]
        for usage in cases:
            with self.subTest(usage=usage):
                retained, cost = adapter.accounting(usage)
                self.assertIsNone(cost)
                self.assertIsInstance(retained, dict)
        result = adapter.perform("generate", self.args(), "key", mock.Mock(return_value=Response(usage=None)))
        self.assertIsNone(result["cost_microusd"])
        self.assertEqual(result["cost_status"], "unavailable")
        self.assertTrue(result["images"])

    def test_bad_artifact_retains_paid_receipt_and_usage_without_creating_file(self):
        for data in [[{"b64_json": "malformed!"}], [], [{"b64_json": base64.b64encode(b"invalid").decode()}]]:
            with self.subTest(data=data):
                opener = mock.Mock(return_value=Response(data=data))
                result = adapter.perform("generate", self.args(), "key", opener)
                self.assertEqual(result["images"], [])
                self.assertIn("artifact_error", result)
                self.assertEqual(result["request_id"], "req-image-fixture")
                self.assertEqual(result["cost_microusd"], 1410)
                self.assertFalse(Path("result.png").exists())
                self.assertEqual(opener.call_count, 1)

    def test_save_failure_retains_paid_accounting_and_does_not_overwrite_racing_file(self):
        def opener(*_args, **_kwargs):
            Path("result.png").write_bytes(b"concurrent writer")
            return Response()
        result = adapter.perform("generate", self.args(), "key", opener)
        self.assertIn("artifact_error", result)
        self.assertEqual(result["cost_microusd"], 1410)
        self.assertEqual(Path("result.png").read_bytes(), b"concurrent writer")

    def test_transport_failure_makes_one_request_and_redirects_are_denied(self):
        opener = mock.Mock(side_effect=urllib.error.URLError("unknown provider outcome"))
        with self.assertRaises(OSError):
            adapter.perform("generate", self.args(), "key", opener)
        self.assertEqual(opener.call_count, 1)
        self.assertFalse(Path("result.png").exists())
        self.assertIsNone(adapter.NoRedirects().redirect_request(None, None, 302, None, None, "https://other.invalid"))

    def test_oversized_response_and_image_are_bounded(self):
        with mock.patch.object(adapter, "MAX_RESPONSE_BYTES", 2):
            result = adapter.perform("generate", self.args(), "key", mock.Mock(return_value=Response()))
        self.assertIn("artifact_error", result)
        self.assertEqual(result["request_id"], "req-image-fixture")
        self.assertIsNone(result["cost_microusd"])
        with mock.patch.object(adapter, "MAX_OUTPUT_BYTES", 2):
            result = adapter.perform("generate", self.args(), "key", mock.Mock(return_value=Response()))
        self.assertIn("artifact_error", result)
        self.assertFalse(Path("result.png").exists())

    def test_unreadable_provider_body_retains_header_receipt_without_assuming_charge(self):
        for failure in [http.client.IncompleteRead(b"private-body"), OSError("private-body")]:
            response = Response()
            response.read = mock.Mock(side_effect=failure)
            opener = mock.Mock(return_value=response)
            result = adapter.perform("generate", self.args(), "key", opener)
            self.assertEqual(result["request_id"], "req-image-fixture")
            self.assertIsNone(result["cost_microusd"])
            self.assertIn("artifact_error", result)
            self.assertNotIn("private-body", json.dumps(result))
            self.assertEqual(opener.call_count, 1)
        response = Response()
        response.body = b"private-body is not JSON"
        result = adapter.perform("generate", self.args(), "key", mock.Mock(return_value=response))
        self.assertIsNone(result["cost_microusd"])
        self.assertEqual(result["images"], [])
        self.assertEqual(result["request_id"], "req-image-fixture")

    def test_cli_requires_injected_credentials_and_redacts_provider_failures(self):
        raw = json.dumps({"prompt": "private-prompt", "output_path": "result.png"}).encode()
        child = subprocess.run([str(PACKAGE / "bin/openai-image"), "generate"], input=raw,
                               env={"PATH": str(Path(sys.executable).parent) + ":/usr/bin:/bin",
                                    "PYTHONDONTWRITEBYTECODE": "1"}, capture_output=True, timeout=10)
        self.assertEqual(child.returncode, 1)
        self.assertIn("error", json.loads(child.stdout))
        self.assertNotIn(b"private-prompt", child.stdout + child.stderr)
        error = urllib.error.HTTPError(adapter.API_ORIGIN + "generations", 429, "private-prompt",
                                       {"x-request-id": "req-refused"}, io.BytesIO(b"private-key"))
        output = io.StringIO()
        with mock.patch.object(sys, "argv", ["openai-image", "generate"]), \
                mock.patch.object(sys, "stdin", SimpleNamespace(buffer=io.BytesIO(raw))), \
                mock.patch.dict(os.environ, {"OPENAI_API_KEY": "private-key"}), \
                mock.patch.object(adapter, "perform", side_effect=error), contextlib.redirect_stdout(output):
            self.assertEqual(adapter.main(), 1)
        result = json.loads(output.getvalue())
        self.assertEqual(result["http_status"], 429)
        self.assertEqual(result["request_id"], "req-refused")
        self.assertNotIn("private", output.getvalue())

    def test_manifest_selects_actions_vault_and_workspace_authority(self):
        manifest = yaml.safe_load((PACKAGE / "SKILL.md").read_text().split("---", 2)[1])
        meta = manifest["metadata"]["magician"]
        runtime = meta["runtime_contract"]
        self.assertEqual(runtime["auth"]["secret_bindings"][0]["secret_ref"], "OPENAI_API_KEY")
        self.assertEqual(runtime["runtime"]["stdin"]["sensitivity"], "private")
        self.assertGreater(runtime["runtime"]["limits"]["timeout_secs"], adapter.PROVIDER_TIMEOUT_SECS)
        actions = meta["runtime_actions"]["actions"]
        self.assertEqual(set(actions), {"generate", "edit"})
        for name, action in actions.items():
            self.assertEqual(action["fixed_args"], [name])
            self.assertEqual(action["parameters"]["output_path"]["access"], "create_file")
            for parameter in action["parameters"].values():
                self.assertLessEqual(set(parameter), {"type", "access", "description", "required", "default",
                                                      "min_length", "max_length", "enum_values"})
        self.assertEqual(actions["edit"]["parameters"]["image_path"]["access"], "read_file")
        self.assertEqual(actions["generate"]["parameters"]["model"]["default"], "gpt-image-2.5-flare")
        self.assertEqual(actions["edit"]["parameters"]["model"]["default"], "gpt-image-2.5-sunburst")
        for name, action in actions.items():
            self.assertEqual(set(action["parameters"]["model"]["enum_values"]), adapter.MODELS)
            self.assertEqual(set(action["parameters"]["quality"]["enum_values"]), adapter.QUALITIES)


if __name__ == "__main__":
    unittest.main()
