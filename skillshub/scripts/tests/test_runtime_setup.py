from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))
import stage_release
import verify_runtime
import install_bot_bundles


class RuntimeSetupTests(unittest.TestCase):
    def test_packaged_installer_ships_and_runs_shared_cli_wiring(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source/skillshub"
            staged = root / "relocated release/skillshub"
            (source / "scripts").mkdir(parents=True)
            tracked = ["Makefile", "package-lock.json", "whatsapp-mcp/SKILL.md"]
            (source / "Makefile").write_text("AGENT_BROWSER_VERSION ?= 1.2.3\n")
            (source / "package-lock.json").write_text(json.dumps({
                "packages": {"node_modules/wu": {"bin": {"wu": "index.js"}}}}))
            (source / "whatsapp-mcp").mkdir()
            (source / "whatsapp-mcp/SKILL.md").write_text(
                "---\nmetadata:\n  magician:\n    requires:\n      bins: [wu]\n---\n")
            for script in ("verify_runtime.py", "link_runtime.py", "migrate_metabase_mcp.py",
                           "pinned_bin_store.py", "stage_release.py", "shared_skill_bins.py"):
                shutil.copy2(SCRIPTS / script, source / "scripts" / script)
            native = b"\xcf\xfa\xed\xfe\x0c\x00\x00\x01" + b"\0" * 56
            for relative in ("document-to-markdown/bin/document-to-markdown", "browser/bin/agent-browser",
                             "browser/_vendor/v1.2.3/bin/agent-browser-darwin-arm64"):
                target = source / relative
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(native)
                target.chmod(0o755)
            for bot in ("sdk", *verify_runtime.BOTS):
                target = source / "bots" / bot / "dist/index.js"
                target.parent.mkdir(parents=True)
                target.write_text("fixture bundle\n")

            def frozen_or_tracked(command, **kwargs):
                if command[0] == "git":
                    return "\0".join("skillshub/" + path for path in tracked) + "\0"
                self.assertEqual(command[0], "uv")
                return "PyYAML==6.0.3\n"

            with patch.object(stage_release.subprocess, "check_output", side_effect=frozen_or_tracked):
                stage_release.stage(source, staged, "aarch64-apple-darwin",
                                    source / "document-to-markdown/bin/document-to-markdown")
            for relative in (".node/bin/node", "node_modules/.bin/wu"):
                target = staged / relative
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_text("#!/bin/sh\nexit 0\n")
                target.chmod(0o755)
            result = subprocess.run([sys.executable, str(staged / "scripts/shared_skill_bins.py")],
                                    text=True, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            for name, target in (("wu", "node_modules/.bin/wu"), ("node", ".node/bin/node")):
                self.assertTrue((staged / "whatsapp-mcp/bin" / name).samefile(staged / target))

    def test_native_payload_missing_even_when_gws_launcher_exists(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            launcher = root / "node_modules/.bin/gws"
            launcher.parent.mkdir(parents=True)
            launcher.write_text("#!/bin/sh\nexit 0\n")
            launcher.chmod(0o755)
            audit = verify_runtime.Audit(root)
            audit.binary(root / "gmail", "gws")
            self.assertTrue(any(".bin_real/gws" in error for error in audit.errors))

    def test_install_missing_bot_fails_instead_of_skipping_it(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "bots/telegram-self"
            source.mkdir(parents=True)
            (source / "package.json").write_text("{}")
            written, failures = install_bot_bundles.install_into_scope(root / "scope", source.parent)
            self.assertEqual((written, failures), (0, 1))

    def test_install_preserves_legacy_dependencies_and_credentials_by_default(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "bots/telegram/dist"
            source.mkdir(parents=True)
            (source / "index.js").write_text("console.log('fixture')")
            scope = root / "scope"
            bot = scope / "bots/telegram"
            (bot / "node_modules").mkdir(parents=True)
            (bot / "node_modules/state").write_text("keep")
            (bot / "package.json").write_text("keep")
            (bot / ".env").write_text("keep")
            self.assertEqual(install_bot_bundles.install_into_scope(scope, root / "bots"), (1, 0))
            self.assertEqual((bot / "node_modules/state").read_text(), "keep")
            self.assertEqual((bot / ".env").read_text(), "keep")
            self.assertEqual((bot / "package.json").read_text(), "keep")

    def test_explicit_legacy_bot_cleanup_preserves_modern_skill_bins(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "bots/whatsapp/dist"
            source.mkdir(parents=True)
            (source / "index.js").write_text("fixture bundle")
            scope = root / "scope"
            adapter = scope / "skills/whatsapp/bin/whatsapp-poll-adapter"
            adapter.parent.mkdir(parents=True)
            adapter.write_text("modern skill executable")
            self.assertEqual(install_bot_bundles.install_into_scope(scope, root / "bots", prune_legacy=True), (1, 0))
            self.assertEqual(adapter.read_text(), "modern skill executable")

    def test_release_rejects_wrong_target_native_artifact(self):
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / "tool"
            binary.write_bytes(b"\xcf\xfa\xed\xfe\x0c\x00\x00\x01" + b"\0" * 56)
            self.assertEqual(stage_release.native_target(binary), "aarch64-apple-darwin")

    def test_release_rejects_tampered_or_empty_artifact_manifest(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "tool").write_text("tampered")
            manifest = {"schema_version": 1, "files": [{"path": "tool", "sha256": "bad"}]}
            (root / "release-tools.json").write_text(json.dumps(manifest))
            with self.assertRaisesRegex(ValueError, "checksum mismatch"):
                stage_release.verify(root)
            manifest["files"] = []
            (root / "release-tools.json").write_text(json.dumps(manifest))
            with self.assertRaisesRegex(ValueError, "empty"):
                stage_release.verify(root)



if __name__ == "__main__":
    unittest.main()
