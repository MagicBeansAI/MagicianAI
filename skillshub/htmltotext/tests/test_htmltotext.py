from __future__ import annotations

import contextlib
import importlib.machinery
import importlib.util
import io
import json
import sys
import unittest
from pathlib import Path


BIN = Path(__file__).resolve().parents[1] / "bin" / "htmltotext"
SKILL = Path(__file__).resolve().parents[1] / "SKILL.md"

ARTICLE = (
    "<html><body><article><p>"
    + "Governed extraction sentence for the fixture. " * 8
    + "</p></article></body></html>"
)


def load_adapter():
    loader = importlib.machinery.SourceFileLoader("htmltotext_adapter", str(BIN))
    spec = importlib.util.spec_from_loader(loader.name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


def declared_bin_blocks() -> list[list[str]]:
    """Every `bins:` list in the manifest frontmatter, as declared.

    Read by indentation rather than through a YAML parser so the tests carry
    no dependency the governed adapter does not already need.
    """
    frontmatter = SKILL.read_text(encoding="utf-8").split("\n---\n", 1)[0]
    blocks: list[list[str]] = []
    lines = frontmatter.splitlines()
    for index, line in enumerate(lines):
        if line.strip() != "bins:":
            continue
        indent = len(line) - len(line.lstrip())
        entries: list[str] = []
        for candidate in lines[index + 1 :]:
            stripped = candidate.strip()
            if not stripped or stripped.startswith("#"):
                continue
            if len(candidate) - len(candidate.lstrip()) < indent or not stripped.startswith("- "):
                break
            entries.append(stripped[2:].strip())
        blocks.append(entries)
    return blocks


def declared_entrypoints() -> list[str]:
    frontmatter = SKILL.read_text(encoding="utf-8").split("\n---\n", 1)[0]
    return [
        line.split(":", 1)[1].strip()
        for line in frontmatter.splitlines()
        if line.strip().startswith("entrypoint:")
    ]


def has_extractor() -> bool:
    for module in ("trafilatura", "bs4"):
        if importlib.util.find_spec(module) is not None:
            return True
    return False


def run_main(module, request: dict) -> tuple[int, dict]:
    stdin = io.StringIO(json.dumps(request))
    original = sys.stdin
    sys.stdin = stdin
    try:
        with contextlib.redirect_stdout(io.StringIO()) as out:
            status = module.main()
    finally:
        sys.stdin = original
    return status, json.loads(out.getvalue())


class GovernedPathContractTests(unittest.TestCase):
    """Pin the interpreter requirement that took this reader dark.

    The defect this guards: the governed child receives a cleared environment
    whose PATH is assembled only from the directories that resolve the
    manifest's declared `bins`. This package declared just its own entry
    point, so `#!/usr/bin/env python3` resolved to the OS interpreter, which
    carries neither trafilatura nor beautifulsoup4.

    Nothing about that looked like a failure. The HTTPS fetch succeeded, the
    process exited zero, no error envelope was produced, and the canary saw
    only `chars_extracted: 0` — the silent zero this whole lane exists to
    catch. Declaring `python3` is what puts the environment holding those
    libraries on the governed PATH.
    """

    def test_the_interpreter_is_declared_so_the_governed_path_can_resolve_it(self):
        blocks = declared_bin_blocks()
        self.assertTrue(blocks, "the manifest declares no bins at all")
        for entries in blocks:
            self.assertIn("htmltotext", entries)
            self.assertIn(
                "python3",
                entries,
                "a bins list omits the interpreter, so the governed PATH falls "
                "back to an OS python without the extraction libraries",
            )

    def test_declaring_a_companion_also_declares_which_binary_is_the_entry_point(self):
        """The half of the companion declaration that is not optional.

        `validate_requirements` in tool-runtime-core refuses a CLI contract
        that names more than one binary without an exact `entrypoint`, and the
        refusal is not soft: the contract fails validation,
        `project_runtime_package_to_pack` errors, the loader logs and DROPS the
        pack, and the tool then answers `unknown inner-loop pack` at dispatch.
        Adding `python3` without adding this line therefore left the skill
        strictly worse off than not declaring the interpreter at all — it went
        from producing empty text to not loading.

        `bins` is a set, so declaration order implies nothing. The lexical
        first name here happens to be the adapter, which is exactly why the
        omission has to be caught by a test rather than by a run that looked
        fine on this package.
        """
        entrypoints = declared_entrypoints()
        self.assertTrue(
            entrypoints,
            "a multi-binary contract declares no entrypoint, so the pack cannot load",
        )
        for entrypoint in entrypoints:
            self.assertEqual(entrypoint, "htmltotext")
        for entries in declared_bin_blocks():
            if len(entries) > 1:
                self.assertIn("htmltotext", entries)

    def test_the_canary_still_demands_a_positive_character_count(self):
        # The fetch succeeded while extraction was dead, so exit status,
        # latency and the absent error envelope all read healthy. Only the
        # count saw it, which is why it may never be relaxed to presence.
        frontmatter = SKILL.read_text(encoding="utf-8").split("\n---\n", 1)[0]
        self.assertIn('require_pointers: ["/chars_extracted"]', frontmatter)
        self.assertIn('error_pointer: "/error"', frontmatter)


class ExtractionDiagnosisTests(unittest.TestCase):
    """An unusable extractor must be distinguishable from an empty page."""

    @classmethod
    def setUpClass(cls):
        cls.adapter = load_adapter()

    @unittest.skipUnless(has_extractor(), "no HTML extractor in this interpreter")
    def test_a_working_extractor_reports_text_and_no_diagnosis(self):
        text, method, unavailable = self.adapter.extract_text(ARTICLE, True)

        self.assertEqual(unavailable, "")
        self.assertIn(method, ("trafilatura", "beautifulsoup_fallback"))
        self.assertGreater(len(text), 0)

    @unittest.skipUnless(has_extractor(), "no HTML extractor in this interpreter")
    def test_a_page_with_no_readable_text_is_empty_but_not_an_error(self):
        # "This page has nothing to read" is a real answer about the page.
        # It must not be dressed up as a broken installation.
        text, _, unavailable = self.adapter.extract_text(
            "<html><body><script>var x=1;</script></body></html>", True
        )

        self.assertEqual(unavailable, "")
        self.assertEqual(text.strip(), "")

    def test_both_extractors_unavailable_produces_a_named_diagnosis(self):
        original = self.adapter.re
        try:
            # Break the BeautifulSoup branch the way a missing dependency
            # does. On an interpreter that already lacks both libraries this
            # changes nothing and the same path is exercised.
            self.adapter.re = None
            text, method, unavailable = self.adapter.extract_text("<p>x</p>", True)
        finally:
            self.adapter.re = original

        self.assertEqual(text, "")
        self.assertEqual(method, "none")
        self.assertNotEqual(
            unavailable,
            "",
            "an interpreter with no usable extractor must say so; returning an "
            "empty string here is the undiagnosable zero the canary caught",
        )
        self.assertIn(
            sys.executable,
            unavailable,
            "the diagnosis must name the interpreter that lacked the library, "
            "because which python3 the governed PATH resolved is the answer",
        )

    def test_an_unavailable_extractor_becomes_an_error_envelope_not_a_silent_zero(self):
        module = load_adapter()
        module.extract_text = lambda *_args, **_kwargs: ("", "none", "no HTML extractor")

        status, result = run_main(module, {"input_file": str(SKILL)})

        self.assertIn("error", result)
        self.assertEqual(result["chars_extracted"], 0)
        self.assertEqual(result["content"], "")
        # The exit code is load-bearing. The governed runtime parses stdout
        # only when the process exits zero and parses stderr otherwise, so an
        # error envelope printed to stdout beside a non-zero exit reaches
        # nobody — neither the canary's declared error pointer nor the
        # content reader's.
        self.assertEqual(
            status,
            0,
            "an /error envelope on stdout must be paired with a zero exit or "
            "the governed runtime discards it",
        )

    def test_a_failed_fetch_keeps_its_diagnosis_readable(self):
        # The SSL-trust failure this package's canary exists to detect arrives
        # through this branch. It returned 1 while printing to stdout, so the
        # diagnosis was thrown away and `/error` could never have resolved.
        module = load_adapter()
        module.read_html = lambda *_args, **_kwargs: (_ for _ in ()).throw(
            RuntimeError("CERTIFICATE_VERIFY_FAILED")
        )

        status, result = run_main(module, {"url": "https://example.com"})

        self.assertEqual(status, 0)
        self.assertIn("CERTIFICATE_VERIFY_FAILED", result["error"])
        self.assertEqual(result["chars_extracted"], 0)


FERN_PRICING_PAGE = (
    "<html><body>"
    "<nav><a href='/'>Docs</a><a href='/models'>Models</a></nav>"
    "<article class='w-content-width mx-auto'><div class='fern-prose prose'>"
    "<h1>Pricing</h1>"
    "<p>" + "All prices are in Indian Rupees and are rounded per the billing notes for each service. " * 6 + "</p>"
    "<h2>Language Models</h2><p>All prices are per 1M tokens for every language model listed below.</p>"
    "<div class='fern-table-root not-prose'><div class='fern-scroll-area'><div class='fern-scroll-area-viewport'><div>"
    "<table class='fern-table'><thead><tr><th>Model</th><th>Input</th><th>Cached input</th><th>Output</th><th>Status</th></tr></thead>"
    "<tbody><tr><td>Sarvam 105B (<code>sarvam-105b</code>)</td><td>₹29.28</td><td>₹10.98</td><td>₹73.2</td><td>Available</td></tr>"
    "<tr><td>Gemma 4 31B (<code>gemma-4-31b</code>)</td><td>₹36.60</td><td>₹13.73</td><td>₹91.50</td><td>Available</td></tr></tbody></table>"
    "</div></div></div></div>"
    "<h2>Speech</h2><p>Speech services are billed per hour of audio processed, rounded up to the next second.</p>"
    "</div></article>"
    "<footer><table><tr><td>© Example</td><td>Privacy</td></tr></table></footer>"
    "</body></html>"
)


class DroppedTableRecovery(unittest.TestCase):
    """A documentation site that wraps its tables in a scroll container —
    Fern, Mintlify, Docusaurus all do — loses them to the article
    extractor, which keeps the prose around them and reports a confident
    page. The pricing comparison the web researcher ran on 2026-09-19 read
    such a page as 1,330 bytes of intro and told the user the rates were
    not exposed; the table with them was in the HTML all along. The
    extraction must keep every content table the article lost."""

    def setUp(self):
        for module in ("trafilatura", "bs4"):
            if importlib.util.find_spec(module) is None:
                self.skipTest(f"{module} is not installed in this interpreter")
        self.adapter = load_adapter()

    def test_a_table_the_article_extractor_dropped_is_recovered_as_rows(self):
        text, method, diagnosis = self.adapter.extract_text(FERN_PRICING_PAGE, include_links=False)
        self.assertEqual(diagnosis, "")
        self.assertIn("Language Models", text, "the article prose is still the body of the extraction")
        self.assertIn("sarvam-105b", text, f"the dropped pricing table must be recovered (method {method}):\n{text}")
        self.assertIn("₹29.28", text)
        self.assertIn("Gemma 4 31B", text)
        # Rows keep their cells together and in order, so a value can be read
        # against its model and its column.
        row = next((line for line in text.splitlines() if "sarvam-105b" in line), "")
        self.assertRegex(row, r"sarvam-105b.*₹29\.28.*₹10\.98.*₹73\.2")
        header = next((line for line in text.splitlines() if "Cached input" in line and "Model" in line), "")
        self.assertTrue(header, "the header row travels with the table so columns have names")

    def test_a_table_the_extractor_kept_is_not_appended_twice(self):
        page = (
            "<html><body><article><p>" + "Plain article prose that the extractor keeps. " * 8 + "</p>"
            "<table><tr><th>Key</th><th>Value</th></tr><tr><td>limit</td><td>500 requests per minute</td></tr></table>"
            "</article></body></html>"
        )
        text, _, _ = self.adapter.extract_text(page, include_links=False)
        self.assertEqual(text.count("500 requests per minute"), 1, text)

    def test_a_dropped_table_whose_words_all_occur_in_the_prose_is_still_recovered(self):
        # "Kept" must mean the row itself is in the text, not that each of
        # its cells happens to be a word the prose also uses. Same page shape
        # as the real drop, with a paragraph that mentions every cell of the
        # first data row on its own.
        mention = (
            "<p>The row for Sarvam 105B ( sarvam-105b ) reads ₹29.28 for input, ₹10.98 for cached input, "
            "₹73.2 for output, and Available for status, as the notes below explain at length. "
            "The row for Sarvam 105B ( sarvam-105b ) reads ₹29.28 for input, ₹10.98 for cached input, "
            "₹73.2 for output, and Available for status, as the notes below explain at length.</p>"
        )
        page = FERN_PRICING_PAGE.replace("<h2>Language Models</h2>", mention + "<h2>Language Models</h2>")
        text, _, _ = self.adapter.extract_text(page, include_links=False)
        rows = [line for line in text.splitlines() if line.startswith("| ")]
        self.assertTrue(
            any("sarvam-105b" in r and "₹29.28" in r for r in rows),
            f"the table must be recovered as rows even though the prose uses every cell:\n{text}",
        )

    def test_chrome_tables_are_not_recovered(self):
        text, _, _ = self.adapter.extract_text(FERN_PRICING_PAGE, include_links=False)
        self.assertNotIn("Privacy", text, "a footer table is chrome, not content")

    def test_a_kept_table_gets_its_dropped_unit_caption_back(self):
        # The OpenAI pricing page as served: the table survives extraction,
        # the section heading survives, and the unit lives in a <small>
        # caption between them that the extractor prunes — so the researcher
        # reports "the table does not state its billing unit".
        text, method, _ = self.adapter.extract_text(OPENAI_PRICING_PAGE, include_links=False)
        self.assertIn("gpt-5.6-sol", text)
        self.assertIn("Prices per 1M tokens.", text, f"the unit caption must travel with the table (method {method}):\n{text}")
        self.assertEqual(text.count("Prices per 1M tokens."), 1)
        self.assertEqual(text.count("gpt-5.6-sol"), 1, "a kept table is not appended again just to carry its caption")
        # The note must sit directly above the table it qualifies, not at the
        # end of the page: a note far from its table reads as unattached, and
        # the researcher then hedges that the unit "did not clearly attach".
        note_at = text.index("Prices per 1M tokens.")
        table_at = text.index("gpt-5.6-sol")
        self.assertLess(note_at, table_at, "the note precedes its table")
        between = text[note_at:table_at]
        self.assertNotIn("Cached input applies", between, "no unrelated prose between the note and its table")
        self.assertLess(len(between), 400, f"the note is adjacent to its table, not {len(between)} chars away")


OPENAI_PRICING_PAGE = (
    "<html><body><main class='min-w-0 flex-1'><div class='page-container'><article class='prose prose-content'>"
    "<p>" + "Our latest models are priced by usage, with every current rate in the tables below. " * 6 + "</p>"
    "<div class='pricing-switcher-layout'>"
    "<div class='pricing-switcher-header pricing-section-heading'>"
    "<div class='anchor-heading-wrapper'><h2 class='anchor-heading' id='text-tokens'><p>Flagship models</p>"
    "<svg class='anchor-heading-icon' role='presentation'><path d='M1 1'></path></svg></h2></div>"
    "<div class='pricing-switcher-subheading'>Our latest models</div>"
    "<small class='pricing-switcher-meta'>Prices per 1M tokens.</small></div>"
    "<div class='content-switcher-root mt-6'><div class='content-switcher-panes'><astro-island uid='x1'><div><div><div>"
    "<table><thead><tr><th>Model</th><th>Input</th><th>Cached input</th><th>Output</th></tr></thead>"
    "<tbody><tr><td>gpt-5.6-sol</td><td>$4.00</td><td>$0.40</td><td>$20.00</td></tr>"
    "<tr><td>gpt-5.6-terra</td><td>$2.00</td><td>$0.20</td><td>$12.00</td></tr>"
    "<tr><td>gpt-5.6-luna</td><td>$0.20</td><td>$0.02</td><td>$1.20</td></tr></tbody></table>"
    "</div></div></div></astro-island></div></div></div>"
    "<p>" + "Cached input applies when a prefix of the prompt was seen recently by the same model. " * 4 + "</p>"
    "</article></div></main></body></html>"
)


def astro_island_page(rendered_chars: int) -> str:
    """An Astro page as www.sarvam.ai/api-pricing is served: a short
    server-rendered blurb, and the pricing table shipped as serialized props
    on a client-hydrated island."""
    services = [
        ("Sarvam 105B", "₹29.28 / ₹10.98 / ₹73.20", "per 1M tokens"),
        ("Sarvam 105B Chat", "₹29.28 / ₹10.98 / ₹73.20", "per 1M tokens"),
        ("Gemma 4 31B", "₹36.60 / ₹13.73 / ₹91.50", "per 1M tokens"),
        ("GLM-5.3", "₹128.10 / ₹23.79 / ₹402.60", "per 1M tokens"),
        ("DeepSeek V4 Flash", "₹25.62 / ₹6.41 / ₹51.24", "per 1M tokens"),
        ("Saarika speech to text", "₹30 per hour of audio", "per hour"),
        ("Saaras speech translation", "₹45 per hour of audio", "per hour"),
        ("Bulbul text to speech", "₹15 per 10,000 characters", "per 10k characters"),
        ("Mayura translation", "₹20 per 10,000 characters", "per 10k characters"),
        ("Sarvam Vision document parsing", "₹1.50 per page", "per page"),
        ("Dubbing", "₹3.00 per minute of media", "per minute"),
    ]
    props = json.dumps({
        "rows": [0, [[0, {"service": [0, name], "price": [0, price], "unit": [0, unit]}] for name, price, unit in services]],
        "footnote": [0, "Prices are exclusive of GST and billed from a shared credit balance."],
    })
    blurb = ("Buy credits once, use them anywhere. Start with free credits, then pay only for what you use. " * 4)[:rendered_chars]
    return (
        "<html><head><script type='application/ld+json'>" + json.dumps({"@type": "WebPage", "name": "API pricing"}) + "</script></head>"
        "<body><main><h1>API pricing</h1><p>" + blurb + "</p>"
        "<astro-island uid='Z2aS8Kc' component-url='/_astro/ApiPricingTable.js' renderer-url='/_astro/client.js' "
        "props='" + html_escape(props) + "'></astro-island>"
        "<script src='/_astro/client.js'></script><script>" + ("/* bundle */ " * 400) + "</script>"
        "</main></body></html>"
    )


def html_escape(value: str) -> str:
    return value.replace("&", "&amp;").replace("'", "&#39;").replace("<", "&lt;")


class ClientRenderedShellDetection(unittest.TestCase):
    """The reader's quality boundary was a character count, so a 458-character
    blurb over a 198 KB app shell counted as a sufficient page and the
    researcher reported the rates it never received. The extractor can see
    what the count cannot: content shipped for client-side hydration that
    the server never rendered. When there is more of that than of rendered
    text, the page is a shell and the reader must hand off to the browser."""

    def setUp(self):
        for module in ("trafilatura", "bs4"):
            if importlib.util.find_spec(module) is None:
                self.skipTest(f"{module} is not installed in this interpreter")
        self.adapter = load_adapter()

    def test_hydration_props_holding_more_content_than_was_rendered_is_a_shell(self):
        html = astro_island_page(rendered_chars=460)
        text, _, _ = self.adapter.extract_text(html, include_links=False)
        verdict = self.adapter.detect_client_rendered_shell(html, text)
        self.assertTrue(verdict["shell"], verdict)
        self.assertIn("hydration", verdict["reason"])
        self.assertGreater(verdict["hidden_payload_chars"], verdict["rendered_chars"])
        self.assertTrue(any("per 1M tokens" in sample for sample in verdict["sample"]), verdict["sample"])

    def test_a_next_data_payload_is_a_hydration_source_too(self):
        page = json.dumps({"props": {"pageProps": {"plans": [
            {"name": "Starter", "limit": "500 requests per minute", "price": "$0 per month"},
            {"name": "Growth", "limit": "5000 requests per minute", "price": "$49 per month"},
            {"name": "Scale", "limit": "50000 requests per minute", "price": "Contact sales for volume pricing"},
        ], "faq": "Plans renew monthly and unused requests do not carry over to the next billing period."}}})
        html = ("<html><body><div id='__next'><p>Loading plans…</p></div>"
                "<script id='__NEXT_DATA__' type='application/json'>" + page + "</script>"
                "<script src='/_next/static/chunks/main.js'></script></body></html>")
        text, _, _ = self.adapter.extract_text(html, include_links=False)
        verdict = self.adapter.detect_client_rendered_shell(html, text)
        self.assertTrue(verdict["shell"], verdict)

    def test_an_empty_app_root_over_a_script_bundle_is_a_shell_even_with_no_payload(self):
        html = ("<html><body><div id='root'></div><noscript>You need to enable JavaScript to run this app.</noscript>"
                "<script>" + ("!function(){var e=1;}(); " * 2000) + "</script></body></html>")
        text, _, _ = self.adapter.extract_text(html, include_links=False)
        verdict = self.adapter.detect_client_rendered_shell(html, text)
        self.assertTrue(verdict["shell"], verdict)
        self.assertIn("empty", verdict["reason"])

    def test_a_server_rendered_page_with_seo_metadata_is_not_a_shell(self):
        # ld+json mirrors page content for search engines; it is not hidden
        # content, and a page whose table is in the HTML must not hand off.
        text, _, _ = self.adapter.extract_text(FERN_PRICING_PAGE, include_links=False)
        html = FERN_PRICING_PAGE.replace("<html><body>", "<html><head><script type='application/ld+json'>"
                                          + json.dumps({"@type": "FAQPage", "mainEntity": [{"name": "What is the pricing for text models?",
                                                        "text": "Sarvam 105B costs ₹29.28 per 1M input tokens and ₹73.2 per 1M output tokens."}]})
                                          + "</script></head><body>")
        verdict = self.adapter.detect_client_rendered_shell(html, text)
        self.assertFalse(verdict["shell"], verdict)

    def test_a_short_plain_article_is_not_a_shell(self):
        html = "<html><body><article><p>" + "A short but complete note that says one thing plainly and stops. " * 4 + "</p></article></body></html>"
        text, _, _ = self.adapter.extract_text(html, include_links=False)
        verdict = self.adapter.detect_client_rendered_shell(html, text)
        self.assertFalse(verdict["shell"], verdict)

    def test_a_page_whose_hydration_payload_repeats_its_rendered_text_is_not_a_shell(self):
        # Islands that hydrate what the server already rendered ship the same
        # strings; nothing is hidden.
        html = astro_island_page(rendered_chars=460).replace(
            "<h1>API pricing</h1>",
            "<h1>API pricing</h1><table>"
            + "".join(f"<tr><td>{name}</td><td>{price}</td><td>{unit}</td></tr>" for name, price, unit in [
                ("Sarvam 105B", "₹29.28 / ₹10.98 / ₹73.20", "per 1M tokens"), ("Sarvam 105B Chat", "₹29.28 / ₹10.98 / ₹73.20", "per 1M tokens"),
                ("Gemma 4 31B", "₹36.60 / ₹13.73 / ₹91.50", "per 1M tokens"), ("GLM-5.3", "₹128.10 / ₹23.79 / ₹402.60", "per 1M tokens"),
                ("DeepSeek V4 Flash", "₹25.62 / ₹6.41 / ₹51.24", "per 1M tokens"), ("Saarika speech to text", "₹30 per hour of audio", "per hour"),
                ("Saaras speech translation", "₹45 per hour of audio", "per hour"), ("Bulbul text to speech", "₹15 per 10,000 characters", "per 10k characters"),
                ("Mayura translation", "₹20 per 10,000 characters", "per 10k characters"), ("Sarvam Vision document parsing", "₹1.50 per page", "per page"),
                ("Dubbing", "₹3.00 per minute of media", "per minute")])
            + "</table><p>Prices are exclusive of GST and billed from a shared credit balance.</p>")
        text, _, _ = self.adapter.extract_text(html, include_links=False)
        verdict = self.adapter.detect_client_rendered_shell(html, text)
        self.assertFalse(verdict["shell"], verdict)

    def test_a_server_rendered_next_page_whose_flight_payload_mirrors_the_dom_is_not_a_shell(self):
        # Next.js app-router pages ship the whole page again as an RSC
        # flight payload, nested as JSON inside pushed JSON strings, full of
        # class lists and chunk URLs. Everything in it the server also
        # rendered — including nav labels the article extractor strips — so
        # nothing is hidden. docs.sarvam.ai is served this way.
        nav = "".join(f"<a href='/models/{i}'>Model family {i}</a>" for i in range(40))
        table = ("<div class='fern-table-root not-prose'><table><tr><th>Model</th><th>Input</th></tr>"
                 "<tr><td>Sarvam 105B</td><td>₹29.28</td></tr></table></div>")
        flight_inner = json.dumps([["$", "div", None, {"className": "fern-table-root not-prose", "children": [
            ["$", "table", None, {"children": [["$", "tr", None, {"children": [["$", "td", None, {"children": "Sarvam 105B"}],
                                                                                   ["$", "td", None, {"children": "₹29.28"}]]}]]}]]}],
            *[["$", "a", None, {"href": f"/models/{i}", "className": "fern-sidebar-link", "children": f"Model family {i}"}] for i in range(40)]])
        flight = "self.__next_f.push([1," + json.dumps("2:" + flight_inner + "\n") + "])"
        html = ("<html><body><nav class='fern-sidebar'>" + nav + "</nav><main><article class='fern-prose'>"
                "<h1>Pricing</h1><p>" + "All prices are in Indian Rupees and rounded per the billing notes. " * 6 + "</p>"
                + table + "</article></main><script>" + flight + "</script></body></html>")
        text, _, _ = self.adapter.extract_text(html, include_links=False)
        verdict = self.adapter.detect_client_rendered_shell(html, text)
        self.assertFalse(verdict["shell"], verdict)

    def test_the_envelope_carries_the_verdict_for_the_reader(self):
        import tempfile

        with tempfile.NamedTemporaryFile("w", suffix=".html", delete=False, encoding="utf-8") as handle:
            handle.write(astro_island_page(rendered_chars=460))
            path = handle.name
        try:
            status, envelope = run_main(self.adapter, {"input_file": path, "include_links": False})
        finally:
            Path(path).unlink(missing_ok=True)
        self.assertEqual(status, 0)
        self.assertTrue(envelope["client_rendered"]["shell"], envelope.get("client_rendered"))
        self.assertIn("content", envelope)


if __name__ == "__main__":
    unittest.main()
