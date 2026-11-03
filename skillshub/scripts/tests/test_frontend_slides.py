"""Offline native packaging checks for the bounded Frontend Slides procedure."""
from pathlib import Path
import hashlib
import json
import re
import sys
import unittest

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))
from validate_skill_md import validate
ROOT = SCRIPTS.parent / "frontend-slides"


class FrontendSlidesTests(unittest.TestCase):
    def test_native_procedure_contract(self):
        self.assertEqual(validate(ROOT), [])
        text = (ROOT / "SKILL.md").read_text()
        self.assertIn("skill_type: procedure", text)
        for forbidden in ("runtime_actions:", "runtime_contract:"):
            self.assertNotIn(forbidden, text)
        for directory in ("bin", "scripts", "bold-template-pack", ".claude-plugin"):
            self.assertFalse((ROOT / directory).exists())

    def test_upstream_reference_integrity(self):
        manifest = json.loads((ROOT / "UPSTREAM.json").read_text())
        self.assertEqual(manifest["commit"], "9906a34d640d2111f724544cbc50f7f130569ae1")
        self.assertEqual(set(manifest["sha256"]), {"STYLE_PRESETS.md", "html-template.md", "animation-patterns.md", "viewport-base.css", "LICENSE"})
        for name, digest in manifest["sha256"].items():
            with self.subTest(name=name):
                self.assertEqual(hashlib.sha256((ROOT / name).read_bytes()).hexdigest(), digest)

    def test_entrypoint_reference_closure_and_scope(self):
        for name in ("SKILL.md", "PROVENANCE.md"):
            for target in re.findall(r"\[[^\]]*\]\(([^)]+)\)", (ROOT / name).read_text()):
                if "://" not in target:
                    resolved = (ROOT / target.split("#")[0]).resolve()
                    self.assertTrue(resolved.is_relative_to(ROOT.resolve()))
                    self.assertTrue(resolved.is_file())
        text = (ROOT / "PROVENANCE.md").read_text()
        self.assertIn("SCOPE=anonymous/default NAMES=frontend-slides", text)
        self.assertIn("no restart is required", text)


if __name__ == "__main__":
    unittest.main()
