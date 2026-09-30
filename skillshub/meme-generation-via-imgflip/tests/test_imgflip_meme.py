from __future__ import annotations

import contextlib
import importlib.machinery
import importlib.util
import io
import json
import os
import re
import unittest
import urllib.parse
from pathlib import Path
from unittest import mock


BIN = Path(__file__).resolve().parents[1] / "bin" / "imgflip-meme"
SKILL = Path(__file__).resolve().parents[1] / "SKILL.md"
GOVERNED_ENVELOPE = {"action": "caption", "text0": "caption", "text1": ""}


def load_adapter():
    loader = importlib.machinery.SourceFileLoader("imgflip_meme_adapter", str(BIN))
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


class ImgflipMemeAdapterTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.adapter = load_adapter()

    def args(self, **overrides):
        values = {"action": "caption", "text1": ""}
        values.update(overrides)
        return self.adapter.parse_input(json.dumps(values).encode())

    def test_lists_bounded_templates_without_sending_credentials(self):
        args = self.args(action="list_templates")
        captured = []

        def opener(request, timeout):
            captured.append((request, timeout))
            return FakeResponse(
                {
                    "success": True,
                    "data": {
                        "memes": [
                            {
                                "id": "61579",
                                "name": "One Does Not Simply",
                                "url": "https://i.imgflip.com/1bij.jpg",
                                "box_count": 2,
                            },
                            {
                                "id": "unsafe",
                                "name": "Unsafe",
                                "url": "https://user:secret@imgflip.com/reject.jpg",
                                "box_count": True,
                            },
                        ]
                    },
                }
            )

        output = self.adapter.perform_action(
            "canary-user", "canary-password", args, opener=opener
        )

        request, timeout = captured[0]
        self.assertEqual(request.full_url, "https://api.imgflip.com/get_memes")
        self.assertEqual(request.method, "GET")
        self.assertIsNone(request.data)
        self.assertNotIn("canary-user", request.full_url)
        self.assertNotIn("canary-password", str(request.header_items()))
        self.assertEqual(timeout, self.adapter.CATALOG_TIMEOUT_SECS)
        self.assertEqual(output["count"], 2)
        self.assertEqual(output["templates"][0]["id"], "61579")
        self.assertEqual(output["templates"][1]["url"], "")
        self.assertEqual(output["templates"][1]["box_count"], 2)

    def test_posts_both_credentials_only_in_caption_form_body(self):
        args = self.args(
            template_id="181913649",
            text0="old way",
            text1="new way",
        )
        captured = []

        def opener(request, timeout):
            captured.append((request, timeout))
            return FakeResponse(
                {
                    "success": True,
                    "data": {
                        "url": "https://i.imgflip.com/result.jpg",
                        "page_url": "https://imgflip.com/i/result",
                    },
                }
            )

        output = self.adapter.perform_action(
            "canary-user", "canary-password", args, opener=opener
        )

        request, timeout = captured[0]
        form = urllib.parse.parse_qs(request.data.decode("utf-8"))
        self.assertEqual(request.full_url, "https://api.imgflip.com/caption_image")
        self.assertEqual(request.method, "POST")
        self.assertEqual(form["username"], ["canary-user"])
        self.assertEqual(form["password"], ["canary-password"])
        self.assertEqual(form["template_id"], ["181913649"])
        self.assertNotIn("canary-user", request.full_url)
        self.assertNotIn("canary-password", str(request.header_items()))
        self.assertEqual(timeout, self.adapter.CAPTION_TIMEOUT_SECS)
        self.assertEqual(output["url"], "https://i.imgflip.com/result.jpg")
        self.assertEqual(output["text0"], "old way")

    def test_fuzzy_template_resolution_fallback_and_suggestions_are_bounded(self):
        adapter = self.adapter
        fallback_args = self.args(template_name="Drake", text0="bad", text1="good")
        requests = []

        def fallback_opener(request, timeout):
            requests.append(request.full_url)
            if request.full_url.endswith("/get_memes"):
                raise adapter.urllib.error.URLError("catalog unavailable")
            return FakeResponse(
                {
                    "success": True,
                    "data": {
                        "url": "https://i.imgflip.com/drake.jpg",
                        "page_url": "https://imgflip.com/i/drake",
                    },
                }
            )

        output = adapter.perform_action(
            "user", "password", fallback_args, opener=fallback_opener
        )
        self.assertEqual(output["template_id"], "181913649")
        self.assertEqual(len(requests), 2)

        missing_args = self.args(template_name="definitely absent", text0="caption")
        with self.assertRaises(adapter.TemplateNotFoundError) as raised:
            adapter.perform_action(
                "user",
                "password",
                missing_args,
                opener=lambda *_args, **_kwargs: FakeResponse(
                    {"success": True, "data": {"memes": adapter.fallback_templates()}}
                ),
            )
        self.assertLessEqual(len(raised.exception.suggestions), adapter.MAX_SUGGESTIONS)
        self.assertEqual(
            set(raised.exception.suggestions[0]), {"id", "name"}
        )

    def test_redirect_response_url_and_scan_authority_fail_closed(self):
        adapter = self.adapter
        handler = adapter.NoRedirectHandler()
        self.assertIsNone(handler.redirect_request(None, None, None, None, None, None))
        self.assertEqual(adapter.safe_url("javascript:unsafe"), "")
        self.assertEqual(adapter.safe_url("https://u:p@example.com/x.jpg"), "")
        values = [{} for _ in range(adapter.MAX_INSPECTED_TEMPLATES)] + [
            {"id": "late", "name": "Too Late", "url": "https://example.com/x.jpg"}
        ]
        self.assertEqual(adapter.normalize_templates(values), [])

        args = self.args(template_id="1", text0="caption")
        with self.assertRaisesRegex(ValueError, "provider error"):
            adapter.perform_action(
                "user",
                "password",
                args,
                opener=lambda *_args, **_kwargs: FakeResponse(
                    {"success": False, "error_message": "canary-password"}
                ),
            )

        class OversizedResponse(FakeResponse):
            def read(self, _limit: int) -> bytes:
                return b"x" * (adapter.MAX_RESPONSE_BYTES + 1)

        with self.assertRaisesRegex(ValueError, "size limit"):
            adapter.perform_action(
                "user",
                "password",
                args,
                opener=lambda *_args, **_kwargs: OversizedResponse({}),
            )

    def test_governed_envelope_and_failures_never_expose_secret_values(self):
        adapter = self.adapter
        with self.assertRaisesRegex(ValueError, "input envelope"):
            adapter.parse_input(b"[]")
        with self.assertRaisesRegex(ValueError, "input envelope"):
            adapter.parse_input(b"x" * (adapter.MAX_INPUT_BYTES + 1))
        stderr = io.StringIO()
        with mock.patch.dict(
            os.environ,
            {"IMGFLIP_USERNAME": "canary-user", "IMGFLIP_PASSWORD": "canary-password"},
            clear=True,
        ):
            with mock.patch.object(
                adapter, "perform_action", side_effect=ValueError("canary-password")
            ):
                with contextlib.redirect_stderr(stderr):
                    result = adapter.main(
                        json.dumps(
                            {"action": "caption", "text0": "caption", "text1": ""}
                        ).encode()
                    )
        self.assertEqual(result, 1)
        self.assertEqual(json.loads(stderr.getvalue()), {"error": "ValueError"})
        self.assertNotIn("canary-user", stderr.getvalue())
        self.assertNotIn("canary-password", stderr.getvalue())


    def test_template_not_found_reports_on_stderr_like_every_other_failure(self):
        # The governed runtime parses stderr when the exit code is non-zero and
        # ignores stdout entirely, and `parse_capability_failure` also reads
        # only stderr. This branch printed to stdout, so the one error carrying
        # actionable suggestions was the only one yielding no `parsed_json` and
        # the only one that could not be recognised as a structured failure.
        #
        # Asserted through `main`, not `perform_action`: the stream is chosen in
        # `main`, so a test that stops at the raise cannot see this at all —
        # which is why it shipped.
        adapter = self.adapter
        suggestions = [{"id": "181913649", "name": "Drake Hotline Bling"}]
        stdout = io.StringIO()
        stderr = io.StringIO()
        with mock.patch.dict(
            os.environ,
            {"IMGFLIP_USERNAME": "user", "IMGFLIP_PASSWORD": "password"},
            clear=True,
        ):
            with mock.patch.object(
                adapter,
                "perform_action",
                side_effect=adapter.TemplateNotFoundError(suggestions),
            ):
                with contextlib.redirect_stdout(stdout):
                    with contextlib.redirect_stderr(stderr):
                        result = adapter.main(
                            json.dumps(
                                {
                                    "action": "caption",
                                    "template_name": "definitely absent",
                                    "text0": "caption",
                                    "text1": "",
                                }
                            ).encode()
                        )
        self.assertEqual(result, 1)
        self.assertEqual(stdout.getvalue(), "", "a failure must write nothing to stdout")
        self.assertEqual(
            json.loads(stderr.getvalue()),
            {"error": "No matching Imgflip template", "suggestions": suggestions},
        )

    def test_governed_kill_outlasts_the_inner_provider_budget(self):
        # A caption resolved by template name runs the catalog request and the
        # caption request in sequence: 10 + 15 = 25s of provider deadline under
        # what used to be a 20s governed kill. The kill therefore always fired
        # first, which made the built-in template fallback and every described
        # caption failure unreachable. SKILL.md is YAML the adapter must not
        # read at execution time, so it mirrors the ceiling and this test is
        # what actually holds the two files together.
        adapter = self.adapter
        self.assertEqual(
            adapter.INNER_WORST_CASE_SECS,
            adapter.CATALOG_TIMEOUT_SECS + adapter.CAPTION_TIMEOUT_SECS,
            "the declared inner budget no longer matches the two-request path",
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
        # Both halves are required, so each half alone must still fail closed.
        for environment in (
            {},
            {"IMGFLIP_USERNAME": "canary-user"},
            {"IMGFLIP_PASSWORD": "canary-password"},
        ):
            stdout = io.StringIO()
            stderr = io.StringIO()
            with mock.patch.dict(os.environ, environment, clear=True):
                with contextlib.redirect_stdout(stdout):
                    with contextlib.redirect_stderr(stderr):
                        result = adapter.main(json.dumps(GOVERNED_ENVELOPE).encode())
            self.assertEqual(result, 2)
            self.assertEqual(
                stdout.getvalue(), "", "a failure must write nothing to stdout"
            )
            self.assertEqual(
                json.loads(stderr.getvalue()),
                {"error": "IMGFLIP_USERNAME and IMGFLIP_PASSWORD not set"},
            )
            self.assertNotIn("canary-user", stderr.getvalue())
            self.assertNotIn("canary-password", stderr.getvalue())


if __name__ == "__main__":
    unittest.main()
