from __future__ import annotations

import contextlib
import importlib.machinery
import importlib.util
import io
import json
import os
import re
import unittest
from pathlib import Path
from unittest import mock


BIN = Path(__file__).resolve().parents[1] / "bin" / "openai-websearch"
SKILL = Path(__file__).resolve().parents[1] / "SKILL.md"
GOVERNED_ENVELOPE = {"query": "bounded", "model": "gpt-4o-mini"}


def load_adapter():
    loader = importlib.machinery.SourceFileLoader("openai_websearch_adapter", str(BIN))
    spec = importlib.util.spec_from_loader(loader.name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


def manifest_governed_kill_secs() -> int:
    """`runtime.limits.timeout_secs` — the runtime's wall-clock kill.

    The adapter must not read its own YAML manifest at execution time, so it
    mirrors this number as `GOVERNED_KILL_CEILING_SECS`. This reader is how the
    test reaches the authoritative side of that mirror.
    """
    frontmatter = SKILL.read_text(encoding="utf-8").split("\n---\n", 1)[0]
    declared = re.findall(r"^ *timeout_secs: (\d+)$", frontmatter, re.MULTILINE)
    assert len(declared) == 1, f"expected one governed kill, found {declared}"
    return int(declared[0])


class FakeResponse:
    def __init__(self, payload: object):
        self.payload = json.dumps(payload).encode("utf-8")

    def __enter__(self):
        return self

    def __exit__(self, *_args):
        return False

    def read(self, limit: int) -> bytes:
        return self.payload[:limit]


class OpenAIWebsearchAdapterTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.adapter = load_adapter()

    def args(self, **overrides):
        values = {"query": "bounded", "model": "gpt-4o-mini"}
        values.update(overrides)
        return self.adapter.parse_input(json.dumps(values).encode())

    def test_builds_documented_request_and_keeps_key_in_fixed_origin_header(self):
        args = self.args(
            query="bounded answer",
            model="gpt-4o",
            allowed_domains="https://Example.com/,research.example",
        )
        captured = {}

        def opener(request, timeout):
            captured["request"] = request
            captured["timeout"] = timeout
            return FakeResponse(
                {
                    "model": "gpt-4o",
                    "output": [],
                    "usage": {"input_tokens": 3, "total_tokens": 4},
                }
            )

        self.adapter.perform_search("canary-secret", args, opener=opener)

        request = captured["request"]
        body = json.loads(request.data)
        self.assertEqual(request.full_url, "https://api.openai.com/v1/responses")
        self.assertEqual(request.get_header("Authorization"), "Bearer canary-secret")
        self.assertNotIn("canary-secret", request.full_url)
        self.assertNotIn("canary-secret", json.dumps(body))
        self.assertEqual(body["tools"][0]["type"], "web_search")
        self.assertEqual(
            body["tools"][0]["filters"]["allowed_domains"],
            ["example.com", "research.example"],
        )
        self.assertEqual(body["include"], ["web_search_call.action.sources"])
        self.assertEqual(captured["timeout"], 60)

    def test_normalizes_bounded_answer_citations_complete_sources_and_usage(self):
        adapter = self.adapter
        output = adapter.normalize_response(
            {
                "model": "m" * 300,
                "output": [
                    {
                        "type": "message",
                        "content": [
                            {
                                "type": "output_text",
                                "text": "a" * (adapter.MAX_ANSWER_CHARS + 1),
                                "annotations": [
                                    {
                                        "type": "url_citation",
                                        "title": "Citation",
                                        "url": "https://one.example/article",
                                    },
                                    {"type": "file_citation", "url": "https://ignored.example"},
                                ],
                            }
                        ],
                    },
                    {
                        "type": "web_search_call",
                        "action": {
                            "sources": [
                                {
                                    "title": "Duplicate",
                                    "url": "https://one.example/article",
                                },
                                {"title": "Consulted", "url": "https://two.example"},
                                {"title": "Unsafe", "url": "javascript:alert(1)"},
                                {
                                    "title": "Credential URL",
                                    "url": "https://user:secret@three.example",
                                },
                                {
                                    "title": "Oversized URL",
                                    "url": "https://four.example/" + "x" * 8_192,
                                },
                                {
                                    "title": "Control URL",
                                    "url": "https://five.example/bad\npath",
                                },
                            ]
                        },
                    },
                ],
                "usage": {
                    "input_tokens": 10,
                    "output_tokens": 5,
                    "total_tokens": 15,
                    "input_tokens_details": {"unbounded": "x" * 1000},
                },
            }
        )

        self.assertEqual(len(output["answer"]), adapter.MAX_ANSWER_CHARS)
        self.assertEqual(len(output["model"]), adapter.MAX_MODEL_CHARS)
        self.assertEqual(
            [source["url"] for source in output["sources"]],
            ["https://one.example/article", "https://two.example"],
        )
        self.assertEqual(
            output["usage"],
            {"input_tokens": 10, "output_tokens": 5, "total_tokens": 15},
        )

    def test_domain_and_response_authority_fail_closed(self):
        for value in (
            "https://user:password@example.com",
            "https://example.com/private",
            "ftp://example.com",
            "example.com:443",
        ):
            with self.subTest(value=value), self.assertRaises(ValueError):
                self.adapter.normalize_domains(value)
        with self.assertRaisesRegex(ValueError, "at most 100"):
            self.adapter.normalize_domains(",".join(f"d{i}.example" for i in range(101)))

        handler = self.adapter.NoRedirectHandler()
        self.assertIsNone(handler.redirect_request(None, None, None, None, None, None))
        with self.assertRaisesRegex(ValueError, "provider error"):
            self.adapter.normalize_response({"error": {"message": "sensitive"}})

    def test_rejects_oversized_response_and_non_finite_or_recursive_usage(self):
        args = self.args()
        adapter = self.adapter

        class OversizedResponse(FakeResponse):
            def read(self, _limit: int) -> bytes:
                return b"x" * (adapter.MAX_RESPONSE_BYTES + 1)

        with self.assertRaisesRegex(ValueError, "size limit"):
            adapter.perform_search(
                "canary-secret",
                args,
                opener=lambda *_args, **_kwargs: OversizedResponse({}),
            )
        output = adapter.normalize_response(
            {
                "output": [],
                "usage": {
                    "input_tokens": float("nan"),
                    "output_tokens": True,
                    "total_tokens": -1,
                },
            }
        )
        self.assertEqual(output["usage"], {})

    def test_governed_envelope_preserves_custom_model_support(self):
        args = self.args(query="ok", model="future-model")
        self.assertEqual(args.model, "future-model")
        with self.assertRaisesRegex(ValueError, "input envelope"):
            self.adapter.parse_input(b"[]")
        with self.assertRaisesRegex(ValueError, "input envelope"):
            self.adapter.parse_input(b"x" * (self.adapter.MAX_INPUT_BYTES + 1))

    def test_governed_kill_outlasts_the_inner_provider_budget(self):
        # The governed kill backs stops a wedged process. Firing before the work
        # it backs stops makes it the primary timeout under the wrong name, and
        # turns this adapter's timeout branch into dead code. The kill and the
        # provider deadline were an exact tie, which the kill always won because
        # the socket deadline only starts after process start and stdin read.
        # SKILL.md is YAML the adapter must not read at execution time, so it
        # mirrors the ceiling and this test holds the two files together.
        adapter = self.adapter
        self.assertEqual(
            adapter.INNER_WORST_CASE_SECS,
            adapter.PROVIDER_TIMEOUT_SECS,
            "perform_search issues one request; the declared budget must match",
        )
        self.assertEqual(
            adapter.GOVERNED_KILL_CEILING_SECS,
            manifest_governed_kill_secs(),
            "the adapter no longer mirrors runtime.limits.timeout_secs",
        )
        self.assertGreater(adapter.GOVERNED_KILL_MARGIN_SECS, 0)
        self.assertGreaterEqual(
            adapter.GOVERNED_KILL_CEILING_SECS,
            adapter.INNER_WORST_CASE_SECS + adapter.GOVERNED_KILL_MARGIN_SECS,
        )

    def test_missing_credential_fails_closed_with_a_useful_message(self):
        adapter = self.adapter
        stdout = io.StringIO()
        stderr = io.StringIO()
        with mock.patch.dict(os.environ, {}, clear=True):
            with contextlib.redirect_stdout(stdout):
                with contextlib.redirect_stderr(stderr):
                    result = adapter.main(json.dumps(GOVERNED_ENVELOPE).encode())
        self.assertEqual(result, 2)
        self.assertEqual(stdout.getvalue(), "", "a failure must write nothing to stdout")
        self.assertEqual(
            json.loads(stderr.getvalue()), {"error": "OPENAI_API_KEY not set"}
        )

    def test_failures_never_expose_the_credential_on_any_stream(self):
        adapter = self.adapter
        for failure in (
            ValueError("canary-openai-secret"),
            adapter.urllib.error.URLError("canary-openai-secret"),
            TimeoutError("canary-openai-secret"),
        ):
            stdout = io.StringIO()
            stderr = io.StringIO()
            with mock.patch.dict(
                os.environ, {"OPENAI_API_KEY": "canary-openai-secret"}, clear=True
            ):
                with mock.patch.object(adapter, "perform_search", side_effect=failure):
                    with contextlib.redirect_stdout(stdout):
                        with contextlib.redirect_stderr(stderr):
                            result = adapter.main(
                                json.dumps(GOVERNED_ENVELOPE).encode()
                            )
            self.assertEqual(result, 1)
            self.assertEqual(stdout.getvalue(), "")
            self.assertEqual(
                json.loads(stderr.getvalue()), {"error": type(failure).__name__}
            )
            self.assertNotIn("canary-openai-secret", stdout.getvalue())
            self.assertNotIn("canary-openai-secret", stderr.getvalue())


class SearchIsUnconditionalTests(unittest.TestCase):
    """Offering the tool is not the same as using it.

    Left to choose, `gpt-4o-mini` answered "What is the Rust programming
    language?" from its own weights: `status: completed`, a 1,500-character
    answer, no `web_search_call` anywhere in the output, and therefore zero
    citations and zero sources. A plausible answer with no provenance, from a
    skill whose contract is an answer WITH sources — and the canary's
    `min_items: 1` at `/sources` was the only thing that noticed.
    """

    @classmethod
    def setUpClass(cls):
        cls.adapter = load_adapter()

    def test_the_request_names_the_web_search_tool_as_the_required_choice(self):
        body = self.adapter.request_body(
            self.adapter.SimpleNamespace(
                query="bounded", model="gpt-4o-mini", allowed_domains=None
            )
        )
        self.assertEqual(body["tool_choice"], {"type": "web_search"})
        self.assertEqual([tool["type"] for tool in body["tools"]], ["web_search"])

    def test_the_canary_still_demands_at_least_one_source(self):
        # The assertion that caught it. Relaxing `/sources` to "the pointer
        # exists" would readmit an unsourced answer as a pass.
        frontmatter = SKILL.read_text(encoding="utf-8").split("\n---\n", 1)[0]
        self.assertIn("min_items: 1", frontmatter)
        self.assertIn('items_pointer: "/sources"', frontmatter)


class ProviderRefusalIsLegibleTests(unittest.TestCase):
    """A refusal must say what it refused.

    `{"error": "HTTPError"}` was the whole envelope, so a retired model, a
    rejected tool spec, an unfunded project and a revoked key were one
    indistinguishable word. The class name stays — callers read it — and the
    status and the provider's own message join it.
    """

    @classmethod
    def setUpClass(cls):
        cls.adapter = load_adapter()

    def refusal(self, body: bytes):
        return self.adapter.urllib.error.HTTPError(
            "https://api.openai.com/v1/responses",
            400,
            "Bad Request",
            {},
            io.BytesIO(body),
        )

    def test_the_status_and_the_provider_message_reach_the_envelope(self):
        adapter = self.adapter
        stderr = io.StringIO()
        failure = self.refusal(
            json.dumps({"error": {"message": "The model `gpt-nope` does not exist."}}).encode()
        )
        with mock.patch.dict(
            os.environ, {"OPENAI_API_KEY": "canary-openai-secret"}, clear=True
        ):
            with mock.patch.object(adapter, "perform_search", side_effect=failure):
                with contextlib.redirect_stderr(stderr):
                    result = adapter.main(json.dumps(GOVERNED_ENVELOPE).encode())
        self.assertEqual(result, 1)
        emitted = json.loads(stderr.getvalue())
        self.assertEqual(emitted["error"], "HTTPError")
        self.assertEqual(emitted["status"], 400)
        self.assertIn("gpt-nope", emitted["detail"])

    def test_a_quoted_credential_is_redacted_before_the_bound(self):
        adapter = self.adapter
        stderr = io.StringIO()
        # Redaction must run before truncation, or a key could survive as the
        # tail fragment of an over-long provider message.
        message = "x" * 400 + "canary-openai-secret" + "y" * 400
        failure = self.refusal(json.dumps({"error": {"message": message}}).encode())
        with mock.patch.dict(
            os.environ, {"OPENAI_API_KEY": "canary-openai-secret"}, clear=True
        ):
            with mock.patch.object(adapter, "perform_search", side_effect=failure):
                with contextlib.redirect_stderr(stderr):
                    adapter.main(json.dumps(GOVERNED_ENVELOPE).encode())
        emitted = stderr.getvalue()
        self.assertNotIn("canary-openai-secret", emitted)
        self.assertLessEqual(
            len(json.loads(emitted)["detail"]), adapter.MAX_FAILURE_DETAIL_CHARS
        )

    def test_a_non_http_failure_keeps_the_bare_name_it_always_had(self):
        adapter = self.adapter
        stderr = io.StringIO()
        with mock.patch.dict(
            os.environ, {"OPENAI_API_KEY": "canary-openai-secret"}, clear=True
        ):
            with mock.patch.object(
                adapter, "perform_search", side_effect=ValueError("boom")
            ):
                with contextlib.redirect_stderr(stderr):
                    adapter.main(json.dumps(GOVERNED_ENVELOPE).encode())
        self.assertEqual(json.loads(stderr.getvalue()), {"error": "ValueError"})


if __name__ == "__main__":
    unittest.main()
