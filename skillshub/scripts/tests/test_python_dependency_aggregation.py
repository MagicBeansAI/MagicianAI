from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).resolve().parents[1] / "aggregate_python_packages.py"
SPEC = importlib.util.spec_from_file_location("aggregate_python_packages", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
AGGREGATOR = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(AGGREGATOR)


class PythonDependencyAggregationTests(unittest.TestCase):
    def test_bootstrap_and_adapter_dependencies_survive_regeneration(self) -> None:
        packages = AGGREGATOR.aggregate()

        self.assertEqual(packages["PyYAML"], ["<baseline>"])
        self.assertEqual(packages["beautifulsoup4"], ["htmltotext"])
        self.assertEqual(packages["maigret==0.6.1"], ["find-details-by-username"])
        self.assertEqual(packages["yt-dlp"], ["youtube-search", "media-fetch"])

    def test_csvkit_is_owned_so_setup_python_installs_its_binaries(self):
        """csvkit declares 13 binaries and 26 actions but had no setup path.

        Its console scripts resolved only from whatever Python happened to be
        on the host PATH — on the machine this was found, an ad-hoc
        `/Library/Frameworks/Python.framework/Versions/3.11/bin/`. It was the
        only CLI pack in skillshub with declared bins and no setup target, so
        a fresh install got 26 dead actions that appeared to work here purely
        by accident.
        """
        from aggregate_python_packages import ADAPTER_PACKAGES
        self.assertIn("csvkit", ADAPTER_PACKAGES)
        self.assertEqual(set(ADAPTER_PACKAGES["csvkit"]), {"csvkit"})

    def test_every_adapter_package_names_a_real_pack(self):
        """A tuple entry is a pack directory name, not free text.

        A typo here silently owns nothing: the package still installs, but the
        `# Required by:` provenance points at a pack that does not exist, and
        nobody learns which skill breaks if it is dropped.
        """
        from aggregate_python_packages import ADAPTER_PACKAGES, SKILLSHUB
        for package, owners in ADAPTER_PACKAGES.items():
            for owner in owners:
                with self.subTest(package=package, owner=owner):
                    self.assertTrue(
                        (SKILLSHUB / owner / "SKILL.md").is_file(),
                        f"{package} claims owner {owner!r}, which has no SKILL.md",
                    )

    def test_yt_dlp_is_owned_by_both_media_packs(self):
        """yt-dlp backs youtube-search and media-fetch; regeneration must keep both."""
        from aggregate_python_packages import ADAPTER_PACKAGES
        self.assertEqual(
            set(ADAPTER_PACKAGES["yt-dlp"]),
            {"youtube-search", "media-fetch"},
        )


if __name__ == "__main__":
    unittest.main()
