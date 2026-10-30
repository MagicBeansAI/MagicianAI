"""Check manifest helper flags against the installed pinned CLI, without OAuth."""
from __future__ import annotations

import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest

import yaml


ROOT = Path(__file__).resolve().parents[2]
GWS = ROOT / "node_modules/.bin/gws"


@unittest.skipUnless(GWS.is_file(), "pinned gws installation not present")
class GwsHelperContractTests(unittest.TestCase):
    def test_every_declared_helper_flag_is_supported_by_the_installed_cli(self):
        checked = {}
        with tempfile.TemporaryDirectory() as directory:
            env = {"HOME": directory, "GOOGLE_WORKSPACE_CLI_CONFIG_DIR": directory,
                   "CLOUDSDK_CONFIG": directory, "GWS_CONFIG_DIR": directory,
                   "PATH": str(ROOT / ".node/bin") + os.pathsep + os.defpath}
            for name in ("gmail", "presto-gmail", "sheets", "presto-sheets", "calendar", "presto-calendar"):
                document = (ROOT / name / "SKILL.md").read_text().split("---", 2)[1]
                actions = yaml.safe_load(document)["metadata"]["magician"]["runtime_actions"]["actions"]
                for action, spec in actions.items():
                    argv = spec.get("fixed_args", [])
                    if not any(part.startswith("+") for part in argv):
                        continue
                    key = tuple(argv)
                    if key not in checked:
                        result = subprocess.run([str(GWS), *argv, "--help"], env=env,
                                                capture_output=True, text=True, timeout=20)
                        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                        checked[key] = set(re.findall(r"(?m)^\s+(--[\w-]+)(?=\s|$)", result.stdout))
                    for mapping in spec.get("mappings", []):
                        if mapping.get("type") in ("flag", "bool_flag"):
                            with self.subTest(skill=name, action=action, flag=mapping["flag"]):
                                self.assertIn(mapping["flag"], checked[key])


if __name__ == "__main__":
    unittest.main()
