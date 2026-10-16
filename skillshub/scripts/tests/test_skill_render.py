from __future__ import annotations

import io
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))
import link_runtime
import skill_render

TEMPLATE = """---
name: demo-mcp
metadata:
  magician:
    runtime_contract:
      runtime:
        transport: {kind: streamable_http, endpoint: https://demo.invalid/api/mcp}
        discovery: {oauth: {authorization_issuer: https://demo.invalid}}
---
Body.
"""
SPEC = """schema_version: magician.skill-render.v1
substitutions:
  - placeholder: https://demo.invalid
    input: DEMO_BASE_URL
    kind: https_origin
"""


class SkillRenderTests(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)
        self.hub = self.root / "skillshub"
        self.source = self.hub / "demo-mcp"
        (self.source / "config").mkdir(parents=True)
        (self.source / "SKILL.md").write_text(TEMPLATE)
        (self.source / "render.yaml").write_text(SPEC)
        (self.source / "config/.env.example").write_text("DEMO_BASE_URL=\n")
        (self.source / "notes.md").write_text("notes")

    def tearDown(self):
        self._tmp.cleanup()

    def scoped(self, scope: str, base_url: str | None) -> Path:
        skill = self.root / "data/scopes" / scope / "skills/demo-mcp"
        (skill / "config").mkdir(parents=True, exist_ok=True)
        if base_url is not None:
            (skill / "config/.env").write_text(f"DEMO_BASE_URL={base_url}\n")
        return skill

    def test_renders_the_scope_value_into_a_real_manifest_with_provenance(self):
        skill = self.scoped("alice/home", "https://Metabase.Example.com/")
        status, _ = skill_render.render_scope_skill(self.source, skill)
        self.assertEqual(status, "rendered")
        manifest = skill / "SKILL.md"
        self.assertFalse(manifest.is_symlink())
        text = manifest.read_text()
        self.assertIn("https://metabase.example.com/api/mcp", text)
        self.assertIn("authorization_issuer: https://metabase.example.com}", text)
        self.assertNotIn(".invalid", text)
        marker = json.loads((skill / skill_render.MARKER).read_text())
        self.assertEqual(marker["skill"], "demo-mcp")
        self.assertTrue(skill_render.is_render_managed(skill))

    def test_unchanged_inputs_do_not_rewrite_and_changes_do(self):
        skill = self.scoped("alice/home", "https://mb.example.com")
        skill_render.render_scope_skill(self.source, skill)
        before = os.stat(skill / "SKILL.md").st_mtime_ns
        self.assertEqual(skill_render.render_scope_skill(self.source, skill)[0], "unchanged")
        self.assertEqual(os.stat(skill / "SKILL.md").st_mtime_ns, before)
        (skill / "config/.env").write_text("DEMO_BASE_URL=https://other.example.com\n")
        self.assertEqual(skill_render.render_scope_skill(self.source, skill)[0], "rendered")
        self.assertIn("other.example.com", (skill / "SKILL.md").read_text())
        (self.source / "SKILL.md").write_text(TEMPLATE.replace("Body.", "New body."))
        self.assertEqual(skill_render.render_scope_skill(self.source, skill)[0], "rendered")
        self.assertIn("New body.", (skill / "SKILL.md").read_text())
        # A hand edit to the rendered copy is corrected too.
        (skill / "SKILL.md").write_text("tampered")
        self.assertEqual(skill_render.render_scope_skill(self.source, skill)[0], "rendered")

    def test_missing_or_unsafe_input_leaves_no_manifest(self):
        skill = self.scoped("alice/home", "https://mb.example.com")
        skill_render.render_scope_skill(self.source, skill)
        for bad in ["", "http://mb.example.com", "https://mb.example.com/path",
                    "https://user:pw@mb.example.com", "https://mb.example.com?x=1",
                    "https://mb.example.com endpoint: evil", "https://mb.example.com}"]:
            (skill / "config/.env").write_text(f"DEMO_BASE_URL={bad}\n")
            status, detail = skill_render.render_scope_skill(self.source, skill)
            self.assertEqual(status, "missing_inputs", bad)
            self.assertIn("DEMO_BASE_URL", detail)
            self.assertFalse((skill / "SKILL.md").exists(), bad)
            self.assertFalse((skill / skill_render.MARKER).exists(), bad)

    def test_a_scope_owned_manifest_is_never_touched(self):
        skill = self.scoped("alice/home", "https://mb.example.com")
        (skill / "SKILL.md").write_text("mine")
        self.assertEqual(skill_render.render_scope_skill(self.source, skill)[0], "scope_owned")
        self.assertEqual((skill / "SKILL.md").read_text(), "mine")

    def test_each_scope_gets_only_its_own_value(self):
        alice = self.scoped("alice/home", "https://alice.example.com")
        bob = self.scoped("bob/home", "https://bob.example.com")
        nobody = self.scoped("carol/home", None)
        for scope in ("alice/home", "bob/home", "carol/home"):
            skill_render.render_scope(self.hub, self.root / "data/scopes" / scope, out=io.StringIO())
        self.assertIn("alice.example.com", (alice / "SKILL.md").read_text())
        self.assertNotIn("bob", (alice / "SKILL.md").read_text())
        self.assertIn("bob.example.com", (bob / "SKILL.md").read_text())
        self.assertFalse((nobody / "SKILL.md").exists())

    def test_link_runtime_never_links_the_template_and_renders_instead(self):
        skill = self.scoped("alice/home", "https://mb.example.com")
        store = self.root / "data/system/skill-bin"
        with mock.patch.object(link_runtime, "ROOT", self.hub), \
             mock.patch.object(link_runtime.pinned_bin_store, "store_skill_bins", return_value=(set(), 0)), \
             mock.patch("sys.stdout", io.StringIO()):
            link_runtime.link_scope(self.root / "data/scopes/alice/home", None, store)
            self.assertFalse((skill / "render.yaml").exists())
            self.assertTrue((skill / "notes.md").is_symlink())
            self.assertFalse((skill / "SKILL.md").is_symlink())
            self.assertIn("mb.example.com", (skill / "SKILL.md").read_text())
            # A rendered skill is setup-managed, so later links still land.
            (self.source / "later.md").write_text("later")
            link_runtime.link_scope(self.root / "data/scopes/alice/home", None, store)
            self.assertTrue((skill / "later.md").is_symlink())


if __name__ == "__main__":
    unittest.main()
