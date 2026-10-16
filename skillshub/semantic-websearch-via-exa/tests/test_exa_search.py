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


BIN = Path(__file__).resolve().parents[1] / "bin" / "exa-search"
SKILL = Path(__file__).resolve().parents[1] / "SKILL.md"
GOVERNED_ENVELOPE = {
    "query": "bounded",
    "type": "auto",
    "num_results": 5,
    "contents": True,
    "highlights": False,
}


def load_adapter():
    loader = importlib.machinery.SourceFileLoader("exa_search_adapter", str(BIN))
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


class ExaSearchAdapterTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.adapter = load_adapter()

    def args(self, **overrides):
        values = {
            "query": "bounded",
            "type": "auto",
            "num_results": 5,
            "contents": True,
            "highlights": False,
        }
        values.update(overrides)
        return self.adapter.parse_input(json.dumps(values).encode())

    def test_omitted_optional_parameters_arrive_as_their_declared_defaults(self):
        # The governed runtime materializes a declared `default:` into the
        # envelope, so a parameter is absent only when the caller omits it AND
        # the manifest declares no default. Each such parameter must still
        # materialize as the adapter's own fallback:
        # `request_body` and `normalize_response` read `num_results`,
        # `contents`, and `highlights` off the namespace unguarded, so an
        # omitted one used to raise AttributeError, and an omitted `type` used
        # to raise KeyError out of `parse_input` itself. Both surfaced to the
        # caller as an opaque one-word error with no usable search result.
        args = self.adapter.parse_input(b'{"query":"x"}')

        body = self.adapter.request_body(args)
        self.assertEqual(body["query"], "x")
        self.assertEqual(body["type"], "auto")
        self.assertEqual(body["numResults"], 5)
        self.assertEqual(body["contents"], {"text": {"maxCharacters": 12_000}})
        self.assertNotIn("category", body)
        self.assertNotIn("maxAgeHours", body)
        self.assertNotIn("startPublishedDate", body)
        self.assertNotIn("includeDomains", body)
        self.assertNotIn("excludeDomains", body)

        # `normalize_response` reads the namespace too, and its result cap is
        # the defaulted `num_results`.
        output = self.adapter.normalize_response(
            {"results": [{"title": str(n), "url": f"https://{n}.example"} for n in range(9)]},
            args,
        )
        self.assertEqual(output["search_type"], "auto")
        self.assertEqual(len(output["results"]), 5)

    def test_builds_bounded_request_and_keeps_key_out_of_url_and_body(self):
        args = self.args(
            query="semantic launch",
            type="neural",
            num_results=3,
            highlights=True,
            category="research paper",
            max_age_hours=24,
            start_published_date="2026-08-01",
            include_domains="example.com, research.example",
        )
        captured = {}

        def opener(request, timeout):
            captured["request"] = request
            captured["timeout"] = timeout
            return FakeResponse({"searchType": "auto", "results": []})

        self.adapter.perform_search("canary-secret", args, opener=opener)

        request = captured["request"]
        body = json.loads(request.data)
        self.assertEqual(request.full_url, "https://api.exa.ai/search")
        self.assertNotIn("canary-secret", request.full_url)
        self.assertNotIn("canary-secret", json.dumps(body))
        self.assertEqual(request.get_header("X-api-key"), "canary-secret")
        self.assertEqual(body["type"], "auto")
        self.assertEqual(body["includeDomains"], ["example.com", "research.example"])
        self.assertEqual(body["contents"]["text"]["maxCharacters"], 12_000)
        self.assertEqual(body["contents"]["highlights"]["numSentences"], 5)
        self.assertEqual(captured["timeout"], 60)

    def test_normalization_caps_results_text_highlights_and_synthesis(self):
        args = self.args(num_results=1)
        output = self.adapter.normalize_response(
            {
                "costDollars": 0.005,
                "output": "s" * 13_000,
                "results": [
                    {
                        "title": "one",
                        "url": "https://one.example",
                        "text": "x" * 13_000,
                        "highlights": ["h" * 3_000] * 21,
                    },
                    {"title": "two", "url": "https://two.example"},
                ],
            },
            args,
        )

        self.assertEqual(output["cost"], 0.005)
        self.assertEqual(len(output["synthesis"]), 12_000)
        self.assertEqual([row["title"] for row in output["results"]], ["one"])
        self.assertEqual(len(output["results"][0]["text"]), 12_000)
        self.assertEqual(len(output["results"][0]["highlights"]), 20)
        self.assertEqual(len(output["results"][0]["highlights"][0]), 2_000)

    def test_rejects_oversized_or_error_provider_responses(self):
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
        with self.assertRaisesRegex(ValueError, "provider error"):
            adapter.normalize_response({"error": "sensitive provider detail"}, args)

    def test_redirects_and_non_finite_provider_numbers_fail_closed(self):
        handler = self.adapter.NoRedirectHandler()
        self.assertIsNone(handler.redirect_request(None, None, None, None, None, None))

        args = self.args()
        output = self.adapter.normalize_response(
            {
                "costDollars": float("nan"),
                "results": [
                    {
                        "title": "one",
                        "url": "https://one.example",
                        "score": float("inf"),
                    }
                ],
            },
            args,
        )
        self.assertNotIn("cost", output)
        self.assertNotIn("score", output["results"][0])

        with self.assertRaisesRegex(ValueError, "YYYY-MM-DD"):
            self.args(start_published_date="not-a-date")
        with self.assertRaisesRegex(ValueError, "input envelope"):
            self.adapter.parse_input(b"[]")

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
        self.assertEqual(json.loads(stderr.getvalue()), {"error": "EXA_API_KEY not set"})

    def test_failures_never_expose_the_credential_on_any_stream(self):
        adapter = self.adapter
        for failure in (
            ValueError("canary-exa-secret"),
            adapter.urllib.error.URLError("canary-exa-secret"),
            TimeoutError("canary-exa-secret"),
        ):
            stdout = io.StringIO()
            stderr = io.StringIO()
            with mock.patch.dict(
                os.environ, {"EXA_API_KEY": "canary-exa-secret"}, clear=True
            ):
                with mock.patch.object(adapter, "perform_search", side_effect=failure):
                    with contextlib.redirect_stdout(stdout):
                        with contextlib.redirect_stderr(stderr):
                            result = adapter.main(
                                json.dumps(GOVERNED_ENVELOPE).encode()
                            )
            self.assertEqual(result, 1)
            self.assertEqual(stdout.getvalue(), "")
            emitted = json.loads(stderr.getvalue())
            self.assertEqual(emitted["error"]["kind"], type(failure).__name__)
            self.assertNotIn("canary-exa-secret", stdout.getvalue())
            self.assertNotIn("canary-exa-secret", stderr.getvalue())

    def test_failures_carry_the_provider_message_not_just_its_class_name(self):
        # A missing CA bundle reaches the adapter as a URLError whose *message*
        # is the only place CERTIFICATE_VERIFY_FAILED appears. Emitting the
        # class name alone destroyed it, so the controller could only bucket an
        # operator misconfiguration as a generic provider outage. The message
        # must survive, bounded, alongside the kind.
        adapter = self.adapter
        reason = (
            "<urlopen error [SSL: CERTIFICATE_VERIFY_FAILED] certificate verify "
            "failed: unable to get local issuer certificate (_ssl.c:1028)>"
        )
        stdout = io.StringIO()
        stderr = io.StringIO()
        with mock.patch.dict(os.environ, {"EXA_API_KEY": "unused-key"}, clear=True):
            with mock.patch.object(
                adapter,
                "perform_search",
                side_effect=adapter.urllib.error.URLError(reason),
            ):
                with contextlib.redirect_stdout(stdout):
                    with contextlib.redirect_stderr(stderr):
                        result = adapter.main(json.dumps(GOVERNED_ENVELOPE).encode())
        self.assertEqual(result, 1)
        self.assertEqual(stdout.getvalue(), "")
        emitted = json.loads(stderr.getvalue())
        self.assertEqual(emitted["error"]["kind"], "URLError")
        self.assertIn("CERTIFICATE_VERIFY_FAILED", emitted["error"]["message"])
        self.assertIn("unable to get local issuer certificate", emitted["error"]["message"])

    def test_emitted_failure_message_stays_bounded(self):
        # An unbounded provider message becomes an unbounded log line.
        adapter = self.adapter
        stderr = io.StringIO()
        with mock.patch.dict(os.environ, {"EXA_API_KEY": "unused-key"}, clear=True):
            with mock.patch.object(
                adapter, "perform_search", side_effect=ValueError("x" * 4_000)
            ):
                with contextlib.redirect_stderr(stderr):
                    adapter.main(json.dumps(GOVERNED_ENVELOPE).encode())
        emitted = json.loads(stderr.getvalue())
        self.assertEqual(len(emitted["error"]["message"]), 512)


if __name__ == "__main__":
    unittest.main()
