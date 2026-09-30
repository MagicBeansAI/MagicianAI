import importlib.util
from importlib.machinery import SourceFileLoader
import pathlib
import unittest
from datetime import date


SCRIPT = pathlib.Path(__file__).parents[1] / "bin" / "github-search"
LOADER = SourceFileLoader("github_search", str(SCRIPT))
SPEC = importlib.util.spec_from_loader("github_search", LOADER)
MODULE = importlib.util.module_from_spec(SPEC)
LOADER.exec_module(MODULE)


class GitHubSearchAdapterTest(unittest.TestCase):
    def test_issue_projection_bounds_content_and_rejects_unsafe_urls(self):
        original = MODULE.get_json
        MODULE.get_json = lambda *_: {"items": [{
            "id": 7,
            "title": "issue",
            "html_url": "file:///secret",
            "body": "x" * 2_000,
            "created_at": "2026-08-08T00:00:00Z",
            "repository_url": "https://api.github.com/repos/acme/demo",
            "comments": 2,
            "reactions": {"total_count": 3},
            "user": {"login": "owner"},
        }]}
        try:
            items = MODULE.fetch_issues("demo", date(2026, 8, 1), date(2026, 8, 8), 1, {})
        finally:
            MODULE.get_json = original
        self.assertEqual(items[0]["url"], "")
        self.assertEqual(len(items[0]["snippet"]), 1_200)
        self.assertEqual(items[0]["container"], "acme/demo")
        self.assertEqual(items[0]["engagement"], {"comments": 2, "reactions_total": 3})

    def test_every_issue_search_names_the_record_kind_github_now_demands(self):
        """GitHub refuses an unqualified issue search.

            422 {"message": "Query must include 'is:issue' or 'is:pull-request'"}

        It refuses the AUTHENTICATED request first, which is why this skill
        worked keyless and broke the moment a valid token was injected — the
        shape of a credential problem over something that is not one. Nothing
        about the token was wrong; the query was.
        """
        queries = []
        original = MODULE.get_json

        def capture(url, _headers):
            queries.append(url)
            return {"items": []}

        MODULE.get_json = capture
        try:
            MODULE.fetch_issues("demo", date(2026, 8, 1), date(2026, 8, 8), 5, {})
        finally:
            MODULE.get_json = original
        self.assertEqual(len(queries), 2, "both record kinds must be searched")
        self.assertTrue(any("is%3Aissue" in url for url in queries))
        self.assertTrue(any("is%3Apull-request" in url for url in queries))

    def test_a_caller_that_pinned_a_kind_gets_that_search_and_no_other(self):
        # Appending a second qualifier to a caller's own `is:` would override
        # its choice rather than widen it — and GitHub keeps only the last
        # qualifier when both appear, so the override would be silent.
        queries = []
        original = MODULE.get_json

        def capture(url, _headers):
            queries.append(url)
            return {"items": []}

        MODULE.get_json = capture
        try:
            MODULE.fetch_issues(
                "demo is:pull-request", date(2026, 8, 1), date(2026, 8, 8), 5, {}
            )
        finally:
            MODULE.get_json = original
        self.assertEqual(len(queries), 1)
        self.assertNotIn("is%3Aissue", queries[0])

    def test_the_two_searches_are_reordered_rather_than_concatenated(self):
        # Each kind arrives already sorted `updated desc`. Concatenating two
        # descending runs is not a descending run, so truncating to `limit`
        # would drop the newest pull requests behind the oldest issues.
        pages = [
            {"items": [
                {"id": 1, "title": "old issue", "updated_at": "2026-08-01T00:00:00Z"},
                {"id": 2, "title": "older issue", "updated_at": "2026-07-01T00:00:00Z"},
            ]},
            {"items": [
                {"id": 3, "title": "new pr", "updated_at": "2026-08-08T00:00:00Z"},
            ]},
        ]
        original = MODULE.get_json
        MODULE.get_json = lambda *_: pages.pop(0)
        try:
            items = MODULE.fetch_issues("demo", date(2026, 8, 1), date(2026, 8, 8), 2, {})
        finally:
            MODULE.get_json = original
        self.assertEqual([item["title"] for item in items], ["new pr", "old issue"])

    def test_a_record_returned_by_both_searches_appears_once(self):
        pages = [
            {"items": [{"id": 9, "title": "same", "updated_at": "2026-08-08T00:00:00Z"}]},
            {"items": [{"id": 9, "title": "same", "updated_at": "2026-08-08T00:00:00Z"}]},
        ]
        original = MODULE.get_json
        MODULE.get_json = lambda *_: pages.pop(0)
        try:
            items = MODULE.fetch_issues("demo", date(2026, 8, 1), date(2026, 8, 8), 5, {})
        finally:
            MODULE.get_json = original
        self.assertEqual(len(items), 1)

    def test_repo_projection_filters_results_outside_window(self):
        original = MODULE.get_json
        MODULE.get_json = lambda *_: {"items": [
            {"id": 1, "full_name": "old/repo", "pushed_at": "2025-01-01T00:00:00Z"},
            {"id": 2, "full_name": "new/repo", "pushed_at": "2026-08-07T00:00:00Z"},
        ]}
        try:
            items = MODULE.fetch_repos("demo", date(2026, 8, 1), date(2026, 8, 8), 2, {})
        finally:
            MODULE.get_json = original
        self.assertEqual([item["title"] for item in items], ["new/repo"])


if __name__ == "__main__":
    unittest.main()
