from __future__ import annotations

import importlib.util
from importlib.machinery import SourceFileLoader
import pathlib
import tempfile
import unittest


SCRIPT = pathlib.Path(__file__).parents[1] / "bin" / "structured-web-data"
LOADER = SourceFileLoader("structured_web_data", str(SCRIPT))
SPEC = importlib.util.spec_from_loader("structured_web_data", LOADER)
assert SPEC is not None
MODULE = importlib.util.module_from_spec(SPEC)
LOADER.exec_module(MODULE)


class StructuredWebDataTests(unittest.TestCase):
    def test_extracts_valid_records_and_ignores_malformed_blocks(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "page.html"
            path.write_text(
                """<html><head><title>Example Item</title>
                <script type="application/ld+json">{"@type":"Article","headline":"Useful"}</script>
                <script type="application/ld+json">not json</script>
                </head></html>""",
                encoding="utf-8",
            )
            output = MODULE.extract(path, 4096)

        self.assertNotIn("error", output)
        self.assertEqual(output["title"], "Example Item")
        self.assertIn('"@type": "Article"', output["content"])

    def test_fails_closed_without_valid_records_or_over_bound(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "page.html"
            path.write_text("<html><title>None</title></html>", encoding="utf-8")
            self.assertIn("error", MODULE.extract(path, 4096))
            path.write_text(
                '<script type="application/ld+json">{"value":"long"}</script>',
                encoding="utf-8",
            )
            self.assertIn("error", MODULE.extract(path, 4))


if __name__ == "__main__":
    unittest.main()
