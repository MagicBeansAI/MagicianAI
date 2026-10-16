from __future__ import annotations

import json
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "setup-desktop-pnpm.sh"


class SetupDesktopPnpmTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary_directory = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary_directory.name)
        self.package_json = self.root / "package.json"
        self.package_json.write_text(
            json.dumps({"packageManager": "pnpm@10.33.0"}), encoding="utf-8"
        )
        self.tool_root = self.root / "tool"

    def tearDown(self) -> None:
        self.temporary_directory.cleanup()

    @staticmethod
    def write_executable(path: Path, source: str) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(source, encoding="utf-8")
        path.chmod(0o755)

    def run_script(
        self, *arguments: str, path: str | None = None
    ) -> subprocess.CompletedProcess[str]:
        environment = os.environ.copy()
        environment["DESKTOP_PACKAGE_JSON"] = str(self.package_json)
        environment["DESKTOP_PNPM_TOOL_ROOT"] = str(self.tool_root)
        if path is not None:
            environment["PATH"] = path
        return subprocess.run(
            ["bash", str(SCRIPT), *arguments],
            check=False,
            capture_output=True,
            text=True,
            env=environment,
        )

    def test_matching_checkout_local_binary_is_reused(self) -> None:
        pnpm = self.tool_root / "node_modules" / ".bin" / "pnpm"
        self.write_executable(pnpm, "#!/bin/sh\necho 10.33.0\n")

        result = self.run_script()

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("desktop pnpm 10.33.0", result.stdout)
        self.assertNotIn("Installing", result.stdout)

    def test_explicit_command_must_match_manifest_pin(self) -> None:
        pnpm = self.root / "wrong-pnpm"
        self.write_executable(pnpm, "#!/bin/sh\necho 11.24.0\n")

        result = self.run_script("--verify-command", str(pnpm))

        self.assertEqual(result.returncode, 1)
        self.assertIn("pnpm 10.33.0 is required", result.stderr)
        self.assertIn("11.24.0", result.stderr)

    def test_missing_binary_is_provisioned_with_npm(self) -> None:
        fake_bin = self.root / "fake-bin"
        fake_bin.mkdir()
        node = shutil.which("node")
        self.assertIsNotNone(node)
        self.write_executable(
            fake_bin / "npm",
            """#!/bin/sh
set -eu
prefix=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--prefix" ]; then prefix="$2"; shift 2; else shift; fi
done
test -n "$prefix"
mkdir -p "$prefix/node_modules/.bin"
printf '#!/bin/sh\\necho 10.33.0\\n' > "$prefix/node_modules/.bin/pnpm"
chmod +x "$prefix/node_modules/.bin/pnpm"
""",
        )

        result = self.run_script(path=f"{fake_bin}:{os.environ['PATH']}")

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Installing checkout-local pnpm 10.33.0", result.stdout)
        self.assertIn("desktop pnpm 10.33.0", result.stdout)


if __name__ == "__main__":
    unittest.main()
