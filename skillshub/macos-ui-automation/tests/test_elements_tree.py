"""The tree the model reads is rendered from the structured elements.

cua-driver 0.28 returns `elements` beside its markdown, whose lines now carry
an attribute bracket the markdown compaction took for a label — a System
Settings window's "labelled" view showed all 212 nodes (40 unlabeled rows and
the whole menu bar among them), at 17 KB exceeded the runtime's scalar budget
and was omitted from the model's page, while the 42 KB `elements` array won
the record budget with its first seven rows. The complete snapshot now goes
to a sidecar on disk; the reply carries a tree rendered from the elements.
"""
from __future__ import annotations

import importlib.machinery
import importlib.util
import json
import os
import tempfile
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


def element(index, role, depth, **extra):
    row = {"element_index": index, "element_token": f"s0000000a:{index}", "role": role, "depth": depth}
    row.update(extra)
    return row


PRESS = ["AXPress"]
ROW = ["AXShowDefaultUI", "AXShowAlternateUI"]
MENU = ["AXCancel", "AXPress", "AXPick"]

ELEMENTS = [
    element(0, "AXWindow", 0, label="Main", actions=["AXRaise"]),
    element(1, "AXOutline", 2, label="Sidebar", actions=["AXShowMenu"]),
    element(2, "AXRow", 3, actions=ROW, selected=False),
    element(3, "AXButton", 5, label="General", actions=PRESS, enabled=True),
    element(4, "AXRow", 3, actions=ROW, selected=True),
    element(5, "AXButton", 5, label="Appearance", actions=PRESS, enabled=True),
    element(6, "AXRow", 3, actions=ROW, selected=False),
    element(7, "AXButton", 5, label="Displays", actions=PRESS, enabled=False),
    element(8, "AXTextField", 2, label="Search", value="Search", actions=["AXShowMenu", "AXConfirm"]),
    element(9, "AXButton", 3, actions=PRESS),
    element(20, "AXButton", 3, label="Go Back", actions=["AXPress", "AXShowMenu"]),
    element(10, "AXGroup", 2),
    element(11, "AXStaticText", 3, value="Appearance"),
    element(12, "AXMenuBar", 1),
    element(13, "AXMenuBarItem", 2, label="File", actions=MENU),
    element(14, "AXMenu", 3, actions=["AXCancel"]),
    element(15, "AXMenuItem", 4, label="Close Window", actions=MENU),
    element(16, "AXMenuItem", 4, label="Export…", actions=MENU),
    element(17, "AXMenuBarItem", 2, label="Help", actions=MENU),
    element(18, "AXMenu", 3, actions=["AXCancel"]),
    element(19, "AXMenuItem", 4, label="Mac User Guide", actions=MENU),
]

# The driver's markdown for the same window: the indexed rows and, between
# them, the text-only nodes `elements` never lists.
MARKDOWN = "\n".join(
    [
        "- [0] AXWindow [id=Main actions=[raise]]",
        "    - [1] AXOutline (Sidebar) [id=com.apple.settings.sidebar actions=[showmenu]]",
        "      - [2] AXRow [actions=[showdefaultui,showalternateui]]",
        "          - [3] AXButton (General) [id=com.apple.settings.general actions=[press]]",
        "      - [4] AXRow [actions=[showdefaultui,showalternateui]]",
        "          - [5] AXButton (Appearance) [id=com.apple.settings.appearance actions=[press]]",
        "      - [6] AXRow [actions=[showdefaultui,showalternateui]]",
        "          - [7] AXButton (Displays) [actions=[press]]",
        '    - [8] AXTextField = "Search" [actions=[showmenu,confirm]]',
        "      - [9] AXButton [id=com.apple.settings.addAccount actions=[press]]",
        "      - [20] AXButton (Go Back) [actions=[press,showmenu]]",
        "    - [10] AXGroup",
        '      - AXStaticText = "Appearance"',
        "      - AXHeading (Theme)",
        '      - AXStaticText = "Colour"',
        '      - AXStaticText = "Multicolour"',
        '        - AXStaticText = "Multicolour"',
        '      - [11] AXStaticText = "Appearance"',
        "  - [12] AXMenuBar",
        '    - [13] AXMenuBarItem "File" [actions=[cancel,press,pick]]',
        "      - [14] AXMenu [actions=[cancel]]",
        '        - [15] AXMenuItem "Close Window" [actions=[cancel,press,pick]]',
        '        - [16] AXMenuItem "Export…" [actions=[cancel,press,pick]]',
        '    - [17] AXMenuBarItem "Help" [actions=[cancel,press,pick]]',
        "      - [18] AXMenu [actions=[cancel]]",
        '        - [19] AXMenuItem "Mac User Guide" [help="Opens the guide." actions=[cancel,press,pick]]',
    ]
)


