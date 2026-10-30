"""Exercise real argv/stdin/process behavior with local fake CLIs; no API calls."""
import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

loader = importlib.machinery.SourceFileLoader("ocr_under_test", str(Path(__file__).resolve().parents[1] / "bin/ocr"))
spec = importlib.util.spec_from_loader(loader.name, loader)
ocr = importlib.util.module_from_spec(spec)
loader.exec_module(ocr)

FAKE_CLI = r'''
import json, os, sys, time
from pathlib import Path
args = sys.argv[1:]
if os.environ.get("OCR_TEST_GROUP"):
    Path(os.environ["OCR_TEST_GROUP"]).write_text(str(os.getpgrp()))
Path(os.environ["OCR_TEST_ARGS"]).write_text(json.dumps(args))
if os.environ.get("OCR_TEST_FAIL"):
    print("invalid model selection: requested model is unavailable", file=sys.stderr)
    sys.exit(7)
if os.environ.get("OCR_TEST_SLEEP"):
    Path(os.environ["OCR_TEST_PID"]).write_text(str(os.getpid()))
    time.sleep(30)
if "exec" in args:
    assert "--skip-git-repo-check" in args
    assert args[args.index("--sandbox") + 1] == "read-only"
    assert args[-2:] == ["--", "-"]
    prompt = sys.stdin.read()
    assert prompt
    Path(args[args.index("--output-last-message") + 1]).write_text("read: " + prompt)
else:
    print("read from engine default")
'''


class CliEngineTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        for name in ["codex", "agy"]:
            cli = self.root / name
            cli.write_text("#!" + sys.executable + "\n" + FAKE_CLI)
            cli.chmod(0o700)
        self.argv = self.root / "argv.json"
        self.env = patch.dict(os.environ, {"PATH": str(self.root), "OCR_TEST_ARGS": str(self.argv)}, clear=False)
        self.env.start()
        self.addCleanup(self.env.stop)

    def test_codex_receives_literal_prompt_over_closed_stdin(self):
        prompt = "Read {every} word; do not evaluate $(anything)"
        self.assertEqual(ocr.codex_text(self.root / "image with spaces.png", prompt, ""), "read: " + prompt)
        args = json.loads(self.argv.read_text())
        self.assertNotIn(prompt, args)
        self.assertNotIn("-m", args)
        self.assertEqual(args[args.index("-i") + 1], str(self.root / "image with spaces.png"))

    def test_agy_uses_engine_default_and_preserves_prompt_braces(self):
        self.assertEqual(ocr.agy_text(self.root / "image.png", "Read {target}; output {literal}", ""), "read from engine default")
        args = json.loads(self.argv.read_text())
        self.assertNotIn("--model", args)
        self.assertEqual(args[args.index("--print-timeout") + 1], "0")
        self.assertIn("Read @" + str(self.root / "image.png") + "; output {literal}", args)
        parsed = ocr.build_parser().parse_args(["extract", "--input-file", "image.png", "--engine", "agy"])
        self.assertEqual(parsed.agy_model, "")
        self.assertIsNone(parsed.timeout_secs)

    def test_explicit_model_is_preserved_and_rejection_keeps_diagnostics(self):
        with patch.dict(os.environ, {"OCR_TEST_FAIL": "1"}):
            with self.assertRaisesRegex(RuntimeError, "status 7: invalid model selection"):
                ocr.agy_text(self.root / "image.png", "Read it", "unavailable-model")
        args = json.loads(self.argv.read_text())
        self.assertEqual(args[args.index("--model") + 1], "unavailable-model")

    @unittest.skipUnless(os.name == "posix", "POSIX process group ownership")
    def test_runtime_workers_inherit_the_group_that_owns_cancellation(self):
        group_file = self.root / "group"
        with patch.dict(os.environ, {"OCR_TEST_GROUP": str(group_file), "MAGICIAN_MANAGED_SHELL": "0"}):
            ocr.run([str(self.root / "agy")])
            self.assertNotEqual(int(group_file.read_text()), os.getpgrp())
            with patch.dict(os.environ, {"MAGICIAN_MANAGED_SHELL": "1"}):
                ocr.run([str(self.root / "agy")])
                self.assertEqual(int(group_file.read_text()), os.getpgrp())

    @unittest.skipUnless(os.name == "posix", "POSIX process group cleanup")
    def test_explicit_timeout_terminates_and_reaps_the_worker(self):
        pid_file = self.root / "pid"
        with patch.dict(os.environ, {"OCR_TEST_SLEEP": "1", "OCR_TEST_PID": str(pid_file)}):
            with self.assertRaisesRegex(RuntimeError, "explicitly requested"):
                ocr.run([str(self.root / "agy")], timeout_secs=0.25)
        pid = int(pid_file.read_text())
        with self.assertRaises(ProcessLookupError):
            os.kill(pid, 0)
        self.assertFalse(ocr._children)


if __name__ == "__main__":
    unittest.main()
