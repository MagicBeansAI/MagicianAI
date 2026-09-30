import importlib.machinery
import importlib.util
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
# The governed skill's entry point has no `.py` suffix (it moved from
# `scripts/htmltotext.py` in the skill migration, and this lane pointed at the
# old path — silently dead — until 2026-09-20), so it is loaded by explicit
# source loader rather than by suffix.
EXTRACTOR = ROOT / "skillshub" / "htmltotext" / "bin" / "htmltotext"


def load_extractor():
    if not EXTRACTOR.is_file():
        raise RuntimeError(f"unable to load extractor at {EXTRACTOR}")
    loader = importlib.machinery.SourceFileLoader("magician_htmltotext", str(EXTRACTOR))
    spec = importlib.util.spec_from_loader(loader.name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


class ContentReaderExtractionEval(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.extractor = load_extractor()

    def test_article_content_survives_while_navigation_and_scripts_do_not(self):
        html = """
        <html><head><title>Reader fixture</title>
        <script>window.secretNoise = 'DO_NOT_KEEP_SCRIPT';</script></head>
        <body>
          <nav>DO_NOT_KEEP_NAVIGATION Home Products Login</nav>
          <main><article>
            <h1>Static reader architecture</h1>
            <p>The acquisition boundary fetches a selected public page only after ranking.</p>
            <p>Conditional requests and scoped content hashes avoid repeated extraction work.</p>
            <p>The extractor should retain connected article prose without surrounding chrome.</p>
          </article></main>
          <footer>DO_NOT_KEEP_FOOTER Legal Careers Cookies</footer>
        </body></html>
        """
        text, method, _diagnosis = self.extractor.extract_text(html, include_links=True)
        self.assertIn("Static reader architecture", text)
        self.assertIn("Conditional requests", text)
        self.assertNotIn("DO_NOT_KEEP_SCRIPT", text)
        self.assertNotIn("DO_NOT_KEEP_NAVIGATION", text)
        self.assertNotIn("DO_NOT_KEEP_FOOTER", text)
        self.assertIn(method, {"trafilatura", "beautifulsoup_fallback"})

    def test_thin_shell_remains_below_the_default_gist_threshold(self):
        html = """
        <html><body><nav>Home Login Menu</nav><main>Loading</main></body></html>
        """
        text, _, _diagnosis = self.extractor.extract_text(html, include_links=False)
        self.assertLess(len(text.strip()), 80)

    def test_long_article_retention_is_not_topic_specific(self):
        paragraphs = "".join(
            f"<p>Paragraph {index} contains neutral prose for extraction retention measurement.</p>"
            for index in range(20)
        )
        text, _, _diagnosis = self.extractor.extract_text(
            f"<html><body><article>{paragraphs}</article></body></html>",
            include_links=False,
        )
        self.assertGreater(len(text), 800)
        self.assertIn("Paragraph 0", text)
        self.assertIn("Paragraph 19", text)

    def test_truncation_marker_stays_inside_the_declared_output_bound(self):
        text, truncated = self.extractor.truncate_text("word " * 100, 80)
        self.assertTrue(truncated)
        self.assertLessEqual(len(text), 80)
        self.assertIn("truncated at 80 chars", text)

    def test_content_tables_survive_every_wrapper_exactly_once_and_chrome_tables_never(self):
        """The generic property behind the 2026-09-19 pricing failure: a page's
        content table reaches the text exactly once whatever the site wrapped
        it in — a bare table, Fern's scroll area, Mintlify's overflow div,
        Docusaurus's table container — and a table inside site chrome never
        does. Neither the class names nor the vendor are what the extractor
        keys on."""
        prose = "<p>" + "Reference prose that the article extractor keeps around the table. " * 6 + "</p>"
        table = (
            "<table><thead><tr><th>Plan</th><th>Requests per minute</th><th>Price</th></tr></thead>"
            "<tbody><tr><td>starter-tier</td><td>500</td><td>$0.00</td></tr>"
            "<tr><td>growth-tier</td><td>5000</td><td>$49.00</td></tr></tbody></table>"
        )
        wrappers = {
            "bare": table,
            "fern": "<div class='fern-table-root not-prose'><div class='fern-scroll-area'>"
                    "<div class='fern-scroll-area-viewport'><div>" + table + "</div></div></div></div>",
            "mintlify": "<div class='overflow-x-auto my-4'>" + table + "</div>",
            "docusaurus": "<div class='tableContainer_k3Xz'>" + table + "</div>",
        }
        for name, wrapped in wrappers.items():
            html = (
                "<html><body><nav><table><tr><td>DO_NOT_KEEP_NAV_CELL</td><td>Login</td></tr></table></nav>"
                "<main><article><h1>Rate limits</h1>" + prose + wrapped + prose + "</article></main>"
                "<footer><table><tr><td>DO_NOT_KEEP_FOOTER_CELL</td><td>Legal</td></tr></table></footer>"
                "</body></html>"
            )
            text, _, _diagnosis = self.extractor.extract_text(html, include_links=False)
            with self.subTest(wrapper=name):
                self.assertEqual(text.count("starter-tier"), 1, f"[{name}] rows appear exactly once:\n{text}")
                self.assertEqual(text.count("growth-tier"), 1)
                row = next((line for line in text.splitlines() if "growth-tier" in line), "")
                self.assertIn("5000", row, f"[{name}] a row keeps its cells together")
                self.assertIn("$49.00", row)
                self.assertNotIn("DO_NOT_KEEP_NAV_CELL", text)
                self.assertNotIn("DO_NOT_KEEP_FOOTER_CELL", text)


if __name__ == "__main__":
    unittest.main()