def shown(tree):
    return [int(l.split("[", 1)[1].split("]")[0]) for l in tree.splitlines() if l.lstrip().startswith("- [")]


class ElementsTree(unittest.TestCase):
    def setUp(self):
        self.m = load_controller()

    def test_the_labelled_view_keeps_controls_text_and_state(self):
        tree = self.m.render_elements_tree(ELEMENTS, {}, MARKDOWN)
        # Labelled nodes, unlabeled controls, the selected row, the pane's
        # text; not the other unlabeled rows, groups, or a static text that
        # repeats what stands above it.
        self.assertEqual(shown(tree), [0, 1, 3, 4, 5, 7, 8, 9, 20, 11, 13, 17])
        self.assertIn("- [4] AXRow *selected*", tree)
        self.assertIn("- [7] AXButton (Displays) (disabled)", tree)
        self.assertIn("- [83] AXButton (Auto) *selected*", self.m.render_elements_tree(
            [element(83, "AXButton", 2, label="Auto", actions=PRESS, selected=True)], {}))
        # A value equal to the label is not repeated; an id names an unlabeled node.
        self.assertIn("- [8] AXTextField (Search)\n", tree)
        self.assertIn("- [9] AXButton id=com.apple.settings.addAccount", tree)
        self.assertNotIn("id=com.apple.settings.general", tree)
        # Text-only rows keep their place, without an index to click.
        lines = tree.splitlines()
        self.assertIn("   - AXHeading (Theme)", lines)
        self.assertIn('   - AXStaticText = "Colour"', lines)
        self.assertIn('   - AXStaticText = "Multicolour"', lines)
        self.assertEqual(lines.index('   - AXStaticText = "Colour"') + 1, lines.index('   - AXStaticText = "Multicolour"'))
        self.assertEqual(tree.count('"Multicolour"'), 1, "the nested repeat is folded")
        # Indent is the element's depth.
        self.assertIn("\n     - [3] AXButton (General)", tree)

    def test_the_markdown_rows_parse_with_their_attributes(self):
        rows = {(r["index"], r["role"]): r for r in self.m.parse_markdown_rows(MARKDOWN)}
        self.assertEqual(rows[(3, "AXButton")]["label"], "General")
        self.assertEqual(rows[(3, "AXButton")]["id"], "com.apple.settings.general")
        self.assertEqual(rows[(3, "AXButton")]["actions"], ["press"])
        self.assertEqual(rows[(3, "AXButton")]["depth"], 5)
        self.assertEqual(rows[(8, "AXTextField")]["value"], "Search")
        self.assertEqual(rows[(13, "AXMenuBarItem")]["label"], "File")
        self.assertEqual(rows[(19, "AXMenuItem")]["actions"], ["cancel", "press", "pick"])
        self.assertEqual(rows[(None, "AXHeading")]["label"], "Theme")
        self.assertEqual(rows[(None, "AXStaticText")]["value"], "Multicolour")
        self.assertEqual(self.m.element_ids_from_markdown(MARKDOWN)[9], "com.apple.settings.addAccount")

    def test_without_markdown_the_tree_comes_from_the_elements_alone(self):
        tree = self.m.render_elements_tree(ELEMENTS, {})
        self.assertEqual(shown(tree), [0, 1, 3, 4, 5, 7, 8, 9, 20, 11, 13, 17])
        self.assertNotIn("Theme", tree)

    def test_menus_collapse_to_their_menu_bar_items(self):
        tree = self.m.render_elements_tree(ELEMENTS, {}, MARKDOWN)
        self.assertIn("- [13] AXMenuBarItem (File)", tree)
        self.assertNotIn("Close Window", tree)
        head = tree.splitlines()[0]
        self.assertIn("21 elements", head)
        self.assertIn("16 lines shown", head)
        self.assertIn("3 menu items collapsed", head)
        self.assertIn('"snapshot_id"', head)
        # A query or a roles view reaches into the menus.
        self.assertIn("- [16] AXMenuItem (Export…)", self.m.render_elements_tree(ELEMENTS, {"query": "export"}, MARKDOWN))
        self.assertEqual(shown(self.m.render_elements_tree(ELEMENTS, {"roles": ["AXMenuItem"]}, MARKDOWN)), [15, 16, 19])

    def test_a_roles_usual_action_set_is_implied_and_a_different_one_is_shown(self):
        tree = self.m.render_elements_tree(ELEMENTS, {}, MARKDOWN)
        self.assertNotIn("actions=[press]", tree)
        self.assertNotIn("actions=[cancel,press,pick]", tree)
        self.assertIn("- [0] AXWindow (Main)\n", tree)
        self.assertIn("- [20] AXButton (Go Back) actions=[press,showmenu]", tree)

    def test_query_keeps_matches_with_their_descendants_and_max_lines_bounds(self):
        tree = self.m.render_elements_tree(ELEMENTS, {"query": "appearance"}, MARKDOWN)
        self.assertEqual(shown(tree), [5, 11])
        self.assertIn('- AXStaticText = "Appearance"', tree)
        tree = self.m.render_elements_tree(ELEMENTS, {"max_lines": 2}, MARKDOWN)
        self.assertEqual(shown(tree), [0, 1])
        self.assertIn("truncated", tree.splitlines()[0])

    def test_a_custom_actions_descriptor_dump_is_not_an_action_name(self):
        rows = [element(23, "AXButton", 2, label="Mode", actions=[
            "AXIncrement", "AXDecrement", "AXDelete", "AXShowMenu", "AXCancel", "AXPress",
            "Name:Move previous\nTarget:0x0\nSelector:(null)", "Name:Remove from toolbar\nTarget:0x0\nSelector:(null)"]),
            element(24, "AXButton", 2, label="Clear", actions=PRESS)]
        tree = self.m.render_elements_tree(rows, {})
        self.assertNotIn("Target", tree)
        self.assertNotIn("\n\n", tree)
        self.assertIn("- [23] AXButton (Mode) actions=[increment,decrement,delete,showmenu,cancel,press]", tree)

    def test_the_full_filter_shows_every_node(self):
        tree = self.m.render_elements_tree(ELEMENTS, {"filter": "full"}, MARKDOWN)
        self.assertEqual(shown(tree), [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 20, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19])
        self.assertEqual(len(tree.splitlines()), 1 + 26)


