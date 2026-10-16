from __future__ import annotations

import importlib.util
from importlib.machinery import SourceFileLoader
import math
import os
import pathlib
import unittest
import urllib.parse
import urllib.error
from datetime import datetime, timedelta, timezone
from unittest import mock


SCRIPT = pathlib.Path(__file__).parents[1] / "bin" / "reddit-search"
LOADER = SourceFileLoader("reddit_search", str(SCRIPT))
SPEC = importlib.util.spec_from_loader("reddit_search", LOADER)
assert SPEC is not None
MODULE = importlib.util.module_from_spec(SPEC)
LOADER.exec_module(MODULE)


# Shaped after a live `search.rss?type=link` entry. Reddit double-escapes the
# post body inside <content>, so the entities survive XML parsing.
FEED = b"""<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>reddit.com: search results - rust async</title>
  <entry>
    <author><name>/u/dogehound</name><uri>https://www.reddit.com/user/dogehound</uri></author>
    <category term="rust" label="r/rust"/>
    <content type="html">&lt;div&gt;We chose Rust &amp;quot;for real&amp;quot;.&lt;/div&gt;
      &amp;#32; submitted by &amp;#32; /u/dogehound &amp;#32; to &amp;#32; r/rust [link] &amp;#32; [comments]</content>
    <id>t3_1vnxyz9</id>
    <link href="https://www.reddit.com/r/rust/comments/1vnxyz9/why_rust/"/>
    <updated>2026-08-13T17:29:07+00:00</updated>
    <title>Why Rust</title>
  </entry>
  <entry>
    <author><name>/u/olderpost</name></author>
    <category term="programming" label="r/programming"/>
    <content type="html">&lt;div&gt;Ancient.&lt;/div&gt;</content>
    <id>t3_oldpost</id>
    <link href="https://www.reddit.com/r/programming/comments/oldpost/ancient/"/>
    <updated>2020-01-01T00:00:00+00:00</updated>
    <title>Ancient thread</title>
  </entry>
</feed>
"""

EMPTY_FEED = b"""<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom"><title>reddit.com: search results</title></feed>
"""

# What the JSON endpoints now serve for every User-Agent: an HTML block page,
# not a feed.
BLOCK_PAGE = b"<body class=theme-beta><div><style>:root{--rem360:22.5rem;}</style></div></body>"


class RedditFeedParsingTests(unittest.TestCase):
    def test_atom_entry_normalizes_to_the_governed_item_shape(self) -> None:
        entry = MODULE.parse_feed(FEED)[0]

        item = MODULE.normalize_atom(entry, "2026-08-14T10:00:00Z")

        self.assertEqual(item["source_native_id"], "1vnxyz9")
        self.assertEqual(item["title"], "Why Rust")
        self.assertEqual(
            item["url"], "https://www.reddit.com/r/rust/comments/1vnxyz9/why_rust/"
        )
        self.assertEqual(item["published_at"], "2026-08-13T17:29:07Z")
        self.assertEqual(item["author"], "dogehound")
        self.assertEqual(item["container"], "rust")

    def test_atom_engagement_is_explicitly_unavailable_not_zeroed(self) -> None:
        """The feed carries no vote data. A zero would read as a real
        measurement of a post nobody upvoted."""
        item = MODULE.normalize_atom(
            MODULE.parse_feed(FEED)[0], "2026-08-14T10:00:00Z"
        )

        self.assertEqual(
            item["engagement"],
            {
                "available": False,
                "reason": "atom_feed_omits_metrics",
                "observed_at": "2026-08-14T10:00:00Z",
            },
        )
        self.assertNotIn("score", item["engagement"])

    def test_body_is_unescaped_and_stripped_of_the_feed_footer(self) -> None:
        item = MODULE.normalize_atom(MODULE.parse_feed(FEED)[0])

        self.assertEqual(item["snippet"], 'We chose Rust "for real".')
        self.assertNotIn("submitted by", item["snippet"])


