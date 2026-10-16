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


BIN = Path(__file__).resolve().parents[1] / "bin" / "tavily-search"
SKILL = Path(__file__).resolve().parents[1] / "SKILL.md"
GOVERNED_ENVELOPE = {
    "query": "bounded",
    "search_depth": "basic",
    "topic": "general",
    "max_results": 5,
    "include_raw_content": "false",
    "include_answer": "false",
    "auto_parameters": False,
    "exact_match": False,
}


def load_adapter():
    loader = importlib.machinery.SourceFileLoader("tavily_search_adapter", str(BIN))
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


class TavilySearchAdapterTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.adapter = load_adapter()

    def args(self, **overrides):
        values = {
            "query": "bounded",
            "search_depth": "basic",
            "topic": "general",
            "max_results": 5,
            "include_raw_content": "false",
            "include_answer": "false",
            "auto_parameters": False,
            "exact_match": False,
        }
        values.update(overrides)
        return self.adapter.parse_input(json.dumps(values).encode())

    def test_omitted_optional_parameters_arrive_as_their_declared_defaults(self):
        # The governed runtime materializes a declared `default:` into the
        # envelope, so a parameter is absent only when the caller omits it AND
        # the manifest declares no default. Each such parameter must still
        # materialize as the adapter's own fallback:
        # `request_body` and `normalize_response` read `search_depth`, `topic`,
        # `max_results`, `include_raw_content`, `include_answer`,
        # `auto_parameters`, and `exact_match` off the namespace unguarded, so
        # an omitted one used to raise AttributeError, which surfaced to the
        # caller as an opaque one-word error with no usable search result.
        args = self.adapter.parse_input(b'{"query":"x"}')

        # Exact equality: the declared defaults are precisely the values that
        # keep every optional provider field out of the request.
        self.assertEqual(
            self.adapter.request_body("canary-secret", args),
            {
                "api_key": "canary-secret",
                "query": "x",
                "search_depth": "basic",
                "topic": "general",
                "max_results": 5,
            },
        )

        # `normalize_response` reads the namespace too: its result cap is the
        # defaulted `max_results` and its cost tier the defaulted `search_depth`.
        output = self.adapter.normalize_response(
            {"results": [{"title": str(n), "url": f"https://{n}.example"} for n in range(9)]},
            args,
        )
        self.assertEqual(output["cost_microunits"], 1_000_000)
        self.assertEqual(len(output["results"]), 5)

    def test_builds_bounded_provider_request_without_putting_key_in_url_or_headers(self):
        args = self.args(
            query="latest launch",
            search_depth="advanced",
            topic="news",
            include_answer="advanced",
            include_raw_content="markdown",
            include_domains="example.com, news.example",
            auto_parameters=True,
        )
        captured = {}

        def opener(request, timeout):
            captured["request"] = request
            captured["timeout"] = timeout
            return FakeResponse(
                {
                    "query": "latest launch",
                    "answer": "Summary",
                    "results": [
                        {
                            "title": "Launch",
                            "url": "https://example.com/launch",
                            "content": "Details",
                            "raw_content": "x" * 13000,
                            "score": 0.9,
                        }
                    ],
                }
            )

        output = self.adapter.perform_search("canary-secret", args, opener=opener)

        request = captured["request"]
        body = json.loads(request.data)
        self.assertEqual(request.full_url, "https://api.tavily.com/search")
        self.assertNotIn("canary-secret", request.full_url)
        self.assertNotIn("canary-secret", json.dumps(dict(request.header_items())))
        self.assertEqual(body["api_key"], "canary-secret")
        self.assertEqual(body["include_domains"], ["example.com", "news.example"])
        self.assertTrue(body["auto_parameters"])
        self.assertEqual(captured["timeout"], 30)
        self.assertEqual(output["cost_microunits"], 2_000_000)
        self.assertEqual(len(output["results"][0]["raw_content"]), 12000)

    def test_rejects_out_of_contract_provider_response_size(self):
        args = self.args()

        class OversizedResponse(FakeResponse):
            def read(self, _limit: int) -> bytes:
                return b"x" * (self_adapter.MAX_RESPONSE_BYTES + 1)

        self_adapter = self.adapter
        with self.assertRaisesRegex(ValueError, "size limit"):
            self.adapter.perform_search(
                "canary-secret",
                args,
                opener=lambda *_args, **_kwargs: OversizedResponse({}),
            )

    def test_normalization_caps_result_count_and_ignores_non_object_rows(self):
        args = self.args(max_results=1)
        output = self.adapter.normalize_response(
            {
                "results": [
                    {"title": "one", "url": "https://one.example"},
                    {"title": "two", "url": "https://two.example"},
                    "invalid",
                ]
            },
            args,
        )
        self.assertEqual([row["title"] for row in output["results"]], ["one"])

    def test_redirects_and_unbounded_or_non_finite_fields_fail_closed(self):
        handler = self.adapter.NoRedirectHandler()
        self.assertIsNone(handler.redirect_request(None, None, None, None, None, None))

        args = self.args()

        with self.assertRaisesRegex(ValueError, "input envelope"):
            self.adapter.parse_input(b"[]")
        with self.assertRaisesRegex(ValueError, "input envelope"):
            self.adapter.parse_input(b"x" * (self.adapter.MAX_INPUT_BYTES + 1))
        output = self.adapter.normalize_response(
            {
                "answer": "a" * 13_000,
                "results": [
                    {
                        "title": "t" * 5_000,
                        "url": "https://one.example/" + "u" * 9_000,
                        "content": "c" * 13_000,
                        "score": float("nan"),
                    }
                ],
            },
            args,
        )
        self.assertEqual(len(output["answer"]), 12_000)
        self.assertEqual(len(output["results"][0]["title"]), 4_096)
        self.assertEqual(len(output["results"][0]["url"]), 8_192)
        self.assertEqual(len(output["results"][0]["content"]), 12_000)
        self.assertNotIn("score", output["results"][0])

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
            json.loads(stderr.getvalue()), {"error": "TAVILY_API_KEY not set"}
        )

    def test_failures_never_expose_the_credential_on_any_stream(self):
        # Tavily is the one adapter that puts its key in the request *body*, so
        # a provider error echoing the body back is a live leak path.
        adapter = self.adapter
        for failure in (
            ValueError("canary-tavily-secret"),
            adapter.urllib.error.URLError("canary-tavily-secret"),
            TimeoutError("canary-tavily-secret"),
        ):
            stdout = io.StringIO()
            stderr = io.StringIO()
            with mock.patch.dict(
                os.environ, {"TAVILY_API_KEY": "canary-tavily-secret"}, clear=True
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
            self.assertNotIn("canary-tavily-secret", stdout.getvalue())
            self.assertNotIn("canary-tavily-secret", stderr.getvalue())

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
        with mock.patch.dict(os.environ, {"TAVILY_API_KEY": "unused-key"}, clear=True):
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
        with mock.patch.dict(os.environ, {"TAVILY_API_KEY": "unused-key"}, clear=True):
            with mock.patch.object(
                adapter, "perform_search", side_effect=ValueError("x" * 4_000)
            ):
                with contextlib.redirect_stderr(stderr):
                    adapter.main(json.dumps(GOVERNED_ENVELOPE).encode())
        emitted = json.loads(stderr.getvalue())
        self.assertEqual(len(emitted["error"]["message"]), 512)


class ReportedCostCommodityTests(unittest.TestCase):
    """The number this adapter reports is credits, and every reader must know.

    Tavily's response carries no cost field and no cost header — verified
    against the live endpoint — and the dollar value of a credit belongs to the
    operator's plan rather than to the call. So the only quantity this adapter
    can honestly report is the one the provider's pricing is denominated in:
    one credit for a basic search, two for an advanced one, expressed as
    microunits of a credit.

    That is correct and it is also a trap for anything that reads the number
    without its commodity. The canary lane did exactly that: it carried exa's
    ceiling of 20000, written in USD microunits, and read one credit as a
    dollar of spend — roughly a hundredfold overstatement of a real cost near
    $0.008. The declaration below is what makes the number legible; these
    assertions keep the two sides pinned together.
    """

    def setUp(self):
        self.frontmatter = SKILL.read_text(encoding="utf-8").split("\n---\n", 1)[0]

    def test_the_package_declares_the_commodity_it_prices_in(self):
        self.assertIn("pointer: /cost_microunits", self.frontmatter)
        self.assertIn("commodity: tavily_credit", self.frontmatter)
        self.assertIn("encoding: microunits", self.frontmatter)

    def test_the_canary_ceiling_names_the_same_commodity_the_package_reports(self):
        declared = re.findall(
            r"^ *max_cost_commodity: (\S+)$", self.frontmatter, re.MULTILINE
        )
        self.assertEqual(
            declared,
            ["tavily_credit"],
            "the ceiling must be written in the commodity this package reports; "
            "a USD ceiling here compares unlike things",
        )

    def test_the_ceiling_admits_a_basic_search_and_refuses_a_silent_upgrade(self):
        ceiling = re.findall(
            r"^ *max_cost_microunits: (\d+)$", self.frontmatter, re.MULTILINE
        )
        self.assertEqual(len(ceiling), 1)
        ceiling = int(ceiling[0])
        adapter = load_adapter()
        basic = adapter.normalize_response(
            {"query": "q", "results": []},
            type("Args", (), {"search_depth": "basic", "max_results": 1, "query": "q"}),
        )["cost_microunits"]
        advanced = adapter.normalize_response(
            {"query": "q", "results": []},
            type(
                "Args", (), {"search_depth": "advanced", "max_results": 1, "query": "q"}
            ),
        )["cost_microunits"]
        self.assertLessEqual(basic, ceiling, "the declared canary call must fit")
        self.assertGreater(
            advanced,
            ceiling,
            "the ceiling must still catch the probe drifting to the depth that "
            "costs double",
        )


if __name__ == "__main__":
    unittest.main()
