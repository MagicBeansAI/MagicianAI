import importlib.util
from importlib.machinery import SourceFileLoader
import contextlib
import io
import json
import os
import pathlib
from types import SimpleNamespace
import unittest
from unittest import mock


SCRIPT_PATH = pathlib.Path(__file__).parents[1] / "bin" / "veo31"
LOADER = SourceFileLoader("veo31_router", str(SCRIPT_PATH))
SPEC = importlib.util.spec_from_loader("veo31_router", LOADER)
MODULE = importlib.util.module_from_spec(SPEC)
LOADER.exec_module(MODULE)


class Veo31RoutingTest(unittest.TestCase):
    def test_manual_model_override_wins(self):
        model, tier = MODULE.resolve_model(
            model_override="veo-3.1-fast-generate-preview",
            quality_tier="balanced",
        )

        self.assertEqual(model, "veo-3.1-fast-generate-preview")
        self.assertEqual(tier, "manual")

    def test_auto_defaults_to_balanced(self):
        model, tier = MODULE.resolve_model(
            model_override="",
            quality_tier="auto",
        )

        self.assertEqual(model, "veo-3.1-generate-preview")
        self.assertEqual(tier, "balanced")

    def test_explicit_fast_selects_fast_model(self):
        model, tier = MODULE.resolve_model(
            model_override="",
            quality_tier="fast",
        )

        self.assertEqual(model, "veo-3.1-fast-generate-preview")
        self.assertEqual(tier, "fast")

    def test_invalid_tier_raises(self):
        with self.assertRaises(ValueError):
            MODULE.resolve_model(
                model_override="",
                quality_tier="pro",
            )

    def test_unsupported_sdk_controls_do_not_create_a_client(self):
        client = mock.Mock()
        for params in [
            {"generate_audio": False}, {"generate_audio": True}, {"seed": 7},
            {"enhance_prompt": False}, {"enhance_prompt": True},
        ]:
            with self.subTest(params=params), mock.patch.object(
                MODULE, "genai", SimpleNamespace(Client=client)
            ), mock.patch.object(MODULE, "types", object()), mock.patch.object(
                MODULE, "read_runtime_input", return_value={"prompt": "synthetic", **params}
            ), mock.patch.dict(os.environ, {"VEO31_API_KEY": "synthetic-fixture"}, clear=True), contextlib.redirect_stdout(io.StringIO()) as output:
                with self.assertRaises(SystemExit) as stopped:
                    MODULE.main()
                self.assertEqual(stopped.exception.code, 1)
                self.assertIn("Unsupported", json.loads(output.getvalue())["error"])
        client.assert_not_called()

    def test_invalid_count_or_resolution_duration_does_not_create_a_client(self):
        client = mock.Mock()
        for params in [{"number_of_videos": 2}, {"resolution": "1080p", "duration_seconds": 4}]:
            with self.subTest(params=params), mock.patch.object(
                MODULE, "genai", SimpleNamespace(Client=client)
            ), mock.patch.object(MODULE, "types", object()), mock.patch.object(
                MODULE, "read_runtime_input", return_value={"prompt": "synthetic", **params}
            ), mock.patch.dict(os.environ, {"VEO31_API_KEY": "synthetic-fixture"}, clear=True), contextlib.redirect_stdout(io.StringIO()) as output:
                with self.assertRaises(SystemExit) as stopped:
                    MODULE.main()
                self.assertEqual(stopped.exception.code, 1)
                self.assertIn("error", json.loads(output.getvalue()))
        client.assert_not_called()

    @unittest.skipIf(MODULE.genai is None, "run with the installed skill SDK environment")
    def test_real_sdk_serializes_the_wrapper_request_without_paid_network(self):
        # Exercise the installed SDK's public request path. A permissive fake
        # GenerateVideosConfig missed the unsupported generate_audio field.
        import httpx

        sdk = MODULE.genai
        requests = []

        def handler(request):
            self.assertEqual(request.method, "POST")
            self.assertTrue(request.url.path.endswith(":predictLongRunning"))
            requests.append(json.loads(request.content))
            return httpx.Response(200, json={
                "name": "operations/synthetic-sdk-request", "done": True,
                "response": {"generateVideoResponse": {"generatedSamples": []}},
            })

        clients = []

        def make_client(**kwargs):
            kwargs["http_options"].client_args = {"transport": httpx.MockTransport(handler)}
            client = sdk.Client(**kwargs)
            clients.append(client)
            return client

        try:
            with mock.patch.object(MODULE, "genai", SimpleNamespace(
                Client=make_client, errors=sdk.errors
            )), mock.patch.object(MODULE, "read_runtime_input", return_value={
                "prompt": "synthetic request", "quality_tier": "fast",
                "duration_seconds": 4, "resolution": "720p",
            }), mock.patch.dict(os.environ, {"VEO31_API_KEY": "synthetic-fixture"}, clear=True), contextlib.redirect_stdout(io.StringIO()) as output, contextlib.redirect_stderr(io.StringIO()):
                MODULE.main()
                result = json.loads(output.getvalue())
            self.assertEqual(result["operation_name"], "operations/synthetic-sdk-request")
            self.assertEqual(len(requests), 1)
            self.assertEqual(requests[0]["parameters"]["durationSeconds"], 4)
            self.assertEqual(requests[0]["parameters"]["sampleCount"], 1)
            self.assertNotIn("generateAudio", requests[0]["parameters"])
            self.assertNotIn("seed", requests[0]["parameters"])
            self.assertNotIn("enhancePrompt", requests[0]["parameters"])
        finally:
            for client in clients:
                client.close()

    @unittest.skipIf(MODULE.genai is None, "run with the installed skill SDK environment")
    def test_real_sdk_submission_error_is_redacted_and_never_retried(self):
        import httpx

        sdk = MODULE.genai
        for status, provider_status in [(400, "INVALID_ARGUMENT"), (500, "INTERNAL")]:
            with self.subTest(status=status):
                requests = []
                clients = []

                def handler(request):
                    requests.append(request)
                    return httpx.Response(status, json={"error": {
                        "code": status, "status": provider_status,
                        "message": "Synthetic provider failure for synthetic-fixture",
                    }})

                def make_client(**kwargs):
                    kwargs["http_options"].client_args = {"transport": httpx.MockTransport(handler)}
                    client = sdk.Client(**kwargs)
                    clients.append(client)
                    return client

                try:
                    with mock.patch.object(MODULE, "genai", SimpleNamespace(
                        Client=make_client, errors=sdk.errors
                    )), mock.patch.object(MODULE, "read_runtime_input", return_value={
                        "prompt": "synthetic request", "duration_seconds": 4,
                    }), mock.patch.dict(os.environ, {"VEO31_API_KEY": "synthetic-fixture"}, clear=True), contextlib.redirect_stdout(io.StringIO()) as output, contextlib.redirect_stderr(io.StringIO()) as diagnostic:
                        with self.assertRaises(SystemExit) as stopped:
                            MODULE.main()
                    self.assertEqual(stopped.exception.code, 1)
                    self.assertEqual(len(requests), 1)
                    self.assertEqual(requests[0].method, "POST")
                    result = json.loads(output.getvalue())
                    self.assertEqual(result["http_status"], status)
                    self.assertEqual(result["provider_status"], provider_status)
                    self.assertEqual(result["phase"], "submission")
                    self.assertIsNone(result["operation_name"])
                    self.assertEqual(json.loads(diagnostic.getvalue().splitlines()[-1]), result)
                    self.assertIn("[REDACTED]", result["error"])
                    self.assertNotIn("synthetic-fixture", output.getvalue() + diagnostic.getvalue())
                finally:
                    for client in clients:
                        client.close()

    @unittest.skipIf(MODULE.genai is None, "run with the installed skill SDK environment")
    def test_real_sdk_poll_error_retains_operation_and_does_not_resubmit(self):
        import httpx

        sdk = MODULE.genai
        methods = []
        clients = []

        def handler(request):
            methods.append(request.method)
            if request.method == "POST":
                return httpx.Response(200, json={"name": "operations/synthetic-known", "done": False})
            return httpx.Response(503, json={"error": {
                "code": 503, "status": "UNAVAILABLE", "message": "Synthetic poll failure",
            }})

        def make_client(**kwargs):
            kwargs["http_options"].client_args = {"transport": httpx.MockTransport(handler)}
            client = sdk.Client(**kwargs)
            clients.append(client)
            return client

        try:
            with mock.patch.object(MODULE, "genai", SimpleNamespace(
                Client=make_client, errors=sdk.errors
            )), mock.patch.object(MODULE, "read_runtime_input", return_value={
                "prompt": "synthetic request", "duration_seconds": 4,
            }), mock.patch.object(MODULE.time, "sleep"), mock.patch.dict(os.environ, {"VEO31_API_KEY": "synthetic-fixture"}, clear=True), contextlib.redirect_stdout(io.StringIO()) as output, contextlib.redirect_stderr(io.StringIO()):
                with self.assertRaises(SystemExit) as stopped:
                    MODULE.main()
            self.assertEqual(stopped.exception.code, 1)
            self.assertEqual(methods, ["POST", "GET"])
            result = json.loads(output.getvalue())
            self.assertEqual(result["http_status"], 503)
            self.assertEqual(result["phase"], "polling")
            self.assertEqual(result["operation_name"], "operations/synthetic-known")
        finally:
            for client in clients:
                client.close()


if __name__ == "__main__":
    unittest.main()