class RedditRequestTests(unittest.TestCase):
    def test_search_url_requests_links_not_subreddits(self) -> None:
        """Without `type=link` the feed answers with matching subreddits, each
        of which would normalize into something that looks like a post."""
        query = urllib.parse.parse_qs(
            urllib.parse.urlsplit(MODULE.build_url("rust", 30, 10, None)).query
        )

        self.assertEqual(query["type"], ["link"])
        self.assertEqual(query["t"], ["month"])
        self.assertNotIn("restrict_sr", query)

    def test_subreddit_scoping_restricts_to_the_named_subreddit(self) -> None:
        url = MODULE.build_url("async", 7, 5, "rust")
        parsed = urllib.parse.urlsplit(url)

        self.assertEqual(parsed.path, "/r/rust/search.rss")
        self.assertEqual(urllib.parse.parse_qs(parsed.query)["restrict_sr"], ["true"])

    def test_subreddit_is_one_encoded_path_segment(self) -> None:
        parsed = urllib.parse.urlsplit(MODULE.build_url("async", 7, 5, "rust/../all"))

        self.assertEqual(parsed.path, "/r/rust%2F..%2Fall/search.rss")

    def test_lookback_maps_onto_reddits_coarse_windows(self) -> None:
        self.assertEqual(
            [MODULE._coarse_window(days) for days in (1, 7, 30, 365, 4000)],
            ["day", "week", "month", "year", "all"],
        )


class RedditFailureSurfacingTests(unittest.TestCase):
    """Zero items must never be able to mean 'the provider was unreachable'."""

    def test_block_page_raises_rather_than_parsing_as_zero_results(self) -> None:
        with self.assertRaises(MODULE.ProviderUnavailable) as caught:
            MODULE.parse_feed(BLOCK_PAGE)

        self.assertIn("not an Atom feed", str(caught.exception))

    def test_non_atom_xml_raises(self) -> None:
        with self.assertRaises(MODULE.ProviderUnavailable):
            MODULE.parse_feed(b"<rss version='2.0'><channel/></rss>")

    def test_empty_feed_is_a_legitimate_zero_not_an_error(self) -> None:
        self.assertEqual(MODULE.parse_feed(EMPTY_FEED), [])


class RedditOAuthTests(unittest.TestCase):
    def test_oauth_recovery_budget_finishes_before_governed_kill(self) -> None:
        self.assertEqual(MODULE.INNER_WORST_CASE_SECS, 54)
        self.assertGreaterEqual(
            MODULE.GOVERNED_KILL_CEILING_SECS,
            MODULE.INNER_WORST_CASE_SECS + MODULE.GOVERNED_KILL_MARGIN_SECS,
        )

    def test_oauth_json_preserves_real_engagement_metrics(self) -> None:
        item = MODULE.normalize_json_post(
            {
                "id": "1vnxyz9",
                "title": "Why Rust",
                "permalink": "/r/rust/comments/1vnxyz9/why_rust/",
                "selftext": "The post body",
                "created_utc": 1_775_000_123,
                "author": "dogehound",
                "subreddit": "rust",
                "ups": 421,
                "score": 417,
                "num_comments": 83,
                "upvote_ratio": 0.96,
            },
            "2026-08-14T10:00:00Z",
        )

        self.assertEqual(
            item["engagement"],
            {
                "available": True,
                "observed_at": "2026-08-14T10:00:00Z",
                "upvotes": 421,
                "score": 417,
                "comments": 83,
                "upvote_ratio": 0.96,
            },
        )

    def test_malformed_numeric_fields_do_not_fail_the_result_page(self) -> None:
        item = MODULE.normalize_json_post(
            {
                "id": "bad-metrics",
                "created_utc": math.inf,
                "ups": math.inf,
                "score": math.nan,
                "num_comments": "many",
                "upvote_ratio": None,
            },
            "2026-08-14T10:00:00Z",
        )

        self.assertIsNone(item["published_at"])
        self.assertEqual(
            item["engagement"],
            {
                "available": False,
                "reason": "oauth_json_omitted_metrics",
                "observed_at": "2026-08-14T10:00:00Z",
            },
        )

    def test_oauth_token_exchange_uses_basic_auth_without_body_credentials(self) -> None:
        captured = []

        class Response:
            def __enter__(self):
                return self

            def __exit__(self, *_args):
                return False

            def read(self, _limit):
                return b'{"access_token":"short-lived-token"}'

        def open_request(request, timeout):
            captured.append((request, timeout))
            return Response()

        with mock.patch.object(MODULE.urllib.request, "urlopen", side_effect=open_request):
            token = MODULE.oauth_access_token("client-id", "client-secret", 9)

        self.assertEqual(token, "short-lived-token")
        request, timeout = captured[0]
        self.assertEqual(timeout, 9)
        self.assertEqual(request.full_url, MODULE.OAUTH_TOKEN_URL)
        self.assertTrue(request.get_header("Authorization").startswith("Basic "))
        self.assertNotIn(b"client-id", request.data)
        self.assertNotIn(b"client-secret", request.data)

    def test_complete_credential_pair_prefers_metric_bearing_json(self) -> None:
        oauth_item = {"engagement": {"available": True, "score": 9}}
        with mock.patch.dict(
            os.environ,
            {"REDDIT_CLIENT_ID": "client", "REDDIT_CLIENT_SECRET": "secret"},
        ), mock.patch.object(
            MODULE, "fetch_oauth", return_value=([oauth_item], 1)
        ) as oauth, mock.patch.object(MODULE, "fetch_atom") as atom:
            result = MODULE.fetch("rust", 7, 10, None)

        self.assertEqual(result, ([oauth_item], 1, "oauth_json", None))
        oauth.assert_called_once()
        atom.assert_not_called()

    def test_oauth_failure_degrades_to_atom_without_losing_search_results(self) -> None:
        atom_item = {"engagement": {"available": False}}
        failure = urllib.error.HTTPError(
            MODULE.OAUTH_TOKEN_URL, 403, "Forbidden", {}, None
        )
        with mock.patch.dict(
            os.environ,
            {"REDDIT_CLIENT_ID": "client", "REDDIT_CLIENT_SECRET": "secret"},
        ), mock.patch.object(MODULE, "fetch_oauth", side_effect=failure), mock.patch.object(
            MODULE, "fetch_atom", return_value=([atom_item], 1)
        ) as atom:
            result = MODULE.fetch("rust", 7, 10, None)

        self.assertEqual(
            result, ([atom_item], 1, "atom_fallback", "oauth_http_403")
        )
        atom.assert_called_once_with("rust", 7, 10, None, 10, allow_retry=False)

    def test_incomplete_credentials_are_visible_and_never_sent(self) -> None:
        with mock.patch.dict(
            os.environ,
            {"REDDIT_CLIENT_ID": "client", "REDDIT_CLIENT_SECRET": ""},
        ), mock.patch.object(MODULE, "fetch_oauth") as oauth, mock.patch.object(
            MODULE, "fetch_atom", return_value=([], 0)
        ):
            result = MODULE.fetch("rust", 7, 10, None)

        self.assertEqual(
            result, ([], 0, "atom_keyless", "oauth_credentials_incomplete")
        )
        oauth.assert_not_called()


