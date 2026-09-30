from __future__ import annotations

import importlib.util
from importlib.machinery import SourceFileLoader
import pathlib
import unittest


SCRIPT = pathlib.Path(__file__).parents[1] / "bin" / "websearch"
LOADER = SourceFileLoader("websearch", str(SCRIPT))
SPEC = importlib.util.spec_from_loader("websearch", LOADER)
assert SPEC is not None
MODULE = importlib.util.module_from_spec(SPEC)
LOADER.exec_module(MODULE)


def result_block(target: str, title: str, snippet: str | None) -> str:
    href = f"//duckduckgo.com/l/?uddg={target}&amp;rut=deadbeef"
    body = f"""
      <div class="result results_links results_links_deep web-result ">
        <div class="links_main links_deep result__body">
          <h2 class="result__title">
            <a rel="nofollow" class="result__a" href="{href}">{title}</a>
          </h2>
          <div class="result__extras">
            <a class="result__url" href="{href}">example.com</a>
          </div>
    """
    if snippet is not None:
        body += f'<a class="result__snippet" href="{href}">{snippet}</a>'
    return body + "</div></div>"


def results_page(*blocks: str) -> str:
    return (
        '<html><body><div class="serp__results"><div id="links" class="results">'
        + "".join(blocks)
        + "</div></div></body></html>"
    )


# The live challenge is an HTTP 202 page whose only distinguishing content is
# the anomaly modal; it carries no results container at all.
CHALLENGE_PAGE = """
<html><body>
  <form id="challenge-form" action="//duckduckgo.com/anomaly.js?sv=html" method="POST">
    <div class="anomaly-modal__mask"><div class="anomaly-modal__modal">
      <div class="anomaly-modal__title">Unfortunately, bots use DuckDuckGo too.</div>
    </div></div>
  </form>
</body></html>
"""


class WebsearchParsingTests(unittest.TestCase):
    def test_redirect_wrapper_is_unwrapped_into_the_destination(self) -> None:
        page = results_page(
            result_block("https%3A%2F%2Frust-lang.org%2F", "Rust", "A language.")
        )

        results = MODULE.parse_results(page, 10)

        self.assertEqual(len(results), 1)
        self.assertEqual(results[0]["url"], "https://rust-lang.org/")
        self.assertEqual(results[0]["title"], "Rust")
        self.assertEqual(results[0]["snippet"], "A language.")

    def test_result_without_a_snippet_is_still_returned(self) -> None:
        """The single ordered regex this replaced required a title AND a
        snippet in that order, and dropped the whole result otherwise."""
        page = results_page(
            result_block("https%3A%2F%2Fa.example%2F", "No snippet here", None),
            result_block("https%3A%2F%2Fb.example%2F", "Has one", "Body text."),
        )

        results = MODULE.parse_results(page, 10)

        self.assertEqual([item["title"] for item in results], ["No snippet here", "Has one"])
        self.assertEqual(results[0]["snippet"], "")
        self.assertEqual(results[1]["snippet"], "Body text.")

    def test_sponsored_results_are_skipped(self) -> None:
        sponsored = (
            '<div class="result result--ad"><a class="result__a" '
            'href="//duckduckgo.com/y.js?ad_provider=bingv7aa">Buy Rust</a></div>'
        )
        page = results_page(
            sponsored, result_block("https%3A%2F%2Forganic.example%2F", "Organic", "Real.")
        )

        results = MODULE.parse_results(page, 10)

        self.assertEqual([item["url"] for item in results], ["https://organic.example/"])

    def test_markup_entities_in_titles_and_snippets_are_decoded(self) -> None:
        page = results_page(
            result_block(
                "https%3A%2F%2Fexample.com%2F",
                "Tips &amp; Tricks",
                "Uses <b>bold</b> &amp; more",
            )
        )

        results = MODULE.parse_results(page, 10)

        self.assertEqual(results[0]["title"], "Tips & Tricks")
        self.assertEqual(results[0]["snippet"], "Uses bold & more")

    def test_num_results_bounds_the_returned_list(self) -> None:
        page = results_page(
            *(result_block(f"https%3A%2F%2F{i}.example%2F", f"R{i}", "s") for i in range(5))
        )

        self.assertEqual(len(MODULE.parse_results(page, 2)), 2)


class WebsearchFailureSurfacingTests(unittest.TestCase):
    """Zero results must never be able to mean 'the provider was unreachable'."""

    def test_bot_challenge_page_raises_rather_than_returning_nothing(self) -> None:
        with self.assertRaises(MODULE.ProviderUnavailable) as caught:
            MODULE.parse_results(CHALLENGE_PAGE, 10)

        self.assertIn("challenge", str(caught.exception))

    def test_page_without_a_results_container_raises(self) -> None:
        with self.assertRaises(MODULE.ProviderUnavailable) as caught:
            MODULE.parse_results("<html><body><p>Service unavailable</p></body></html>", 10)

        self.assertIn("results container", str(caught.exception))

    def test_results_container_with_no_results_is_an_empty_answer_not_an_error(self) -> None:
        """A real results page that matched nothing is a legitimate zero."""
        self.assertEqual(MODULE.parse_results(results_page(), 10), [])


class RedirectUnwrappingTests(unittest.TestCase):
    def test_only_the_duckduckgo_redirect_wrapper_is_accepted(self) -> None:
        unwrap = MODULE.DuckDuckGoResults._target_url

        self.assertEqual(
            unwrap("//duckduckgo.com/l/?uddg=https%3A%2F%2Fa.example%2F&rut=x"),
            "https://a.example/",
        )
        self.assertEqual(unwrap("/l/?uddg=https%3A%2F%2Fb.example%2F"), "https://b.example/")
        self.assertIsNone(unwrap("//duckduckgo.com/y.js?ad_provider=bingv7aa"))
        self.assertIsNone(unwrap("//duckduckgo.com/l/?rut=x"))
        self.assertIsNone(unwrap(None))


if __name__ == "__main__":
    unittest.main()
