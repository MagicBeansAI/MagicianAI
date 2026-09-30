"""Controller behaviour that follows from cua-driver 0.28's contract.

Each `cua-driver call` is its own transport, so a call without a `session`
label runs in a fresh implicit session and the agent cursor's settings die with
it; every `call` needs the daemon, reads included; an element address is scoped
to the snapshot that minted it, so resending after a refresh cannot succeed; and
a full-display capture inlines ~1.8 MB of base64 unless it goes to a file.
"""
from __future__ import annotations

import importlib.machinery
import importlib.util
import json
import os
import subprocess
import tempfile
import unittest
from unittest.mock import patch

CONTROLLER = os.path.join(
    os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "bin", "macos-ui-controller"
)


def load_controller():
    loader = importlib.machinery.SourceFileLoader("macos_ui_controller_028", CONTROLLER)
    spec = importlib.util.spec_from_loader("macos_ui_controller_028", loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


def completed(code=0, stdout="", stderr=""):
    return subprocess.CompletedProcess([], code, stdout, stderr)


class Session(unittest.TestCase):
    def setUp(self):
        self.controller = load_controller()

    def test_a_session_tool_gets_the_shared_label(self):
        with patch.dict(os.environ, {}, clear=True):
            args = json.loads(self.controller.with_session("set_agent_cursor_motion", '{"glide_duration_ms":750}'))
        self.assertEqual(args, {"glide_duration_ms": 750, "session": "magician"})

    def test_the_env_label_wins_and_a_caller_label_is_kept(self):
        with patch.dict(os.environ, {"MAGICIAN_CUA_SESSION": "tutor-7"}):
            self.assertEqual(json.loads(self.controller.with_session("click", "{}"))["session"], "tutor-7")
            self.assertEqual(json.loads(self.controller.with_session("click", '{"session":"mine"}'))["session"], "mine")

    def test_tools_without_a_session_and_end_session_are_untouched(self):
        for action in ["list_windows", "bring_to_front", "zoom", "end_session"]:
            self.assertEqual(self.controller.with_session(action, '{"pid":1}'), '{"pid":1}')


class Dispatch(unittest.TestCase):
    def setUp(self):
        self.controller = load_controller()

    def dispatch(self, action, result):
        c = self.controller
        with patch.object(c, "local_cua_driver_available", return_value=True), \
                patch.object(c, "ensure_daemon", return_value=True) as ensure, \
                patch.object(c, "call_cua", return_value=result) as call, \
                patch.object(c, "known_driver_tools", return_value=set()), \
                patch.object(c, "print_completed") as printed:
            code = c.dispatch(action, '{"pid":1,"window_id":2,"snapshot_id":"s00000001","element_index":3}', None)
        return code, ensure, call, printed

    def test_reads_start_the_daemon_too(self):
        for action in ["check_permissions", "get_config", "set_config"]:
            _, ensure, _, _ = self.dispatch(action, completed())
            ensure.assert_called_once()

    def test_a_stale_address_is_not_retried_and_says_how_to_recover(self):
        for marker in ["snapshot_id_required", "stale_element_token", "No cached AX state for pid 1"]:
            result = completed(1, stderr=marker)
            code, _, call, _ = self.dispatch("click", result)
            self.assertEqual(code, 1)
            call.assert_called_once()
            self.assertIn("Re-run get_window_state", result.stderr)


class CaptureFiles(unittest.TestCase):
    def setUp(self):
        self.controller = load_controller()

    def test_desktop_and_zoom_captures_default_to_files(self):
        with tempfile.TemporaryDirectory() as folder, patch.dict(os.environ, {self.controller.CAPTURES_DIR_ENV: folder}):
            desktop = self.controller.default_screenshot_out_file("get_desktop_state", "{}")
            zoom = self.controller.default_screenshot_out_file("zoom", '{"pid":1,"window_id":2}')
        self.assertTrue(desktop.startswith(folder) and desktop.endswith("-get_desktop_state.png"))
        self.assertTrue(zoom.endswith("-zoom.jpg"))

    def test_a_zoom_reply_names_its_file(self):
        with tempfile.TemporaryDirectory() as folder:
            crop = os.path.join(folder, "crop.jpg")
            open(crop, "wb").write(b"jpeg")
            reply = json.dumps({"format": "jpeg", "mime_type": "image/jpeg", "width": 358, "height": 238})
            named = json.loads(self.controller.name_screenshot_file(reply, crop))
        self.assertEqual(named["screenshot_file"], crop)
        self.assertNotIn("screenshot_mime_type", named)

    def test_a_desktop_reply_drops_the_inline_image(self):
        with tempfile.TemporaryDirectory() as folder:
            shot = os.path.join(folder, "desk.png")
            open(shot, "wb").write(b"png")
            reply = json.dumps({"screenshot_width": 3360, "screenshot_height": 1890, "screenshot_png_b64": "AAAA"})
            named = json.loads(self.controller.name_screenshot_file(reply, shot))
        self.assertNotIn("screenshot_png_b64", named)
        self.assertEqual(named["screenshot_file"], shot)


if __name__ == "__main__":
    unittest.main()
