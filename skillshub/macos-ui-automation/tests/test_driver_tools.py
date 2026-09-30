"""The controller publishes the driver's own tool schemas for the host.

The agent's tool takes an opaque `args_json`; the driver's MCP `tools/list` is
the only declaration of each action's parameters. The controller writes it to
a file the host's step judge reads, and rewrites it only when the driver's
version changes; a failed refresh keeps the previous file.
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
    loader = importlib.machinery.SourceFileLoader("macos_ui_controller_driver_tools", CONTROLLER)
    spec = importlib.util.spec_from_loader("macos_ui_controller_driver_tools", loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


TOOLS = [
    {"name": "click", "description": "Left-click", "inputSchema": {"properties": {"element_token": {}}},
     "annotations": {"readOnlyHint": False}},
    {"name": "press_key", "description": "Press", "inputSchema": {"required": ["key"]}},
    {"name": "get_window_state", "description": "Snapshot", "inputSchema": {},
     "annotations": {"readOnlyHint": True}},
]


class DriverToolsTest(unittest.TestCase):
    def setUp(self):
        self.controller = load_controller()
        self.dir = tempfile.TemporaryDirectory()
        self.path = os.path.join(self.dir.name, "tools.json")
        os.environ[self.controller.DRIVER_TOOLS_ENV] = self.path
        self.listed = 0

        def listing():
            self.listed += 1
            return TOOLS

        self.controller.driver_tool_list = listing

    def tearDown(self):
        os.environ.pop(self.controller.DRIVER_TOOLS_ENV, None)
        self.dir.cleanup()

    def read(self):
        with open(self.path, encoding="utf-8") as handle:
            return json.load(handle)

    def test_a_new_driver_version_is_published_once(self):
        self.controller.driver_version = lambda: "cua-driver 0.28.2"
        self.controller.refresh_driver_tools()
        published = self.read()
        self.assertEqual(published["driver_version"], "cua-driver 0.28.2")
        self.assertEqual([t["name"] for t in published["tools"]], ["click", "press_key", "get_window_state"])
        # The driver's own readOnlyHint; a tool without one is not read-only.
        self.assertEqual([t["read_only"] for t in published["tools"]], [False, False, True])
        self.assertEqual(published["tools"][1]["input_schema"], {"required": ["key"]})
        self.controller.refresh_driver_tools()
        self.assertEqual(self.listed, 1, "the same version is not listed again")
        self.controller.driver_version = lambda: "cua-driver 0.29.0"
        self.controller.refresh_driver_tools()
        self.assertEqual(self.listed, 2)
        self.assertEqual(self.read()["driver_version"], "cua-driver 0.29.0")

    def test_a_file_from_an_older_controller_is_rewritten(self):
        self.controller.driver_version = lambda: "cua-driver 0.28.2"
        with open(self.path, "w", encoding="utf-8") as handle:
            json.dump({"driver_version": "cua-driver 0.28.2", "tools": []}, handle)
        self.controller.refresh_driver_tools()
        self.assertEqual(self.listed, 1)
        self.assertEqual(self.read()["format"], self.controller.DRIVER_TOOLS_FORMAT)

    def test_a_failed_refresh_keeps_the_previous_file(self):
        self.controller.driver_version = lambda: "cua-driver 0.28.2"
        self.controller.refresh_driver_tools()
        self.controller.driver_version = lambda: "cua-driver 0.29.0"
        self.controller.driver_tool_list = lambda: None
        self.controller.refresh_driver_tools()
        self.assertEqual(self.read()["driver_version"], "cua-driver 0.28.2")


if __name__ == "__main__":
    unittest.main()
