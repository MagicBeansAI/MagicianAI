from __future__ import annotations

import hashlib
import io
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))
import link_runtime
import scope_env

TEMPLATE = """---
name: demo-mcp
metadata:
  magician:
    runtime_contract:
      runtime:
        transport:
          kind: streamable_http
          endpoint: ${DEMO_BASE_URL}/api/mcp
        discovery:
          oauth:
            authorization_issuer: ${DEMO_BASE_URL}
---
Body.
"""


class ScopeEnvTests(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)
        self.hub = self.root / "skillshub"
        self.source = self.hub / "demo-mcp"
        (self.source / "config").mkdir(parents=True)
        (self.source / "SKILL.md").write_text(TEMPLATE)
        (self.source / "config/.env.example").write_text("DEMO_BASE_URL=\n")
        (self.source / "notes.md").write_text("notes")
        self.store = self.root / "data/system/skill-bin"

    def tearDown(self):
        self._tmp.cleanup()

    def scoped(self, scope: str, base_url: str | None) -> Path:
        skill = self.root / "data/scopes" / scope / "skills/demo-mcp"
        (skill / "config").mkdir(parents=True, exist_ok=True)
        if base_url is not None:
            (skill / "config/.env").write_text(f"DEMO_BASE_URL={base_url}\n")
        return skill

    def rendered(self, skill: Path, text: str) -> None:
        """What the retired renderer left: a real manifest and its marker."""
        (skill / "SKILL.md").write_text(text)
        (skill / scope_env.LEGACY_RENDER_MARKER).write_text(json.dumps({
            "schema_version": scope_env.LEGACY_RENDER_SCHEMA,
            "skill": "demo-mcp",
            "manifest_sha256": hashlib.sha256(text.encode()).hexdigest(),
        }))

    def link(self, scope: str) -> None:
        with mock.patch.object(link_runtime, "ROOT", self.hub), \
             mock.patch.object(link_runtime.pinned_bin_store, "store_skill_bins", return_value=(set(), 0)), \
             mock.patch("sys.stdout", io.StringIO()):
            link_runtime.link_scope(self.root / "data/scopes" / scope, None, self.store)

    def test_origin_rule(self):
        self.assertEqual(scope_env.https_origin("https://Metabase.Example.com/"), "https://metabase.example.com")
        self.assertEqual(scope_env.https_origin("https://mb.example:443"), "https://mb.example")
        self.assertEqual(scope_env.https_origin("https://mb.example:8443"), "https://mb.example:8443")
        for bad in ("http://mb.example", "https://mb.example/api", "https://u:p@mb.example",
                    "https://mb.example?x=1", "https://mb.example#f", "https://mb.example:x", "",
                    # Hosts the runtime's URL parser refuses or rewrites.
                    "https://1.2.3", "https://10.1.2", "https://1234", "https://0x7f.1",
                    "https://.example", "https://-mb.example", "https://a..b"):
            self.assertIsNone(scope_env.https_origin(bad), bad)
        self.assertEqual(scope_env.https_origin("https://10.0.0.5:8443"), "https://10.0.0.5:8443")

    def test_every_scope_links_the_shared_manifest(self):
        alice = self.scoped("alice/home", "https://alice.example")
        bob = self.scoped("bob/home", "https://bob.example")
        self.link("alice/home")
        self.link("bob/home")
        for skill in (alice, bob):
            self.assertTrue((skill / "SKILL.md").is_symlink())
            self.assertEqual((skill / "SKILL.md").resolve(), (self.source / "SKILL.md").resolve())
        self.assertEqual(scope_env.read_scope_env(alice), {"DEMO_BASE_URL": "https://alice.example"})
        self.assertEqual(scope_env.read_scope_env(bob), {"DEMO_BASE_URL": "https://bob.example"})

    def test_a_rendered_manifest_is_relinked_and_later_files_still_land(self):
        skill = self.scoped("alice/home", "https://mb.example.com")
        self.rendered(skill, TEMPLATE.replace("${DEMO_BASE_URL}", "https://mb.example.com"))
        self.link("alice/home")
        self.assertTrue((skill / "SKILL.md").is_symlink())
        self.assertFalse((skill / scope_env.LEGACY_RENDER_MARKER).exists())
        self.assertEqual(scope_env.read_scope_env(skill), {"DEMO_BASE_URL": "https://mb.example.com"})
        (self.source / "later.md").write_text("later")
        self.link("alice/home")
        self.assertTrue((skill / "later.md").is_symlink())

    def test_an_owned_or_edited_manifest_is_never_clobbered(self):
        owned = self.scoped("alice/home", None)
        (owned / "SKILL.md").write_text("---\nname: demo-mcp\n---\nmine\n")
        self.link("alice/home")
        self.assertFalse((owned / "SKILL.md").is_symlink())
        self.assertIn("mine", (owned / "SKILL.md").read_text())

        edited = self.scoped("bob/home", "https://mb.example.com")
        self.rendered(edited, TEMPLATE.replace("${DEMO_BASE_URL}", "https://mb.example.com"))
        (edited / "SKILL.md").write_text("hand edited")
        self.assertEqual(scope_env.retire_rendered_manifest(self.source, edited), "edited")
        self.link("bob/home")
        self.assertFalse((edited / "SKILL.md").is_symlink())
        self.assertEqual((edited / "SKILL.md").read_text(), "hand edited")
        self.assertTrue((edited / scope_env.LEGACY_RENDER_MARKER).is_file())

    def test_a_stale_marker_beside_a_link_is_removed(self):
        skill = self.scoped("alice/home", None)
        self.link("alice/home")
        (skill / scope_env.LEGACY_RENDER_MARKER).write_text("{}")
        self.assertEqual(scope_env.retire_rendered_manifest(self.source, skill), "marker_removed")
        self.assertFalse((skill / scope_env.LEGACY_RENDER_MARKER).exists())
        self.assertTrue((skill / "SKILL.md").is_symlink())


if __name__ == "__main__":
    unittest.main()
