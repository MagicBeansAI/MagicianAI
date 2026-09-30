#!/usr/bin/env python3
"""Focused regressions for Swift/XcodeGen codegraph discovery."""

from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path


SCRIPT_DIR = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location(
    "generate_code_graph", SCRIPT_DIR / "generate_code_graph.py"
)
assert SPEC is not None and SPEC.loader is not None
codegraph = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(codegraph)


class SwiftCodegraphTests(unittest.TestCase):
    def test_discovers_xcodegen_project_and_all_swift_targets(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            project = root / "mobile"
            (project / "App").mkdir(parents=True)
            (project / "AppTests").mkdir()
            (project / "App" / "Screen.swift").write_text(
                "struct Screen {}\n", encoding="utf-8"
            )
            (project / "AppTests" / "ScreenTests.swift").write_text(
                "final class ScreenTests {}\n", encoding="utf-8"
            )
            (project / "project.yml").write_text(
                "name: MobileApp\ntargets:\n  App:\n    type: application\n",
                encoding="utf-8",
            )

            self.assertEqual(
                codegraph.discover_swift_projects(root),
                [("MobileApp", project)],
            )

    def test_swift_test_targets_are_classified_by_directory(self) -> None:
        self.assertTrue(codegraph.is_test_path("magios/MagiosTests/TestSupport.swift"))
        self.assertTrue(codegraph.is_test_path("magios/MagiosUITests/AppLaunch.swift"))
        self.assertFalse(codegraph.is_test_path("magios/Magios/AttentionView.swift"))

    def test_swift_symbols_supply_ranges_for_test_call_edges(self) -> None:
        source = """
func produceResult() -> String { "ok" }

func testResult() {
    _ = produceResult()
}
"""
        functions, _ = codegraph.extract_swift_symbols(source)
        calls = codegraph.extract_call_sites(source, functions, "swift")

        self.assertTrue(all("line_end" in function for function in functions))
        self.assertTrue(
            any(
                call["callee"] == "produceResult"
                and call["owner_line"] == functions[1]["line"]
                for call in calls
            )
        )


if __name__ == "__main__":
    unittest.main()
