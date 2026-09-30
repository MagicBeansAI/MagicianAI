from __future__ import annotations

import importlib.util
from importlib.machinery import SourceFileLoader
import pathlib
import unittest
from datetime import datetime, timedelta, timezone


SCRIPT = pathlib.Path(__file__).parents[1] / "bin" / "producthunt-search"
LOADER = SourceFileLoader("producthunt_search", str(SCRIPT))
SPEC = importlib.util.spec_from_loader("producthunt_search", LOADER)
assert SPEC is not None
MODULE = importlib.util.module_from_spec(SPEC)
LOADER.exec_module(MODULE)


# Shaped after a live entry: the first paragraph is the tagline and the second
# is the feed's own Discussion/Link footer.
FEED = b"""<?xml version="1.0" encoding="UTF-8"?>
<feed xml:lang="en-US" xmlns="http://www.w3.org/2005/Atom">
  <title>Product Hunt</title>
  <entry>
    <id>tag:www.producthunt.com,2005:Post/1221923</id>
    <published>2026-08-13T00:46:55-07:00</published>
    <updated>2026-08-14T02:36:49-07:00</updated>
    <link rel="alternate" type="text/html" href="https://www.producthunt.com/products/openmotion"/>
    <title>Openmotion</title>
    <content type="html">
      &lt;p&gt;
        Turn product screenshots &amp;amp; prompts into motion videos
      &lt;/p&gt;
      &lt;p&gt;
        &lt;a href="https://www.producthunt.com/products/openmotion"&gt;Discussion&lt;/a&gt;
        |
        &lt;a href="https://www.producthunt.com/r/p/1221923"&gt;Link&lt;/a&gt;
      &lt;/p&gt;
    </content>
    <author><name>Usman Farooq</name></author>
  </entry>
</feed>
"""

EMPTY_FEED = b"""<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom"><title>Product Hunt</title></feed>
"""

# What the v2 GraphQL endpoint now answers for every anonymous request.
GRAPHQL_401 = (
    b'{"data":null,"errors":[{"error":"invalid_oauth_token",'
    b'"error_description":"Please supply a valid access token."}]}'
)


def feed_with(*published_iso: str) -> bytes:
    entries = "".join(
        f"""
  <entry>
    <id>tag:www.producthunt.com,2005:Post/{100 + index}</id>
    <published>{published}</published>
    <link rel="alternate" type="text/html" href="https://www.producthunt.com/products/p{index}"/>
    <title>Product {index}</title>
    <content type="html">&lt;p&gt;An AI video tool&lt;/p&gt;</content>
    <author><name>Maker {index}</name></author>
  </entry>"""
        for index, published in enumerate(published_iso)
    )
    return (
        '<?xml version="1.0" encoding="UTF-8"?>'
        '<feed xmlns="http://www.w3.org/2005/Atom">' + entries + "</feed>"
    ).encode()


class ProductHuntFeedParsingTests(unittest.TestCase):
    def test_atom_entry_normalizes_to_the_governed_item_shape(self) -> None:
        item = MODULE.normalize(MODULE.parse_feed(FEED)[0])

        self.assertEqual(item["source_native_id"], "1221923")
        self.assertEqual(item["title"], "Openmotion")
        self.assertEqual(item["url"], "https://www.producthunt.com/products/openmotion")
        self.assertEqual(item["published_at"], "2026-08-13T07:46:55Z")
        self.assertEqual(item["author"], "Usman Farooq")

    def test_only_the_first_paragraph_is_taken_as_the_tagline(self) -> None:
        """The rest of the body is the feed's Discussion/Link footer, which is
        navigation rather than a product description."""
        item = MODULE.normalize(MODULE.parse_feed(FEED)[0])

        self.assertEqual(item["snippet"], "Turn product screenshots & prompts into motion videos")
        self.assertNotIn("Discussion", item["snippet"])

    def test_absent_provider_fields_are_null_rather_than_invented(self) -> None:
        """The feed has no vote counts and no topic labels. Zeroes here would
        read as a launch nobody upvoted."""
        item = MODULE.normalize(MODULE.parse_feed(FEED)[0])

        self.assertEqual(item["engagement"], {})
        self.assertIsNone(item["container"])


class ProductHuntQueryMatchingTests(unittest.TestCase):
    def test_short_terms_match_whole_words_only(self) -> None:
        """A bare `ai` substring hits `email`, `training`, and `explain`, which
        is how a category filter quietly stops filtering."""
        self.assertTrue(MODULE.matches("ai", "An AI video tool"))
        self.assertFalse(MODULE.matches("ai", "Cold email outreach, explained"))

    def test_long_terms_match_as_substrings(self) -> None:
        self.assertTrue(MODULE.matches("video", "Turn prompts into videos"))

    def test_any_term_matching_is_enough(self) -> None:
        self.assertTrue(MODULE.matches("kubernetes video", "Turn prompts into videos"))

    def test_an_empty_query_matches_everything(self) -> None:
        self.assertTrue(MODULE.matches("", "anything at all"))


class ProductHuntFailureSurfacingTests(unittest.TestCase):
    """Zero items must never be able to mean 'the provider was unreachable'."""

    def test_a_graphql_error_body_raises_rather_than_reading_as_zero_launches(self) -> None:
        with self.assertRaises(MODULE.ProviderUnavailable) as caught:
            MODULE.parse_feed(GRAPHQL_401)

        self.assertIn("not an Atom feed", str(caught.exception))

    def test_non_atom_xml_raises(self) -> None:
        with self.assertRaises(MODULE.ProviderUnavailable):
            MODULE.parse_feed(b"<rss version='2.0'><channel/></rss>")

    def test_empty_feed_is_a_legitimate_zero_not_an_error(self) -> None:
        self.assertEqual(MODULE.parse_feed(EMPTY_FEED), [])


class ProductHuntWindowReportingTests(unittest.TestCase):
    # Dates are relative to the run so these assertions cannot expire and start
    # reporting a stale fixture as a filter bug.
    def setUp(self) -> None:
        self.now = datetime.now(timezone.utc)

    def _fetch(self, feed: bytes, query: str, days: int, limit: int = 10):
        original = MODULE.get
        MODULE.get = lambda *_args, **_kwargs: feed
        try:
            return MODULE.fetch(query, days, limit)
        finally:
            MODULE.get = original

    def test_the_covered_span_is_reported_so_a_wider_days_is_visible(self) -> None:
        feed = feed_with(
            (self.now - timedelta(days=1)).isoformat(timespec="seconds"),
            (self.now - timedelta(days=2)).isoformat(timespec="seconds"),
        )

        items, provider_entries, window = self._fetch(feed, "ai", 90)

        self.assertEqual(len(items), 2)
        self.assertEqual(provider_entries, 2)
        self.assertEqual(window["entries"], 2)
        self.assertIsNotNone(window["oldest"])
        self.assertIsNotNone(window["newest"])

    def test_a_query_that_excludes_everything_still_reports_the_provider_count(self) -> None:
        feed = feed_with((self.now - timedelta(days=1)).isoformat(timespec="seconds"))

        items, provider_entries, window = self._fetch(feed, "kubernetes", 90)

        self.assertEqual(items, [])
        self.assertEqual(provider_entries, 1)
        self.assertEqual(window["entries"], 1)

    def test_a_window_that_excludes_everything_still_reports_the_provider_count(self) -> None:
        feed = feed_with((self.now - timedelta(days=900)).isoformat(timespec="seconds"))

        items, provider_entries, _ = self._fetch(feed, "ai", 7)

        self.assertEqual((items, provider_entries), ([], 1))


if __name__ == "__main__":
    unittest.main()