class SnapshotSidecar(unittest.TestCase):
    def setUp(self):
        self.m = load_controller()

    def reply(self):
        return {
            "pid": 675, "window_id": 52, "snapshot_id": "s0000000a", "element_count": 21,
            "elements": ELEMENTS, "tree_markdown": MARKDOWN, "screenshot_width": 1568,
            "screenshot_png_b64": "AAAA", "_note": "Prefer `elements`",
        }

    def test_the_reply_carries_the_rendered_tree_and_the_elements_go_to_the_sidecar(self):
        with tempfile.TemporaryDirectory() as folder:
            sidecar = os.path.join(folder, "shot.json")
            out = json.loads(self.m.compact_get_window_state(json.dumps(self.reply()), {}, sidecar))
            self.assertEqual(out["snapshot_file"], sidecar)
            self.assertNotIn("elements", out)
            self.assertIn("snapshot_file", out["_note"])
            self.assertEqual(out["tree_view"], "labelled")
            self.assertIn("- [5] AXButton (Appearance)", out["tree_markdown"])
            self.assertEqual(out["element_count"], 21)
            stored = json.load(open(sidecar))
            self.assertEqual(len(stored["elements"]), 21)
            self.assertEqual(stored["tree_markdown"], MARKDOWN)
            self.assertNotIn("screenshot_png_b64", stored)

    def test_without_a_sidecar_the_elements_stay_inline(self):
        out = json.loads(self.m.compact_get_window_state(json.dumps(self.reply()), {}))
        self.assertEqual(len(out["elements"]), 21)
        self.assertNotIn("snapshot_file", out)
        self.assertIn("- [5] AXButton (Appearance)", out["tree_markdown"])

    def test_the_raw_full_view_is_the_drivers_reply_plus_the_sidecar(self):
        with tempfile.TemporaryDirectory() as folder:
            sidecar = os.path.join(folder, "shot.json")
            out = json.loads(self.m.compact_get_window_state(json.dumps(self.reply()), {"filter": "full"}, sidecar))
            self.assertEqual(out["tree_markdown"], MARKDOWN)
            self.assertEqual(len(out["elements"]), 21)
            self.assertEqual(out["snapshot_file"], sidecar)
            self.assertEqual(out["tree_view"], "full")

    def test_a_reply_without_elements_still_compacts_its_markdown(self):
        reply = {"pid": 1, "tree_markdown": "- [0] AXWindow\n  - [1] AXGroup\n    - [2] AXButton (Go)"}
        out = json.loads(self.m.compact_get_window_state(json.dumps(reply), {}))
        self.assertNotIn("[1] AXGroup", out["tree_markdown"])
        self.assertIn("[2] AXButton (Go)", out["tree_markdown"])

    def test_the_sidecar_sits_beside_the_screenshot_or_under_the_captures_directory(self):
        c = self.m
        self.assertEqual(c.snapshot_out_file_for("get_window_state", "{}", "/tmp/caps/x.png"), "/tmp/caps/x.json")
        with tempfile.TemporaryDirectory() as folder:
            os.environ[c.CAPTURES_DIR_ENV] = folder
            try:
                path = c.snapshot_out_file_for("get_window_state", '{"pid":675,"window_id":52,"include_screenshot":false}', None)
            finally:
                del os.environ[c.CAPTURES_DIR_ENV]
        self.assertTrue(path.startswith(folder) and path.endswith("-675-52.json"), path)
        self.assertIsNone(c.snapshot_out_file_for("click", '{"pid":675,"window_id":52}', None))
        self.assertIsNone(c.snapshot_out_file_for("get_window_state", '{"pid":675}', None))


