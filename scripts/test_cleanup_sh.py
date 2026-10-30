"""Dry-run checks for scripts/cleanup.sh's native-backend handling.

Every run uses --dry-run and a temporary HOME, so nothing real is touched.
"""
from __future__ import annotations

import json
import os
import platform
import subprocess
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
SCRIPT = REPO / "scripts/cleanup.sh"
SERVICE_ID = "ai.magicbeans.magican.backend"


@unittest.skipUnless(platform.system() == "Darwin", "the native LaunchAgent flow is macOS-only")
class NativeCleanupDryRunTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.home = Path(os.path.realpath(self.temporary.name))
        self.support = self.home / "Library/Application Support/dev.magician.desktop"
        self.support.mkdir(parents=True)
        self.prefix = self.home / ".magician"
        (self.prefix / "share/seed").mkdir(parents=True)
        (self.prefix / "MANIFEST.yaml").write_text("version: test\n")
        (self.prefix / "share/seed/magician-config.yaml").write_text("enabled: true\n")
        self.data = self.home / "MagicianNotes"
        self.data.mkdir()
        (self.support / "desktop-app.quit").write_text("quit\n")
        agents = self.home / "Library/LaunchAgents"
        agents.mkdir(parents=True)
        (agents / f"{SERVICE_ID}.plist").write_text(
            '<?xml version="1.0" encoding="UTF-8"?>\n<plist version="1.0"><dict>'
            "<key>EnvironmentVariables</key><dict>"
            f"<key>MAGICIAN_ROOT_DIR</key><string>{self.data}</string></dict></dict></plist>\n"
        )

    def manifest(self, *, service_pre_existing: bool = False) -> None:
        (self.support / "install-manifest.json").write_text(json.dumps({
            "runtime": "Native services",
            "data_dirs": [str(self.data)],
            "installed_by_us": {"native_prefix": str(self.prefix)},
            "pre_existing": {"native_service": service_pre_existing, "native_backend": False},
        }))

    def run_dry(self, *args: str) -> list[str]:
        env = {"HOME": str(self.home), "PATH": os.environ["PATH"], "MAGICIAN_PREFIX": str(self.prefix)}
        result = subprocess.run(["bash", str(SCRIPT), "--dry-run", *args], env=env,
                                capture_output=True, text=True, check=True)
        return result.stdout.splitlines()

    @staticmethod
    def index(lines: list[str], needle: str) -> int:
        return next(i for i, line in enumerate(lines) if needle in line)

    def test_full_cleanup_unregisters_the_service_before_any_data_is_removed(self):
        self.manifest()
        lines = self.run_dry()
        bootout = self.index(lines, f"launchctl bootout gui/{os.getuid()}/{SERVICE_ID}")
        plist = self.index(lines, f"rm -f {self.home}/Library/LaunchAgents/{SERVICE_ID}.plist")
        enable = self.index(lines, f"launchctl enable gui/{os.getuid()}/{SERVICE_ID}")
        prefix = self.index(lines, f"rm -rf {self.prefix}")
        data = self.index(lines, f"rm -rf {self.data}")
        self.assertLess(bootout, plist)
        self.assertLess(plist, enable, "the disable override is cleared after removal")
        self.assertLess(enable, prefix)
        self.assertLess(prefix, data, "service and prefix go before the data")
        self.index(lines, f"rm -f {self.support}/desktop-app.quit")
        self.assertFalse(any("docker" in line for line in lines), "no container steps for a native install")
        self.assertTrue(self.data.exists() and self.prefix.exists(), "dry run changes nothing")

    def test_only_data_stops_the_backend_then_restarts_it(self):
        self.manifest()
        lines = self.run_dry("--only-data")
        stop = self.index(lines, f"launchctl bootout gui/{os.getuid()}/{SERVICE_ID}")
        data = self.index(lines, f"rm -rf {self.data}")
        start = self.index(lines, f"launchctl kickstart gui/{os.getuid()}/{SERVICE_ID}")
        self.assertLess(stop, data)
        self.assertLess(data, start)
        self.index(lines, f"Re-seeding {self.data}")
        self.assertFalse(any(f"rm -rf {self.prefix}" in line for line in lines), "tools are kept")

    def test_a_service_that_predates_desktop_is_never_touched(self):
        self.manifest(service_pre_existing=True)
        lines = self.run_dry()
        self.assertFalse(any("launchctl" in line for line in lines))
        self.index(lines, "existed before Desktop setup; left in place")

    def test_a_refused_delete_is_never_followed_by_seeding(self):
        outside = Path(tempfile.mkdtemp(dir="/private/tmp" if Path("/private/tmp").is_dir() else None))
        self.addCleanup(lambda: outside.rmdir() if outside.exists() else None)
        (self.support / "install-manifest.json").write_text(json.dumps({
            "runtime": "Native services",
            "data_dirs": [str(outside)],
            "installed_by_us": {"native_prefix": str(self.prefix)},
            "pre_existing": {"native_service": False, "native_backend": False},
        }))
        plist = self.home / f"Library/LaunchAgents/{SERVICE_ID}.plist"
        plist.write_text(plist.read_text().replace(str(self.data), str(outside)))
        lines = self.run_dry("--only-data")
        self.index(lines, "REFUSED to delete")
        self.assertFalse(any("Re-seeding" in line for line in lines), "the intact root is not seeded")
        self.index(lines, f"launchctl kickstart gui/{os.getuid()}/{SERVICE_ID}")

    def test_reseeding_never_creates_an_env_development(self):
        (self.prefix / "share/seed/.env.example").write_text("KEY=\n")
        self.manifest()
        lines = self.run_dry("--only-data")
        self.assertFalse(any(".env.development" in line for line in lines))

    def test_a_prefix_with_another_identity_is_refused(self):
        self.manifest()
        (self.prefix / "MANIFEST.yaml").unlink()
        lines = self.run_dry()
        self.index(lines, "REFUSED to remove native backend files")
        self.assertFalse(any(f"rm -rf {self.prefix}" in line for line in lines))


if __name__ == "__main__":
    unittest.main()
