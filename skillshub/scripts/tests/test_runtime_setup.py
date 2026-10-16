from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))
import generated_tools
import stage_release
import verify_runtime
import install_bot_bundles


class RuntimeSetupTests(unittest.TestCase):
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

    def test_generator_refuses_unreviewed_spec_changes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            scripts = root / "metabase/scripts"
            scripts.mkdir(parents=True)
            (scripts / "spec.json").write_text("changed spec")
            (root / "metabase/generator-pin.json").write_text('{"spec_sha256":"old"}')
            with self.assertRaisesRegex(ValueError, "spec differs"):
                generated_tools.inputs(root)


if __name__ == "__main__":
    unittest.main()