def feed_with(*updated_iso: str) -> bytes:
    entries = "".join(
        f"""
  <entry>
    <author><name>/u/someone</name></author>
    <category term="rust" label="r/rust"/>
    <content type="html">&lt;div&gt;Body.&lt;/div&gt;</content>
    <id>t3_{index}</id>
    <link href="https://www.reddit.com/r/rust/comments/{index}/post/"/>
    <updated>{updated}</updated>
    <title>Post {index}</title>
  </entry>"""
        for index, updated in enumerate(updated_iso)
    )
    return (
        '<?xml version="1.0" encoding="UTF-8"?>'
        '<feed xmlns="http://www.w3.org/2005/Atom">' + entries + "</feed>"
    ).encode()


class RedditWindowFilterTests(unittest.TestCase):
    # Dates are relative to the run so the window assertion cannot expire and
    # start reporting a stale fixture as a filter bug.
    def setUp(self) -> None:
        now = datetime.now(timezone.utc)
        self.feed = feed_with(
            (now - timedelta(days=1)).isoformat(timespec="seconds"),
            (now - timedelta(days=900)).isoformat(timespec="seconds"),
        )

    def test_window_filter_reports_what_the_provider_served(self) -> None:
        """Zero items with a non-zero provider count is a window outcome, and
        the envelope has to be able to say so."""
        original = MODULE.get_atom
        MODULE.get_atom = lambda *_args, **_kwargs: self.feed
        try:
            items, provider_entries = MODULE.fetch_atom("rust", 30, 10, None)
        finally:
            MODULE.get_atom = original

        self.assertEqual(provider_entries, 2)
        self.assertEqual([item["title"] for item in items], ["Post 0"])

    def test_a_window_that_excludes_everything_still_reports_the_provider_count(self) -> None:
        original = MODULE.get_atom
        MODULE.get_atom = lambda *_args, **_kwargs: feed_with(
            (datetime.now(timezone.utc) - timedelta(days=900)).isoformat(timespec="seconds")
        )
        try:
            items, provider_entries = MODULE.fetch_atom("rust", 7, 10, None)
        finally:
            MODULE.get_atom = original

        self.assertEqual((items, provider_entries), ([], 1))


if __name__ == "__main__":
    unittest.main()
