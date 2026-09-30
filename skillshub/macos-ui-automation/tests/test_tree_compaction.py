"""The AX tree `get_window_state` returns is compacted before the agent sees it.

A WorkFlowy window came back as 734 lines / 70 KB (~17k tokens): 324
unlabeled `AXGroup` containers and the same `actions=[AXShowMenu,
AXScrollToVisible]` suffix on 695 lines. The agentic loop projects a tool
result to ~6k tokens and pages the rest through `read_result`, one decision
per page — a look at one window cost four decisions, and the models started
dumping the JSON to /tmp and grepping it with python instead.
"""
from __future__ import annotations

import importlib.machinery
import importlib.util
import json
import os
import unittest

CONTROLLER = os.path.join(
    os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "bin", "macos-ui-controller"
)


def load_controller():
    loader = importlib.machinery.SourceFileLoader("macos_ui_controller", CONTROLLER)
    spec = importlib.util.spec_from_loader("macos_ui_controller", loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


UBIQ = "actions=[AXShowMenu, AXScrollToVisible]"
TREE = "\n".join(
    [
        f"- [0] AXApplication (WorkFlowy) {UBIQ}",
        f"  - [1] AXWebArea {UBIQ}",
        f"    - [2] AXGroup {UBIQ}",
        f"      - [3] AXGroup {UBIQ}",
        f"        - [4] AXLink (Home) {UBIQ}",
        f'          - [5] AXStaticText = "Home" {UBIQ}',
        f"        - [6] AXLink (Inbox) {UBIQ}",
        f'          - [7] AXStaticText = "Inbox" {UBIQ}',
        f"      - [8] AXGroup {UBIQ}",
        f"        - [9] AXImage {UBIQ}",
        f"        - [10] AXButton actions=[AXPress, AXShowMenu, AXScrollToVisible]",
        f'        - [11] AXTextArea = "Magician CUA test" {UBIQ}',
        f"        - [12] AXGroup {UBIQ}",
        f"          - [13] AXTextField {UBIQ}",
        f"          - [14] AXImage (avatar) {UBIQ}",
    ]
)


class TreeCompaction(unittest.TestCase):
    def setUp(self):
        self.m = load_controller()

    def test_labelled_filter_keeps_what_an_agent_can_read_or_act_on(self):
        out = self.m.compact_tree_markdown(TREE, {"filter": "labelled"})
        lines = out.splitlines()
        kept = [int(l.split("[", 1)[1].split("]")[0]) for l in lines if l.lstrip().startswith("- [")]
        # Labelled containers, links, actionable controls, labelled images stay;
        # the static text that only repeats its parent link's title does not.
        self.assertEqual(kept, [0, 4, 6, 10, 11, 13, 14])
        # Unlabeled AXGroup / AXWebArea / AXImage are gone.
        self.assertNotIn("[2] AXGroup", out)
        self.assertNotIn("[9] AXImage", out)
        # The ubiquitous action suffix is gone; a distinctive one survives.
        self.assertNotIn(UBIQ, out)
        self.assertIn("[10] AXButton actions=[AXPress", out)
        # Element indices are the driver's own, untouched.
        self.assertIn("[11] AXTextArea = \"Magician CUA test\"", out)
        # Indentation still shows structure, one space per level.
        self.assertTrue(lines[-1].startswith("     - [14]"), lines[-1])

    def test_the_summary_line_says_what_was_hidden_and_how_to_see_it(self):
        out = self.m.compact_tree_markdown(TREE, {"filter": "labelled"})
        head = out.splitlines()[0]
        self.assertIn("15 nodes", head)
        self.assertIn("7 shown", head)
        self.assertIn('"filter":"full"', head)

    def test_full_filter_is_the_raw_tree(self):
        self.assertEqual(self.m.compact_tree_markdown(TREE, {"filter": "full"}), TREE)

    def test_query_keeps_matches_with_their_children(self):
        out = self.m.compact_tree_markdown(TREE, {"query": "inbox"})
        self.assertIn("[6] AXLink (Inbox)", out)
        self.assertNotIn("(Home)", out)

    def test_roles_and_max_lines_bound_the_page(self):
        out = self.m.compact_tree_markdown(TREE, {"roles": ["AXTextArea", "AXTextField"]})
        body = [l for l in out.splitlines() if l.lstrip().startswith("- [")]
        self.assertEqual(len(body), 2)
        out = self.m.compact_tree_markdown(TREE, {"filter": "labelled", "max_lines": 3})
        body = [l for l in out.splitlines() if l.lstrip().startswith("- [")]
        self.assertEqual(len(body), 3)
        self.assertIn("truncated", out.splitlines()[0])

    def test_view_args_are_stripped_before_the_driver_sees_them(self):
        forwarded, view = self.m.split_view_args(
            json.dumps({"pid": 1, "window_id": 2, "filter": "labelled", "query": "x", "roles": ["AXLink"], "max_lines": 9})
        )
        self.assertEqual(json.loads(forwarded), {"pid": 1, "window_id": 2})
        self.assertEqual(view, {"filter": "labelled", "query": "x", "roles": ["AXLink"], "max_lines": 9})

    def test_default_view_is_labelled(self):
        out = self.m.compact_get_window_state(json.dumps({"tree_markdown": TREE, "element_count": 15}), {})
        payload = json.loads(out)
        self.assertNotIn("[2] AXGroup", payload["tree_markdown"])
        self.assertEqual(payload["element_count"], 15)
        self.assertEqual(payload["tree_view"], "labelled")


if __name__ == "__main__":
    unittest.main()