if __name__ == "__main__":
    unittest.main()


class InvocationLog(unittest.TestCase):
    """One line per call, so tool use counts the same whoever drove it."""

    def setUp(self):
        self.m = load_controller()

    def test_typed_text_is_counted_but_never_written(self):
        args = json.dumps({"pid": 7, "window_id": 3, "text": "hunter2", "element_index": 4})
        self.assertEqual(
            self.m.redacted_args(args),
            {"pid": 7, "window_id": 3, "text": "<redacted:7 chars>", "element_index": 4},
        )
        nested = self.m.redacted_args(json.dumps({"pid": 7, "roles": ["AXButton"], "value": "secret"}))
        self.assertEqual(nested, {"pid": 7, "roles": "<list>", "value": "<redacted:6 chars>"})
        self.assertEqual(self.m.redacted_args("not json"), {"unparsed_chars": 8})

    def test_an_entry_lands_only_when_the_log_is_named(self):
        import time as _time
        with tempfile.TemporaryDirectory() as folder:
            path = os.path.join(folder, "nested", "bench.jsonl")
            os.environ[self.m.BENCH_LOG_ENV] = path
            os.environ[self.m.BENCH_RUN_ENV] = "arm-1"
            try:
                self.m.log_invocation("click", '{"pid":7,"text":"abc"}', _time.monotonic(), 0)
                self.m.log_invocation("get_window_state", '{"pid":7}', _time.monotonic(), 1)
            finally:
                del os.environ[self.m.BENCH_LOG_ENV], os.environ[self.m.BENCH_RUN_ENV]
            lines = [json.loads(l) for l in open(path)]
        self.assertEqual([l["action"] for l in lines], ["click", "get_window_state"])
        self.assertEqual({l["run"] for l in lines}, {"arm-1"})
        self.assertEqual(lines[0]["args"]["text"], "<redacted:3 chars>")
        self.assertEqual(lines[1]["exit_code"], 1)
        self.assertNotIn("abc", json.dumps(lines))
        # Unset: nothing is written and nothing raises.
        self.m.log_invocation("click", "{}", _time.monotonic(), 0)


