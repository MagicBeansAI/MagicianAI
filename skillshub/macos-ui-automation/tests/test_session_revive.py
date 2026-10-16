import importlib.machinery
import importlib.util
import json
import os
import subprocess
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
CONTROLLER = os.path.join(HERE, "..", "bin", "macos-ui-controller")


def load_controller():
    loader = importlib.machinery.SourceFileLoader("macos_ui_controller_revive", CONTROLLER)
    spec = importlib.util.spec_from_loader(loader.name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


def completed(stderr="", code=1):
    return subprocess.CompletedProcess(args=[], returncode=code, stdout="", stderr=stderr)


ENDED = "session 'magician' has ended; tool call 'click' was rejected."


class SessionReviveTest(unittest.TestCase):
    def setUp(self):
        self.controller = load_controller()
        self.revived = []

        def run(cmd, timeout=None):
            self.revived.append(cmd)
            return completed(code=0)

        self.controller.run = run
        self.controller.driver_command = lambda *parts: list(parts)

    def test_the_shared_default_label_is_revived_once(self):
        original = json.dumps({"pid": 1})
        labelled = self.controller.with_session("click", original)
        self.assertTrue(self.controller.revive_default_session(
            "click", original, labelled, completed(ENDED)))
        self.assertEqual(self.revived, [["call", "start_session", '{"session": "magician"}']])

    def test_a_session_the_caller_named_is_left_to_the_caller(self):
        named = json.dumps({"pid": 1, "session": "magician"})
        self.assertFalse(self.controller.revive_default_session(
            "click", named, named, completed(ENDED)))
        self.assertEqual(self.revived, [])

    def test_other_failures_are_not_retried(self):
        original = json.dumps({"pid": 1})
        labelled = self.controller.with_session("click", original)
        self.assertFalse(self.controller.revive_default_session(
            "click", original, labelled, completed("element not found")))
        self.assertEqual(self.revived, [])


if __name__ == "__main__":
    unittest.main()
