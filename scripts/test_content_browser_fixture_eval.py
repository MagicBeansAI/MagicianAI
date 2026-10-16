from __future__ import annotations

import importlib.util
import json
import sys
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("eval-content-browser-fixtures.py")
SPEC = importlib.util.spec_from_file_location("content_browser_fixture_eval", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
fixture_eval = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = fixture_eval
SPEC.loader.exec_module(fixture_eval)


class ContentBrowserFixtureEvalTests(unittest.TestCase):
    def test_shipped_corpus_passes_all_authority_and_lifecycle_gates(self) -> None:
        corpus = json.loads(fixture_eval.DEFAULT_CORPUS.read_text(encoding="utf-8"))
        report = fixture_eval.evaluate_corpus(corpus)
        self.assertEqual(report["gate"], "PASS")
        self.assertGreaterEqual(report["summary"]["case_count"], 12)
        self.assertEqual(report["summary"]["false_browser_escalations"], 0)
        self.assertEqual(report["summary"]["authority_violations"], 0)
        self.assertEqual(report["summary"]["session_leaks"], 0)

    def test_read_authority_cannot_authorize_interaction(self) -> None:
        route = fixture_eval.choose_route(
            {
                "static_outcome": "authentication_required",
                "api_replay_outcome": "authentication_required",
                "requires_identity": True,
                "needs_interaction": True,
            }
        )
        self.assertEqual(route["authority"], "authenticated_interact")
        self.assertEqual(route["action"], "browser.cdp.interact_handoff")

    def test_static_or_replay_success_never_launches_browser(self) -> None:
        static = fixture_eval.choose_route(
            {"static_outcome": "sufficient", "api_replay_outcome": "unavailable"}
        )
        replay = fixture_eval.choose_route(
            {"static_outcome": "javascript_required", "api_replay_outcome": "sufficient"}
        )
        self.assertEqual(static["action"], "static_http.read")
        self.assertEqual(replay["action"], "api_replay.read")


if __name__ == "__main__":
    unittest.main()