class BenchSettings(unittest.TestCase):
    """The log is configurable by file, since a governed child inherits no env."""

    def setUp(self):
        self.m = load_controller()
        for key in (self.m.BENCH_LOG_ENV, self.m.BENCH_RUN_ENV, self.m.CAPTURES_DIR_ENV):
            os.environ.pop(key, None)

    def tearDown(self):
        for key in (self.m.BENCH_LOG_ENV, self.m.BENCH_RUN_ENV, self.m.CAPTURES_DIR_ENV):
            os.environ.pop(key, None)

    def test_the_config_file_beside_the_captures_directory_turns_logging_on(self):
        with tempfile.TemporaryDirectory() as folder:
            os.environ[self.m.CAPTURES_DIR_ENV] = os.path.join(folder, "caps")
            with open(os.path.join(folder, self.m.BENCH_CONFIG_FILE), "w") as handle:
                json.dump({"log": f"{folder}/tools.jsonl", "run": "A2-fable"}, handle)
            self.assertEqual(self.m.bench_settings(), (f"{folder}/tools.jsonl", "A2-fable"))
            # The environment still wins when it is set.
            os.environ[self.m.BENCH_LOG_ENV] = "/tmp/from-env.jsonl"
            os.environ[self.m.BENCH_RUN_ENV] = "A3-claude"
            self.assertEqual(self.m.bench_settings(), ("/tmp/from-env.jsonl", "A3-claude"))

    def test_no_file_and_no_env_means_no_logging(self):
        with tempfile.TemporaryDirectory() as folder:
            os.environ[self.m.CAPTURES_DIR_ENV] = os.path.join(folder, "caps")
            self.assertEqual(self.m.bench_settings(), ("", ""))
            for body in ("not json", '"a string"', '{"run":"x"}', '{"log":"   "}'):
                with open(os.path.join(folder, self.m.BENCH_CONFIG_FILE), "w") as handle:
                    handle.write(body)
                self.assertEqual(self.m.bench_settings()[0], "", body)


class UnknownActionRefusal(unittest.TestCase):
    """The driver rejects a name it does not have as a risk-classification
    problem; the reply should say it is an unknown tool and name the real one."""

    def setUp(self):
        self.m = load_controller()

    def refusal(self, action, text):
        import subprocess
        result = subprocess.CompletedProcess([action], 1, stdout="", stderr=text)
        self.m.explain_unknown_action(action, result)
        return result.stderr

    def test_an_invented_focus_action_is_named_as_unknown_and_redirected(self):
        out = self.refusal("activate_app", "Permission denied: tool 'activate_app' has no reviewed risk classification")
        self.assertIn("`activate_app` is not a cua-driver tool", out)
        self.assertIn("use `bring_to_front` instead", out)
        self.assertIn("bring_to_front", self.refusal("focus_window", "Permission denied: tool 'focus_window' has no reviewed risk classification"))
        # `screenshot` is the other name agents reach for; the window snapshot
        # already writes one to disk.
        self.assertIn("get_window_state", self.refusal("screenshot", "Permission denied: tool 'screenshot' has no reviewed risk classification"))

    def test_an_unrelated_failure_is_left_exactly_as_the_driver_reported_it(self):
        self.assertEqual(self.refusal("click", "No cached AX state for pid 1"), "No cached AX state for pid 1")
