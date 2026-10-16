"""Scope skills layers must survive a cleaned, moved or copied install.

The 2026-09-30 incident: every scope's `skills/browser/bin/agent-browser` was
an absolute link to the checkout's ignored, generated `browser/bin/`, which
was emptied, so the runtime's pinned-CLI lookup failed in every workspace.
"""
from __future__ import annotations

import importlib.util
import os
from pathlib import Path
import shutil
import sys
import tempfile
import unittest

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))
import link_runtime  # noqa: E402
import pinned_bin_store  # noqa: E402
import verify_runtime  # noqa: E402

_spec = importlib.util.spec_from_file_location("install_skill_layer_under_test", SCRIPTS / "install_skill_layer.py")
install_skill_layer = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(install_skill_layer)

MACHO = b"\xcf\xfa\xed\xfe" + b"\0" * 60


def write_exe(path: Path, content: bytes) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(content)
    path.chmod(0o755)
    return path


class Fixture:
    """A skillshub with one browser skill, and a data root with one scope."""

    def __init__(self, base: Path):
        self.hub = base / "checkout" / "skillshub"
        self.skill = self.hub / "browser"
        (self.skill / "SKILL.md").parent.mkdir(parents=True)
        (self.skill / "SKILL.md").write_text("---\nname: browser\n---\n")
        self.native = write_exe(self.skill / "bin" / "agent-browser", MACHO + b"v1")
        self.script = write_exe(self.skill / "bin" / "helper", b"#!/bin/sh\nexit 0\n")
        # npm owns this tree; even a native file there must stay a link.
        write_exe(self.hub / "node_modules" / "rg" / "rg", MACHO + b"rg")
        (self.skill / "bin" / "rg").symlink_to(self.hub / "node_modules" / "rg" / "rg")
        self.data = base / "data"
        self.scope = self.data / "scopes" / "anonymous" / "default"
        self.skills = self.scope / "skills"
        self.store = self.data / "system" / "skill-bin"


class StoreRuleTests(unittest.TestCase):
    def test_store_root_only_for_scope_skills_trees(self):
        self.assertEqual(pinned_bin_store.store_root_for(Path("/d/scopes/p/w/skills")),
                         Path("/d/system/skill-bin"))
        self.assertIsNone(pinned_bin_store.store_root_for(Path("/container/skills")))
        self.assertIsNone(pinned_bin_store.store_root_for(Path("/d/other/p/w/skills")))

    def test_only_owned_native_executables_are_stored(self):
        with tempfile.TemporaryDirectory() as directory:
            f = Fixture(Path(directory))
            entries = pinned_bin_store.native_bin_entries(f.hub, f.skill)
            self.assertEqual(set(entries), {"agent-browser"})


class InstallTests(unittest.TestCase):
    def test_scope_keeps_its_cli_when_the_checkout_bin_is_cleaned(self):
        with tempfile.TemporaryDirectory() as directory:
            f = Fixture(Path(directory))
            install_skill_layer.install_skill(f.skill, f.skills / "browser",
                                              pinned_bin_store.store_root_for(f.skills))
            scoped = f.skills / "browser" / "bin" / "agent-browser"
            self.assertFalse(os.path.isabs(os.readlink(scoped)), "store link must be relative")
            self.assertTrue(os.path.isabs(os.readlink(f.skills / "browser" / "bin" / "helper")))
            f.native.unlink()  # what emptied skillshub/browser/bin on 09-30
            self.assertEqual(scoped.read_bytes(), MACHO + b"v1")

    def test_copied_data_root_keeps_resolving(self):
        with tempfile.TemporaryDirectory() as directory:
            f = Fixture(Path(directory))
            install_skill_layer.install_skill(f.skill, f.skills / "browser",
                                              pinned_bin_store.store_root_for(f.skills))
            copy = Path(directory) / "elsewhere" / "data"
            shutil.copytree(f.data, copy, symlinks=True)
            shutil.rmtree(f.data)
            moved = copy / "scopes/anonymous/default/skills/browser/bin/agent-browser"
            self.assertEqual(moved.read_bytes(), MACHO + b"v1")


