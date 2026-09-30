"""A pid-only `get_window_state` reads the app's main window.

`launch_app` returns a pid, and a `hotkey`/`press_key` names only the app, so
act→observe often has no window_id. `list_windows` lists hidden helper windows
too (TextEdit: five off-screen 64x64 / 1312x26 windows); the controller picks
the on-screen, layer-0, largest one and names it in the reply, and fails
plainly when the app shows none.
"""
from __future__ import annotations

import importlib.machinery
import importlib.util
import json
import os
import subprocess
import unittest

CONTROLLER = os.path.join(
    os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "bin", "macos-ui-controller"
)


def load_controller():
    loader = importlib.machinery.SourceFileLoader("macos_ui_controller_main_window", CONTROLLER)
    spec = importlib.util.spec_from_loader("macos_ui_controller_main_window", loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


def window(window_id, width, height, on_screen=True, layer=0, z=0):
    return {
        "window_id": window_id,
        "bounds": {"width": width, "height": height, "x": 0, "y": 0},
        "is_on_screen": on_screen,
        "layer": layer,
        "z_index": z,
    }


class MainWindowTest(unittest.TestCase):
    def setUp(self):
        self.controller = load_controller()

    def test_the_largest_on_screen_window_wins(self):
        windows = [
            window(2654, 64, 64, on_screen=False, z=101),
            window(2653, 1312, 26, on_screen=False, z=100),
            window(3364, 800, 600, z=5),
            window(3365, 300, 200, z=9),
            window(9000, 2000, 2000, layer=25),
        ]
        self.assertEqual(self.controller.main_window_id(windows), 3364)

    def test_no_on_screen_window_is_none(self):
        windows = [window(2654, 64, 64, on_screen=False), window(2653, 1312, 26, on_screen=False)]
        self.assertIsNone(self.controller.main_window_id(windows))

    def test_a_pid_only_request_gains_the_main_window(self):
        listed = {"windows": [window(2654, 64, 64, on_screen=False), window(3364, 800, 600)]}
        self.controller.run = lambda cmd, timeout=None: subprocess.CompletedProcess(
            cmd, 0, json.dumps(listed), ""
        )
        args, pid, error = self.controller.with_main_window('{"pid":10392}')
        self.assertEqual((json.loads(args), pid, error), ({"pid": 10392, "window_id": 3364}, 10392, None))
        reply = self.controller.name_window('{"tree_markdown":"x"}', 10392, 3364)
        self.assertEqual(json.loads(reply)["window_id"], 3364)

    def test_an_app_with_no_window_on_screen_fails_plainly(self):
        listed = {"windows": [window(2654, 64, 64, on_screen=False)]}
        self.controller.run = lambda cmd, timeout=None: subprocess.CompletedProcess(
            cmd, 0, json.dumps(listed), ""
        )
        _, pid, error = self.controller.with_main_window('{"pid":10392}')
        self.assertEqual(pid, 10392)
        self.assertIn("no on-screen window", error)

    def test_a_named_window_is_left_alone(self):
        self.controller.run = lambda *a, **k: self.fail("list_windows must not run")
        args, pid, error = self.controller.with_main_window('{"pid":1,"window_id":2}')
        self.assertEqual((args, pid, error), ('{"pid":1,"window_id":2}', None, None))


if __name__ == "__main__":
    unittest.main()
