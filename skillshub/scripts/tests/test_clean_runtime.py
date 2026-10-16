from __future__ import annotations

import io
from pathlib import Path
import sys
import tempfile
import unittest

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))
import clean_runtime


def _write(path: Path, text: str = "x") -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)
    return path


class CleanRuntimeTests(unittest.TestCase):
    def _scope(self, root: Path) -> Path:
        scope = root / "scopes/anonymous/default"
        bots = scope / "bots"
        _write(bots / "bot_configs.yaml", "bots: {}\n")
        _write(bots / "kapso/.env.development", "KAPSO_API_KEY=secret\n")
        _write(bots / "gmail/.env.business", "GWS_EXPECTED_EMAIL=a@b.c\n")
        _write(bots / "gmail/workdirs/state.json", "{}")
        _write(bots / "kapso/dist/index.js", "// bundle")
        _write(bots / "kapso/package.json", "{}")
        _write(bots / "kapso/node_modules/dep/index.js", "")
        _write(bots / "stray.txt")
        _write(scope / "skills/zepto-mcp/SKILL.md")
        _write(scope / "auth/gws-business/token.json", "{}")
        _write(root / "system/skills/legacy/SKILL.md")
        return scope

    def test_bot_secrets_and_state_survive_while_installs_go(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            scope = self._scope(root)
            clean_runtime.clean(root, dry=False, out=io.StringIO())
            bots = scope / "bots"
            for kept in [
                bots / "bot_configs.yaml",
                bots / "kapso/.env.development",
                bots / "gmail/.env.business",
                bots / "gmail/workdirs/state.json",
                scope / "auth/gws-business/token.json",
            ]:
                self.assertTrue(kept.exists(), kept)
            self.assertEqual(
                (bots / "kapso/.env.development").read_text(), "KAPSO_API_KEY=secret\n"
            )
            for gone in [
                bots / "kapso/dist",
                bots / "kapso/package.json",
                bots / "kapso/node_modules",
                bots / "stray.txt",
                scope / "skills",
                root / "system/skills",
            ]:
                self.assertFalse(gone.exists(), gone)

    def test_dry_run_deletes_nothing(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            scope = self._scope(root)
            before = sorted(str(p) for p in root.rglob("*"))
            out = io.StringIO()
            clean_runtime.clean(root, dry=True, out=out)
            self.assertEqual(before, sorted(str(p) for p in root.rglob("*")))
            self.assertIn("would remove 4 item(s) under", out.getvalue())
            self.assertTrue((scope / "skills").exists())

    def test_symlinked_entries_are_unlinked_not_followed(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = _write(root / "skillshub/bots/kapso/dist/index.js", "// source")
            shim = root / "scopes/anonymous/default/bots/kapso/dist"
            shim.parent.mkdir(parents=True)
            shim.symlink_to(source.parent, target_is_directory=True)
            clean_runtime.clean(root, dry=False, out=io.StringIO())
            self.assertFalse(shim.exists() or shim.is_symlink())
            self.assertTrue(source.exists(), "a shim's target must never be deleted")


if __name__ == "__main__":
    unittest.main()