class LinkRuntimeTests(unittest.TestCase):
    def setUp(self):
        self._root = link_runtime.ROOT

    def tearDown(self):
        link_runtime.ROOT = self._root

    def test_repair_relinks_a_moved_checkout_and_stores_the_native(self):
        with tempfile.TemporaryDirectory() as directory:
            f = Fixture(Path(directory))
            link_runtime.ROOT = f.hub
            stale = f.skills / "browser" / "SKILL.md"
            stale.parent.mkdir(parents=True)
            stale.symlink_to("/old/checkout/skillshub/browser/SKILL.md")
            changed, skipped = link_runtime.link_scope(f.scope, None, f.store)
            self.assertGreater(changed, 0)
            self.assertEqual(skipped, 0)
            self.assertEqual(stale.resolve(), (f.skill / "SKILL.md").resolve())
            scoped = f.skills / "browser" / "bin" / "agent-browser"
            self.assertTrue(str(scoped.resolve()).startswith(str(f.store.resolve())))
            # Idempotent: a second pass changes nothing.
            self.assertEqual(link_runtime.link_scope(f.scope, None, f.store), (0, 0))

    def test_scope_owned_skill_copy_is_left_alone(self):
        with tempfile.TemporaryDirectory() as directory:
            f = Fixture(Path(directory))
            link_runtime.ROOT = f.hub
            own = f.skills / "browser"
            own.mkdir(parents=True)
            (own / "SKILL.md").write_text("mine")
            self.assertEqual(link_runtime.link_scope(f.scope, None, f.store), (0, 1))
            self.assertFalse((own / "bin").exists())

    def test_all_scopes_covers_later_workspaces_but_not_sinks(self):
        with tempfile.TemporaryDirectory() as directory:
            data = Path(directory)
            for scope in ("anonymous/default", "alice/project", "system/system"):
                (data / "scopes" / scope).mkdir(parents=True)
            names = [p.relative_to(data / "scopes").as_posix()
                     for p in link_runtime.scope_dirs(data, "anonymous/default", True, False)]
            self.assertEqual(names, ["anonymous/default", "alice/project"])
            self.assertEqual(link_runtime.scope_dirs(data, "bob/new", False, True), [])

    def test_prune_keeps_what_a_scope_links(self):
        with tempfile.TemporaryDirectory() as directory:
            f = Fixture(Path(directory))
            link_runtime.ROOT = f.hub
            link_runtime.link_scope(f.scope, None, f.store)
            orphan = f.store / ("0" * 64)
            orphan.mkdir(parents=True)
            removed = pinned_bin_store.prune_store(f.data / "scopes", f.store)
            self.assertEqual(removed, [orphan])
            self.assertEqual((f.skills / "browser/bin/agent-browser").read_bytes(), MACHO + b"v1")


class VerifyScopeTests(unittest.TestCase):
    def test_dangling_scope_cli_names_the_target_that_rebuilds_it(self):
        with tempfile.TemporaryDirectory() as directory:
            f = Fixture(Path(directory))
            scoped = f.skills / "browser" / "bin" / "agent-browser"
            scoped.parent.mkdir(parents=True)
            scoped.symlink_to(f.native)
            (f.skills / "browser" / "SKILL.md").symlink_to(f.skill / "SKILL.md")
            f.native.unlink()
            audit = verify_runtime.Audit(f.hub)
            audit.scope(f.scope, only={"browser"})
            joined = "\n".join(audit.errors)
            self.assertIn("browser/bin/agent-browser points at", joined)
            self.assertIn("make setup-agent-browser", joined)

    def test_healthy_scope_cli_enters_the_inventory_by_absolute_path(self):
        with tempfile.TemporaryDirectory() as directory:
            f = Fixture(Path(directory))
            link_runtime_root = link_runtime.ROOT
            try:
                link_runtime.ROOT = f.hub
                link_runtime.link_scope(f.scope, None, f.store)
            finally:
                link_runtime.ROOT = link_runtime_root
            audit = verify_runtime.Audit(f.hub)
            audit.scope(f.scope, only={"browser"})
            self.assertEqual(audit.errors, [])
            self.assertIn(os.path.abspath(f.skills / "browser/bin/agent-browser"), audit.files)

    def test_pack_data_in_bin_is_not_held_to_executable(self):
        with tempfile.TemporaryDirectory() as directory:
            f = Fixture(Path(directory))
            (f.skill / "bin" / "generated.json").write_text("{}\n")
            link_runtime_root = link_runtime.ROOT
            try:
                link_runtime.ROOT = f.hub
                link_runtime.link_scope(f.scope, None, f.store)
            finally:
                link_runtime.ROOT = link_runtime_root
            audit = verify_runtime.Audit(f.hub)
            audit.scope(f.scope, only={"browser"})
            self.assertEqual(audit.errors, [])
            self.assertIn(os.path.abspath(f.skills / "browser/bin/generated.json"), audit.files)


if __name__ == "__main__":
    unittest.main()
