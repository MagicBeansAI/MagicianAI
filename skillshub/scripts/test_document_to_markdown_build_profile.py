from __future__ import annotations

import subprocess
import unittest
from pathlib import Path


SKILLSHUB_ROOT = Path(__file__).resolve().parent.parent


class DocumentToMarkdownBuildProfileTests(unittest.TestCase):
    def make_plan(self, profile: str) -> str:
        result = subprocess.run(
            [
                "make",
                "--dry-run",
                "--no-print-directory",
                "setup-document-to-markdown",
                f"PROFILE={profile}",
                "CARGO_TARGET_DIR=/tmp/magician-profile-proof",
            ],
            cwd=SKILLSHUB_ROOT,
            check=True,
            capture_output=True,
            text=True,
        )
        return result.stdout

    def test_debug_profile_builds_and_links_debug_binary(self) -> None:
        plan = self.make_plan("debug")
        self.assertIn("-p document-to-markdown-cli", plan)
        self.assertNotIn("--release", plan)
        self.assertIn(
            "/tmp/magician-profile-proof/debug/document-to-markdown", plan
        )

    def test_release_profile_builds_and_links_release_binary(self) -> None:
        plan = self.make_plan("release")
        self.assertIn("-p document-to-markdown-cli --release", plan)
        self.assertIn(
            "/tmp/magician-profile-proof/release/document-to-markdown", plan
        )

    def test_unknown_profile_fails_before_any_build_command(self) -> None:
        result = subprocess.run(
            [
                "make",
                "--dry-run",
                "--no-print-directory",
                "setup-document-to-markdown",
                "PROFILE=optimized",
            ],
            cwd=SKILLSHUB_ROOT,
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("PROFILE must be debug or release", result.stderr)


if __name__ == "__main__":
    unittest.main()
