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


BIN = Path(__file__).resolve().parents[1] / "bin" / "klipy-gif-search"
SKILL = Path(__file__).resolve().parents[1] / "SKILL.md"
GOVERNED_ENVELOPE = {"query": "reaction", "limit": "5", "content_filter": "pg"}


def load_adapter():
    loader = importlib.machinery.SourceFileLoader("klipy_gif_search_adapter", str(BIN))
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


class KlipyGifSearchAdapterTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.adapter = load_adapter()

    def args(self, **overrides):
        values = {"query": "bounded", "limit": "5", "content_filter": "pg"}
        values.update(overrides)
        return self.adapter.parse_input(json.dumps(values).encode())

    def test_builds_fixed_origin_request_with_escaped_path_credential(self):
        args = self.args(
            query="slow clap & celebrate",
            limit="7",
            content_filter="pg-13",
        )
        captured = {}

        def opener(request, timeout):
            captured["request"] = request
            captured["timeout"] = timeout
            return FakeResponse({"data": {"data": []}})

        self.adapter.perform_search("canary/key", args, opener=opener)

        request = captured["request"]
        parsed = self.adapter.urllib.parse.urlsplit(request.full_url)
        query = self.adapter.urllib.parse.parse_qs(parsed.query)
        self.assertEqual(parsed.scheme, "https")
        self.assertEqual(parsed.netloc, "api.klipy.com")
        self.assertEqual(parsed.path, "/api/v1/canary%2Fkey/gifs/search")
        self.assertEqual(query["q"], ["slow clap & celebrate"])
        self.assertEqual(query["per_page"], ["7"])
        self.assertEqual(query["page"], ["1"])
        self.assertEqual(query["content_filter"], ["pg-13"])
        self.assertEqual(query["locale"], ["en"])
        self.assertEqual(captured["timeout"], 15)
        self.assertNotIn("canary/key", parsed.query)

    def test_normalizes_bounded_safe_renditions_and_fallbacks(self):
        adapter = self.adapter
        output = adapter.normalize_response(
            {
                "data": {
                    "data": [
                        {
                            "title": "t" * (adapter.MAX_TITLE_CHARS + 1),
                            "slug": "s" * (adapter.MAX_SLUG_CHARS + 1),
                            "file": {
                                "hd": {
                                    "gif": {"url": "javascript:unsafe"},
                                    "webp": {"url": "https://cdn.example/preview.webp"},
                                },
                                "md": {"gif": {"url": "https://cdn.example/main.gif"}},
                            },
                        },
                        {
                            "title": "fallback",
                            "url": "https://cdn.example/fallback.gif",
                            "file": {},
                        },
                        {
                            "title": "credential URL",
                            "url": "https://user:secret@cdn.example/reject.gif",
                        },
                    ]
                }
            },
            "reaction",
            20,
        )

        self.assertEqual(output["count"], 2)
        self.assertEqual(output["gifs"][0]["url"], "https://cdn.example/main.gif")
        self.assertEqual(
            output["gifs"][0]["preview"], "https://cdn.example/preview.webp"
        )
        self.assertEqual(len(output["gifs"][0]["title"]), adapter.MAX_TITLE_CHARS)
        self.assertEqual(len(output["gifs"][0]["slug"]), adapter.MAX_SLUG_CHARS)
        self.assertEqual(
            output["gifs"][1]["preview"], "https://cdn.example/fallback.gif"
        )

    def test_provider_redirect_and_unsafe_response_authority_fail_closed(self):
        adapter = self.adapter
        handler = adapter.NoRedirectHandler()
        self.assertIsNone(handler.redirect_request(None, None, None, None, None, None))
        with self.assertRaisesRegex(ValueError, "provider error"):
            adapter.normalize_response({"error": {"message": "sensitive"}}, "q", 5)
        with self.assertRaisesRegex(ValueError, "credential"):
            adapter.validate_api_key("bad\nkey")
        for value in (
            "javascript:unsafe",
            "https://user:secret@example.com/a.gif",
            "https://example.com/" + "x" * adapter.MAX_URL_CHARS,
        ):
            with self.subTest(value=value):
                self.assertEqual(adapter.safe_url(value), "")

    def test_response_and_governed_envelope_are_strictly_bounded(self):
        adapter = self.adapter
        args = self.args()

        class OversizedResponse(FakeResponse):
            def read(self, _limit: int) -> bytes:
                return b"x" * (adapter.MAX_RESPONSE_BYTES + 1)

        with self.assertRaisesRegex(ValueError, "size limit"):
            adapter.perform_search(
                "canary-secret",
                args,
                opener=lambda *_args, **_kwargs: OversizedResponse({}),
            )
        bounded_scan = adapter.normalize_response(
            {
                "data": {
                    "data": [{} for _ in range(adapter.MAX_INSPECTED_ITEMS)]
                    + [{"url": "https://cdn.example/too-late.gif"}]
                }
            },
            "bounded",
            adapter.MAX_RESULTS,
        )
        self.assertEqual(bounded_scan["count"], 0)
        with self.assertRaisesRegex(ValueError, "input envelope"):
            adapter.parse_input(b"[]")
        with self.assertRaisesRegex(ValueError, "input envelope"):
            adapter.parse_input(b"x" * (adapter.MAX_INPUT_BYTES + 1))
        with self.assertRaises(ValueError):
            self.args(limit="not-an-integer")

    def test_cli_failure_output_never_contains_provider_values(self):
        adapter = self.adapter
        stderr = io.StringIO()
        with mock.patch.dict(os.environ, {"KLIPY_API_KEY": "canary-secret"}, clear=True):
            with mock.patch.object(
                adapter, "perform_search", side_effect=ValueError("canary-secret")
            ):
                with contextlib.redirect_stderr(stderr):
                    result = adapter.main(
                        json.dumps(
                            {"query": "reaction", "limit": "5", "content_filter": "pg"}
                        ).encode()
                    )
        self.assertEqual(result, 1)
        self.assertEqual(json.loads(stderr.getvalue()), {"error": "ValueError"})
        self.assertNotIn("canary-secret", stderr.getvalue())

    def test_governed_kill_outlasts_the_inner_provider_budget(self):
        # This adapter already satisfied the invariant — its 15s provider
        # deadline sits 5s under the 20s governed kill — and the constants now
        # record that arithmetic instead of leaving it implicit. A kill that
        # fired first would turn this adapter's timeout branch into dead code.
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
            json.loads(stderr.getvalue()), {"error": "KLIPY_API_KEY not set"}
        )


if __name__ == "__main__":
    unittest.main()
