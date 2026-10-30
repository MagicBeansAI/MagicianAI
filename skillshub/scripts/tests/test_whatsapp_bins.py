"""Exercise the real setup target without WhatsApp accounts or host PATH CLIs."""
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import verify_runtime


MAKEFILE = Path(__file__).resolve().parents[2] / "Makefile"


def executable(path):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("#!/bin/sh\nexit 0\n")
    path.chmod(0o755)


class WhatsappBinSetupTests(unittest.TestCase):
    def test_setup_fixes_both_transports_and_repeated_setup_is_safe(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            executable(root / ".node/bin/node")
            executable(root / "node_modules/.bin/wu")
            before = verify_runtime.Audit(root)
            before.binary(root / "whatsapp-mcp", "wu")
            self.assertTrue(before.errors, "shared npm installation must not hide missing activation link")
            for _ in range(2):
                result = subprocess.run(["make", "-f", str(MAKEFILE), "setup-whatsapp-bins"],
                                        cwd=root, capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            for skill in ("whatsapp", "whatsapp-mcp"):
                audit = verify_runtime.Audit(root)
                for name, source in (("wu", "node_modules/.bin/wu"), ("node", ".node/bin/node")):
                    self.assertTrue((root / skill / "bin" / name).samefile(root / source))
                    audit.binary(root / skill, name)
                self.assertEqual(audit.errors, [])

    def test_missing_dependency_fails_before_creating_partial_links(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            executable(root / "node_modules/.bin/wu")
            result = subprocess.run(["make", "-f", str(MAKEFILE), "setup-whatsapp-bins"],
                                    cwd=root, capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("Pinned wu or Node missing", result.stdout)
            self.assertFalse((root / "whatsapp/bin/wu").exists())
            self.assertFalse((root / "whatsapp-mcp/bin/wu").exists())


if __name__ == "__main__":
    unittest.main()
