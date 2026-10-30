from __future__ import annotations

import importlib.util
from importlib.machinery import SourceFileLoader
import io
import json
from pathlib import Path
import ssl
import unittest
from unittest.mock import patch
from urllib.parse import parse_qs, urlsplit


SCRIPT = Path(__file__).parents[1] / "bin/polymarket-search"
LOADER = SourceFileLoader("polymarket_search", str(SCRIPT))
SPEC = importlib.util.spec_from_loader(LOADER.name, LOADER)
MODULE = importlib.util.module_from_spec(SPEC)
LOADER.exec_module(MODULE)


def response(events, more=False):
    return io.BytesIO(json.dumps({
        "events": events, "pagination": {"hasMore": more},
    }).encode())


def event(*markets):
    return {"id": "event-1", "slug": "launch-date", "active": True,
            "description": "Provider search matched this event.", "markets": list(markets)}


def market(identity, **extra):
    return {"id": identity, "question": f"Before December {identity}?",
            "slug": f"specific-market-{identity}", "active": True, "closed": False,
            "volume": "123.5", "liquidity": "7", **extra}


class PolymarketSearchTests(unittest.TestCase):
    def test_searches_the_query_and_pages_nested_markets_without_global_keyword_filter(self):
        pages = [response([event(market("1"), market("closed", closed=True))], True),
                 response([event(market("1"), market("2"))])]
        with patch.object(MODULE.urllib.request, "urlopen", side_effect=pages) as opened:
            items = MODULE.fetch("rocket launch", 2)
        self.assertEqual([x["source_native_id"] for x in items], ["1", "2"])
        for page, call in enumerate(opened.call_args_list, 1):
            url = urlsplit(call.args[0].full_url)
            self.assertEqual((url.netloc, url.path), ("gamma-api.polymarket.com", "/public-search"))
            query = parse_qs(url.query)
            self.assertEqual(query["q"], ["rocket launch"])
            self.assertEqual(query["page"], [str(page)])
            self.assertEqual(query["events_status"], ["active"])
            self.assertLessEqual(call.kwargs["timeout"], 10)
        self.assertEqual(items[0]["url"], "https://polymarket.com/event/launch-date")
        self.assertEqual(items[0]["engagement"], {"volume": 123.5, "liquidity": 7.0})

    def test_excludes_inactive_archived_and_closed_markets_and_events(self):
        data = [event(market("inactive", active=False), market("archived", archived=True),
                      market("closed", closed=True), market("open")),
                {**event(market("old")), "closed": True},
                {**event(market("archive")), "archived": True}]
        with patch.object(MODULE.urllib.request, "urlopen", return_value=response(data)):
            items = MODULE.fetch("query", 10)
        self.assertEqual([x["source_native_id"] for x in items], ["open"])

    def test_empty_search_succeeds_and_does_not_follow_empty_pages(self):
        with patch.object(MODULE.urllib.request, "urlopen", return_value=response([], True)) as opened:
            self.assertEqual(MODULE.fetch("no matches", 10), [])
        self.assertEqual(opened.call_count, 1)

    def test_limits_results_even_with_many_markets_in_one_event(self):
        with patch.object(MODULE.urllib.request, "urlopen", return_value=response([
            event(*(market(str(i)) for i in range(12)))
        ])):
            self.assertEqual(len(MODULE.fetch("query", 3)), 3)

    def test_bounds_pagination_when_provider_keeps_returning_duplicates(self):
        with patch.object(MODULE.urllib.request, "urlopen", side_effect=lambda *a, **k: response([
            event(market("1"))
        ], True)) as opened:
            self.assertEqual(len(MODULE.fetch("query", 10)), 1)
        self.assertEqual(opened.call_count, 10)

    def test_bad_numeric_metadata_does_not_drop_an_otherwise_valid_market(self):
        with patch.object(MODULE.urllib.request, "urlopen", return_value=response([
            event(market("1", volume="NaN", liquidity="unknown"))
        ])):
            item = MODULE.fetch("query", 1)[0]
        self.assertEqual(item["engagement"], {"volume": 0.0, "liquidity": 0.0})

    def test_invalid_query_or_limit_does_not_contact_provider(self):
        with patch.object(MODULE.urllib.request, "urlopen") as opened:
            for query, limit in [(" ", 1), ("query", 0), ("query", 101)]:
                with self.assertRaises(ValueError):
                    MODULE.fetch(query, limit)
        opened.assert_not_called()

    def test_response_size_and_total_time_are_bounded(self):
        with self.assertRaisesRegex(ValueError, "size limit"):
            MODULE._read_json_response(io.BytesIO(b"x" * (MODULE.MAX_RESPONSE_BYTES + 1)))
        with patch.object(MODULE.time, "monotonic", side_effect=[0, 26]), \
                patch.object(MODULE.urllib.request, "urlopen") as opened:
            with self.assertRaises(TimeoutError):
                MODULE.fetch("query", 10)
        opened.assert_not_called()

    def test_connection_failure_is_reported_as_failure_not_an_empty_success(self):
        output = io.StringIO()
        with patch.object(MODULE.sys, "stdin", io.StringIO('{"query":"election"}')), \
                patch.object(MODULE.sys, "stdout", output), \
                patch.object(MODULE.sys, "stderr", io.StringIO()), \
                patch.object(MODULE.urllib.request, "urlopen", side_effect=ConnectionResetError("reset")), \
                patch.object(MODULE.urllib.request, "build_opener") as build:
            build.return_value.open.side_effect = ConnectionResetError("reset")
            self.assertEqual(MODULE.main(), 0)
        envelope = json.loads(output.getvalue())
        self.assertEqual(envelope["status"], "failed")
        self.assertEqual(envelope["items"], [])
        self.assertIn("ConnectionResetError", envelope["reason"])

    def test_connection_reset_retries_once_over_ipv4(self):
        reset = MODULE.urllib.error.URLError(ConnectionResetError("reset"))
        with patch.object(MODULE.urllib.request, "urlopen", side_effect=reset) as standard, \
                patch.object(MODULE.urllib.request, "build_opener") as build:
            build.return_value.open.return_value = response([event(market("1"))])
            self.assertEqual(len(MODULE.fetch("query", 1)), 1)
        self.assertIsInstance(build.call_args.args[0], MODULE._IPv4HTTPSHandler)
        self.assertEqual(standard.call_count, 1)
        self.assertEqual(build.return_value.open.call_count, 1)

    def test_bad_certificates_and_provider_errors_do_not_retry(self):
        errors = [MODULE.urllib.error.URLError(ssl.SSLCertVerificationError("invalid certificate")),
                  MODULE.urllib.error.HTTPError("https://gamma-api.polymarket.com", 403, "Forbidden", {}, None)]
        for error in errors:
            with self.subTest(error=type(error).__name__), \
                    patch.object(MODULE.urllib.request, "urlopen", side_effect=error), \
                    patch.object(MODULE.urllib.request, "build_opener") as build:
                with self.assertRaises(MODULE.urllib.error.URLError):
                    MODULE.fetch("query", 1)
                build.assert_not_called()

    def test_ipv4_transport_retains_hostname_and_certificate_checks(self):
        connection = MODULE._IPv4HTTPSConnection("gamma-api.polymarket.com")
        self.assertEqual(connection._context.verify_mode, ssl.CERT_REQUIRED)
        self.assertTrue(connection._context.check_hostname)
        with patch.object(MODULE, "_ipv4_connection") as connect, \
                patch.object(connection._context, "wrap_socket") as tls:
            connection.connect()
        self.assertEqual(connect.call_args.args[0], ("gamma-api.polymarket.com", 443))
        self.assertEqual(tls.call_args.kwargs["server_hostname"], "gamma-api.polymarket.com")


if __name__ == "__main__":
    unittest.main()
