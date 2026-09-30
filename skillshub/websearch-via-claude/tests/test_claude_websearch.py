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


BIN = Path(__file__).resolve().parents[1] / "bin" / "claude-websearch"
SKILL = Path(__file__).resolve().parents[1] / "SKILL.md"


def load_adapter():
    loader = importlib.machinery.SourceFileLoader("claude_websearch_adapter", str(BIN))
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


class ClaudeWebsearchAdapterTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.adapter = load_adapter()

    def args(self, **overrides):
        values = {
            "query": "bounded",
            "model": "claude-haiku-4-5-20251001",
            "max_searches": 3,
        }
        values.update(overrides)
        return self.adapter.parse_input(json.dumps(values).encode())

    def test_builds_direct_search_request_and_keeps_key_in_fixed_origin_header(self):
        args = self.args(
            query="cited answer",
            model="future-claude",
            max_searches=5,
            allowed_domains="https://Example.com/blog/,research.example",
        )
        captured = []

        def opener(request, timeout):
            captured.append((request, timeout))
            return FakeResponse(
                {
                    "model": "future-claude",
                    "stop_reason": "end_turn",
                    "content": [],
                    "usage": {"input_tokens": 3, "output_tokens": 2},
                }
            )

        self.adapter.perform_search("canary-secret", args, opener=opener)

        request, timeout = captured[0]
        body = json.loads(request.data)
        self.assertEqual(request.full_url, "https://api.anthropic.com/v1/messages")
        self.assertEqual(request.get_header("X-api-key"), "canary-secret")
        self.assertEqual(request.get_header("Anthropic-version"), "2023-06-01")
        self.assertNotIn("canary-secret", request.full_url)
        self.assertNotIn("canary-secret", json.dumps(body))
        self.assertEqual(body["tools"][0]["type"], "web_search_20260209")
        self.assertEqual(body["tools"][0]["allowed_callers"], ["direct"])
        self.assertEqual(body["tools"][0]["max_uses"], 5)
        self.assertEqual(
            body["tools"][0]["allowed_domains"],
            ["example.com/blog/", "research.example"],
        )
        self.assertEqual(timeout, 60)

    def test_pause_turn_is_replayed_exactly_and_normalized_iteratively(self):
        args = self.args(query="continue me")
        paused_content = [
            {
                "type": "web_search_tool_result",
                "tool_use_id": "srvtoolu_one",
                "content": [
                    {
                        "type": "web_search_result",
                        "title": "Result",
                        "url": "https://one.example",
                        "encrypted_content": "opaque-value",
                    }
                ],
            }
        ]
        payloads = [
            {
                "model": "claude-haiku-4-5-20251001",
                "stop_reason": "pause_turn",
                "content": paused_content,
                "usage": {"input_tokens": 5, "server_tool_use": {"web_search_requests": 1}},
            },
            {
                "model": "claude-haiku-4-5-20251001",
                "stop_reason": "end_turn",
                "content": [
                    {
                        "type": "text",
                        "text": "Final answer",
                        "citations": [
                            {
                                "type": "web_search_result_location",
                                "title": "Cited result",
                                "url": "https://one.example",
                                "cited_text": "Relevant evidence",
                            }
                        ],
                    }
                ],
                "usage": {"output_tokens": 7, "server_tool_use": {"web_search_requests": 1}},
            },
        ]
        requests = []

        def opener(request, timeout):
            requests.append(json.loads(request.data))
            return FakeResponse(payloads[len(requests) - 1])

        output = self.adapter.perform_search("canary-secret", args, opener=opener)

        self.assertEqual(len(requests), 2)
        self.assertEqual(requests[0]["tools"][0]["max_uses"], 3)
        self.assertEqual(requests[1]["tools"][0]["max_uses"], 2)
        self.assertEqual(requests[1]["messages"][1], {"role": "assistant", "content": paused_content})
        self.assertEqual(output["answer"], "Final answer")
        self.assertEqual(
            output["sources"],
            [
                {
                    "title": "Cited result",
                    "url": "https://one.example",
                    "cited_text": "Relevant evidence",
                }
            ],
        )
        self.assertNotIn("opaque-value", json.dumps(output))
        self.assertEqual(
            output["usage"],
            {"input_tokens": 5, "output_tokens": 7, "web_search_requests": 2},
        )

    def test_tool_errors_redirects_and_invalid_domains_fail_closed(self):
        for value in (
            "https://user:password@example.com",
            "ftp://example.com",
            "example.com:443",
            "example.com/path?query=yes",
        ):
            with self.subTest(value=value), self.assertRaises(ValueError):
                self.adapter.normalize_domains(value)
        with self.assertRaisesRegex(ValueError, "at most 100"):
            self.adapter.normalize_domains(",".join(f"d{i}.example" for i in range(101)))

        handler = self.adapter.NoRedirectHandler()
        self.assertIsNone(handler.redirect_request(None, None, None, None, None, None))
        with self.assertRaisesRegex(ValueError, "tool error"):
            self.adapter.validate_provider_response(
                {
                    "content": [
                        {
                            "type": "web_search_tool_result",
                            "content": {
                                "type": "web_search_tool_result_error",
                                "error_code": "unavailable",
                            },
                        }
                    ]
                }
            )
        with self.assertRaisesRegex(ValueError, "invalid message content"):
            self.adapter.validate_provider_response({"stop_reason": "pause_turn"})

    def test_response_and_pause_limits_are_bounded_without_recursion(self):
        args = self.args()
        adapter = self.adapter

        class OversizedResponse(FakeResponse):
            def read(self, _limit: int) -> bytes:
                return b"x" * (adapter.MAX_RESPONSE_BYTES + 1)

        with self.assertRaisesRegex(ValueError, "per-response size limit"):
            adapter.perform_search(
                "canary-secret",
                args,
                opener=lambda *_args, **_kwargs: OversizedResponse({}),
            )

        calls = 0

        def always_paused(_request, timeout):
            nonlocal calls
            self.assertEqual(timeout, 60)
            calls += 1
            return FakeResponse(
                {"stop_reason": "pause_turn", "content": [], "usage": {}}
            )

        with self.assertRaisesRegex(ValueError, "continuation limit"):
            adapter.perform_search("canary-secret", args, opener=always_paused)
        self.assertEqual(calls, adapter.MAX_PAUSE_CONTINUATIONS + 1)

        with self.assertRaisesRegex(ValueError, "search-use limit"):
            adapter.perform_search(
                "canary-secret",
                self.args(max_searches=1),
                opener=lambda *_args, **_kwargs: FakeResponse(
                    {
                        "stop_reason": "pause_turn",
                        "content": [],
                        "usage": {"server_tool_use": {"web_search_requests": 1}},
                    }
                ),
            )

    def test_governed_envelope_preserves_custom_model_support(self):
        args = self.args(query="ok", model="future-claude")
        self.assertEqual(args.model, "future-claude")
        with self.assertRaisesRegex(ValueError, "input envelope"):
            self.adapter.parse_input(b"[]")
        with self.assertRaisesRegex(ValueError, "input envelope"):
            self.adapter.parse_input(b"x" * (self.adapter.MAX_INPUT_BYTES + 1))

    def test_governed_kill_outlasts_the_inner_provider_budget(self):
        # The governed kill backs stops a wedged process. Firing before the work
        # it backs stops makes it the primary timeout under the wrong name, and
        # turns every pause-turn and timeout branch above into dead code — which
        # is what a 60s kill over a 300s budget did here. SKILL.md is YAML the
        # adapter must not read at execution time, so it mirrors the ceiling and
        # this test is what actually holds the two files together.
        adapter = self.adapter
        self.assertEqual(
            adapter.INNER_WORST_CASE_SECS,
            adapter.PROVIDER_TIMEOUT_SECS * (adapter.MAX_PAUSE_CONTINUATIONS + 1),
            "the declared inner budget no longer matches the request loop",
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
                    result = adapter.main(json.dumps({"query": "anything"}).encode())
        self.assertEqual(result, 2)
        self.assertEqual(stdout.getvalue(), "", "a failure must write nothing to stdout")
        self.assertEqual(
            json.loads(stderr.getvalue()), {"error": "ANTHROPIC_API_KEY not set"}
        )

    def test_failures_never_expose_the_credential_on_any_stream(self):
        adapter = self.adapter
        for failure in (
            ValueError("canary-anthropic-secret"),
            adapter.urllib.error.URLError("canary-anthropic-secret"),
            TimeoutError("canary-anthropic-secret"),
        ):
            stdout = io.StringIO()
            stderr = io.StringIO()
            with mock.patch.dict(
                os.environ,
                {"ANTHROPIC_API_KEY": "canary-anthropic-secret"},
                clear=True,
            ):
                with mock.patch.object(adapter, "perform_search", side_effect=failure):
                    with contextlib.redirect_stdout(stdout):
                        with contextlib.redirect_stderr(stderr):
                            result = adapter.main(
                                json.dumps({"query": "anything"}).encode()
                            )
            self.assertEqual(result, 1)
            self.assertEqual(stdout.getvalue(), "")
            self.assertEqual(
                json.loads(stderr.getvalue()), {"error": type(failure).__name__}
            )
            self.assertNotIn("canary-anthropic-secret", stdout.getvalue())
            self.assertNotIn("canary-anthropic-secret", stderr.getvalue())

    def test_a_provider_refusal_carries_its_status_and_its_own_words(self):
        """The envelope that reported a billing problem as `HTTPError`.

        The live lane's whole account of this skill was `{"error":
        "HTTPError"}`. The provider had in fact answered HTTP 400 with "Your
        credit balance is too low to access the Anthropic API" — an operator
        action, stated plainly, thrown away by the adapter. An unfunded
        account, a retired model id, a tool version the account cannot use and
        an expired key are all 4xx and all identical without the message, so
        the red could not be acted on and could not be told from an outage.
        """
        adapter = self.adapter
        stderr = io.StringIO()
        failure = adapter.urllib.error.HTTPError(
            "https://api.anthropic.com/v1/messages",
            400,
            "Bad Request",
            {},
            io.BytesIO(
                json.dumps(
                    {
                        "type": "error",
                        "error": {
                            "type": "invalid_request_error",
                            "message": "Your credit balance is too low to access the Anthropic API.",
                        },
                    }
                ).encode()
            ),
        )
        with mock.patch.dict(
            os.environ, {"ANTHROPIC_API_KEY": "canary-anthropic-secret"}, clear=True
        ):
            with mock.patch.object(adapter, "perform_search", side_effect=failure):
                with contextlib.redirect_stderr(stderr):
                    result = adapter.main(json.dumps({"query": "anything"}).encode())
        self.assertEqual(result, 1)
        emitted = json.loads(stderr.getvalue())
        self.assertEqual(emitted["error"], "HTTPError")
        self.assertEqual(emitted["status"], 400)
        self.assertIn("credit balance is too low", emitted["detail"])

    def test_a_quoted_credential_is_redacted_before_the_detail_is_bounded(self):
        adapter = self.adapter
        stderr = io.StringIO()
        # Redaction runs before truncation, or a key could survive as the tail
        # fragment of an over-long provider message.
        message = "x" * 400 + "canary-anthropic-secret" + "y" * 400
        failure = adapter.urllib.error.HTTPError(
            "https://api.anthropic.com/v1/messages",
            401,
            "Unauthorized",
            {},
            io.BytesIO(json.dumps({"error": {"message": message}}).encode()),
        )
        with mock.patch.dict(
            os.environ, {"ANTHROPIC_API_KEY": "canary-anthropic-secret"}, clear=True
        ):
            with mock.patch.object(adapter, "perform_search", side_effect=failure):
                with contextlib.redirect_stderr(stderr):
                    adapter.main(json.dumps({"query": "anything"}).encode())
        emitted = stderr.getvalue()
        self.assertNotIn("canary-anthropic-secret", emitted)
        self.assertLessEqual(
            len(json.loads(emitted)["detail"]), adapter.MAX_FAILURE_DETAIL_CHARS
        )


if __name__ == "__main__":
    unittest.main()
