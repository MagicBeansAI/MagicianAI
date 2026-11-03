"""Offline packaging and discovery contract for the Impeccable procedure."""
from pathlib import Path
import re
import sys
import unittest

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))
from validate_skill_md import validate

ROOT = SCRIPTS.parent / "impeccable"


class ImpeccableTests(unittest.TestCase):
    def test_native_procedure_contract(self):
        self.assertEqual(validate(ROOT), [])
        text = (ROOT / "SKILL.md").read_text()
        self.assertIn("skill_type: procedure", text)
        self.assertNotIn("runtime_actions:", text)
        self.assertNotIn("runtime_contract:", text)
        self.assertFalse((ROOT / "bin").exists())
        self.assertFalse((ROOT / "scripts").exists())

    def test_all_24_workflows_have_guidance(self):
        text = (ROOT / "references/commands.md").read_text()
        sections = re.split(r"^### (\w+)\n", text, flags=re.M)
        expected = set("craft shape init document extract critique audit polish bolder quieter distill harden onboard animate colorize typeset layout delight overdrive clarify adapt optimize live generate".split())
        self.assertEqual(set(sections[1::2]), expected)
        self.assertEqual(len(sections[1::2]), 24)
        for name, body in zip(sections[1::2], sections[2::2]):
            with self.subTest(workflow=name):
                self.assertGreater(len(body.strip()), 120)

    def test_local_reference_closure(self):
        for source in ROOT.rglob("*.md"):
            for target in re.findall(r"\[[^\]]*\]\(([^)]+)\)", source.read_text()):
                if "://" in target:
                    continue
                resolved = (source.parent / target.split("#")[0]).resolve()
                with self.subTest(source=source.name, target=target):
                    self.assertTrue(resolved.is_relative_to(ROOT.resolve()))
                    self.assertTrue(resolved.is_file())

    def test_provenance_license_and_exclusions(self):
        provenance = (ROOT / "PROVENANCE.md").read_text()
        self.assertIn("414603bf14c7a344edb466bb1f85690c77ce8b7d", provenance)
        self.assertIn("skill-v4.8.0", provenance)
        self.assertIn("Copyright 2025 Paul Bakaus", (ROOT / "LICENSE").read_text())
        self.assertIn("ehmo", (ROOT / "NOTICE.md").read_text())
        for marker in ("native detector", "no provider key", "not the complete upstream"):
            self.assertIn(marker, provenance)


if __name__ == "__main__":
    unittest.main()
