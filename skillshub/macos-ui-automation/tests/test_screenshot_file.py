"""A window snapshot's screenshot goes to disk and the reply names the file.

`get_window_state` always returns a screenshot, and without
`--screenshot-out-file` the driver inlines it as `screenshot_png_b64` — ~350 KB
of base64 ahead of `tree_markdown`. The agentic loop pages a tool result past
~6k tokens, so the model's first page was base64 and the tree was pages away,
and no image reached the model, which attaches a capture only from a file.
The phone lane hit the same wall. Every snapshot is now captured to a file —
the caller's `screenshot_out_file`, else one under the captures directory —
and the reply carries `screenshot_file` beside the click-space dimensions.
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


class ScreenshotFile(unittest.TestCase):
    def setUp(self):
        self.controller = load_controller()

    def test_a_window_snapshot_gets_a_capture_path_under_the_captures_directory(self):
        with tempfile.TemporaryDirectory() as folder:
            os.environ[self.controller.CAPTURES_DIR_ENV] = folder
            try:
                path = self.controller.default_screenshot_out_file(
                    "get_window_state", '{"pid":675,"window_id":52}'
                )
            finally:
                del os.environ[self.controller.CAPTURES_DIR_ENV]
        self.assertIsNotNone(path)
        self.assertTrue(path.startswith(folder))
        self.assertTrue(path.endswith("-675-52.png"))

    def test_only_a_snapshot_that_names_a_window_and_wants_a_screenshot_is_captured(self):
        c = self.controller
        self.assertIsNone(c.default_screenshot_out_file("click", '{"pid":1,"window_id":2,"x":3,"y":4}'))
        self.assertIsNone(c.default_screenshot_out_file("get_window_state", '{"pid":1}'))
        self.assertIsNone(
            c.default_screenshot_out_file("get_window_state", '{"pid":1,"window_id":2,"include_screenshot":false}')
        )
        self.assertIsNone(c.default_screenshot_out_file("get_window_state", "not json"))

    def test_the_reply_names_the_file_and_drops_any_inline_image(self):
        with tempfile.TemporaryDirectory() as folder:
            capture = os.path.join(folder, "shot.png")
            with open(capture, "wb") as handle:
                handle.write(b"\x89PNG")
            reply = json.dumps(
                {
                    "pid": 675,
                    "window_id": 52,
                    "screenshot_width": 1568,
                    "screenshot_height": 743,
                    "screenshot_png_b64": "AAAA",
                    "tree_markdown": "- [0] AXWindow",
                }
            )
            named = json.loads(self.controller.name_screenshot_file(reply, capture))
            self.assertEqual(named["screenshot_file"], capture)
            self.assertEqual(named["screenshot_mime_type"], "image/png")
            self.assertNotIn("screenshot_png_b64", named)
            self.assertEqual(named["screenshot_width"], 1568)

            # A reply whose capture never landed on disk is left as it is.
            missing = self.controller.name_screenshot_file(reply, os.path.join(folder, "absent.png"))
            self.assertEqual(json.loads(missing), json.loads(reply))
            # A reply without a screenshot (tree only) is left as it is.
            tree_only = json.dumps({"pid": 675, "tree_markdown": "- [0] AXWindow"})
            self.assertEqual(self.controller.name_screenshot_file(tree_only, capture), tree_only)


class DriverHome(unittest.TestCase):
    def test_a_missing_home_is_derived_from_the_passwd_database(self):
        controller = load_controller()
        saved = os.environ.pop("HOME", None)
        try:
            controller.ensure_home_for_driver()
            self.assertTrue(os.environ.get("HOME"), "HOME is set for the driver")
            self.assertTrue(os.path.isdir(os.environ["HOME"]))
        finally:
            if saved is not None:
                os.environ["HOME"] = saved

    def test_an_existing_home_is_left_alone(self):
        controller = load_controller()
        saved = os.environ.get("HOME")
        os.environ["HOME"] = "/tmp/magician-home-under-test"
        try:
            controller.ensure_home_for_driver()
            self.assertEqual(os.environ["HOME"], "/tmp/magician-home-under-test")
        finally:
            if saved is not None:
                os.environ["HOME"] = saved


if __name__ == "__main__":
    unittest.main()
