from __future__ import annotations

import importlib.util
from importlib.machinery import SourceFileLoader
import pathlib
import unittest


SCRIPT = pathlib.Path(__file__).parents[1] / "bin" / "arxiv-search"
LOADER = SourceFileLoader("arxiv_search", str(SCRIPT))
SPEC = importlib.util.spec_from_loader("arxiv_search", LOADER)
assert SPEC is not None
MODULE = importlib.util.module_from_spec(SPEC)
LOADER.exec_module(MODULE)


ATOM_FIXTURE = b"""<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom"
      xmlns:arxiv="http://arxiv.org/schemas/atom">
  <entry>
    <id>https://arxiv.org/abs/2607.12345v2</id>
    <title>  Bounded   Retrieval Ladders </title>
    <summary>Deterministic evidence acquisition with typed fallback.</summary>
    <published>2026-07-22T10:30:00Z</published>
    <author><name>Ada Researcher</name></author>
    <category term="cs.AI" />
    <arxiv:primary_category term="cs.AI" />
  </entry>
</feed>
"""


class ArxivSearchTests(unittest.TestCase):
    def test_atom_response_normalizes_to_canonical_v1_item(self) -> None:
        items = MODULE.parse_response(ATOM_FIXTURE, 10)

        self.assertEqual(len(items), 1)
        self.assertEqual(items[0]["source_item_id"], "2607.12345v2")
        self.assertEqual(items[0]["title"], "Bounded Retrieval Ladders")
        self.assertEqual(
            items[0]["canonical_url"], "https://arxiv.org/abs/2607.12345v2"
        )
        self.assertEqual(items[0]["metadata"]["authors"], ["Ada Researcher"])
        self.assertEqual(items[0]["metadata"]["primary_category"], "cs.AI")
        self.assertIsInstance(items[0]["published_at_ms"], int)

    def test_limit_is_applied_during_parsing(self) -> None:
        self.assertEqual(MODULE.parse_response(ATOM_FIXTURE, 0), [])


if __name__ == "__main__":
    unittest.main()
