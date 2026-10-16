from __future__ import annotations

import importlib.util
from pathlib import Path
import unittest
from unittest import mock


SCRIPT = Path(__file__).with_name("eval-observable-sources-live.py")
SPEC = importlib.util.spec_from_file_location("observable_sources_eval", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
EVAL = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(EVAL)


class ObservableSourcesLiveEvalTests(unittest.TestCase):
    def test_public_url_rejects_local_private_and_credentialed_targets(self) -> None:
        rejected = (
            "http://127.0.0.1/feed",
            "http://10.0.0.1/feed",
            "https://localhost/feed",
            "https://service.local/feed",
            "https://user:secret@example.com/feed",
        )
        for target in rejected:
            with self.subTest(target=target), self.assertRaises(EVAL.EvalFailure):
                EVAL.public_url(target)
        self.assertEqual(
            EVAL.public_url("https://example.com/feed"),
            "https://example.com/feed",
        )

    def test_feed_parser_caps_large_inputs_and_counts_unique_links(self) -> None:
        items = "".join(
            f"<item><guid>{index}</guid><link>https://example.com/{index % 3}</link></item>"
            for index in range(EVAL.MAX_ENTRIES + 20)
        )
        count, unique_links = EVAL.parse_feed(
            f"<rss><channel>{items}</channel></rss>".encode()
        )
        self.assertEqual(count, EVAL.MAX_ENTRIES)
        self.assertEqual(unique_links, 3)

    def test_shipped_manifests_expose_only_exact_rss_observe_profiles(self) -> None:
        sources = EVAL.load_manifests()
        self.assertEqual(
            {source["source_id"] for source in sources},
            {"arxiv-ai", "product-hunt"},
        )
        self.assertTrue(all(source["targets"] for source in sources))

    def test_api_gate_requires_stable_totals_exact_bindings_and_no_duplicates(self) -> None:
        pages = [
            {
                "items": [
                    {
                        "offer_id": "arxiv-ai:observe-rss",
                        "source_id": "arxiv-ai",
                        "readiness": "eligible",
                        "action_bindings": [
                            {"action_id": "rss.discover", "adapter_id": "rss"}
                        ],
                    }
                ],
                "total": 2,
                "next_cursor": "arxiv-ai:observe-rss",
            },
            {
                "items": [
                    {
                        "offer_id": "product-hunt:observe-rss",
                        "source_id": "product-hunt",
                        "readiness": "eligible",
                        "action_bindings": [
                            {"action_id": "rss.discover", "adapter_id": "rss"}
                        ],
                    }
                ],
                "total": 2,
            },
        ]
        with mock.patch.object(EVAL, "api_page", side_effect=pages):
            result = EVAL.inspect_api(
                "http://127.0.0.1:3002",
                1.0,
                {"arxiv-ai", "product-hunt"},
            )
        self.assertEqual(result, {"pages": 2, "offer_count": 2, "total": 2})

    def test_api_gate_rejects_action_widening(self) -> None:
        page = {
            "items": [
                {
                    "offer_id": "product-hunt:observe-rss",
                    "source_id": "product-hunt",
                    "readiness": "eligible",
                    "action_bindings": [
                        {"action_id": "exa.discover", "adapter_id": "exa"}
                    ],
                }
            ],
            "total": 1,
        }
        with mock.patch.object(EVAL, "api_page", return_value=page):
            with self.assertRaises(EVAL.EvalFailure):
                EVAL.inspect_api(
                    "http://127.0.0.1:3002",
                    1.0,
                    {"product-hunt"},
                )


if __name__ == "__main__":
    unittest.main()
