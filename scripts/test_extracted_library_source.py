"""Source-ownership regression cases; no Cargo, Git, or network is invoked."""
from pathlib import Path
import os
import platform
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import check_phase5g_conformance as conformance
import with_extracted_library as libraries


class PinnedLibrarySourceTests(unittest.TestCase):
    def test_private_git_fetch_uses_the_normal_credential_helper_without_an_env_override(self):
        configuration = (Path(__file__).resolve().parents[1] / ".cargo/config.toml").read_text(encoding="utf-8")
        self.assertRegex(configuration, r"(?s)\[net\]\s+git-fetch-with-cli\s*=\s*true")

    def test_magicrun_test_lanes_wrap_apple_targets_only(self):
        script = libraries.FIXTURE_TEMP_RUNNER
        self.assertTrue(script.is_file())
        wrapped = libraries.magicrun_fixture_runner_env("aarch64-apple-darwin")
        self.assertEqual(
            wrapped,
            {"CARGO_TARGET_AARCH64_APPLE_DARWIN_RUNNER": f"/bin/sh {script}"},
        )
        self.assertEqual(libraries.magicrun_fixture_runner_env("x86_64-apple-darwin")["CARGO_TARGET_X86_64_APPLE_DARWIN_RUNNER"], f"/bin/sh {script}")
        self.assertEqual(libraries.magicrun_fixture_runner_env("x86_64-unknown-linux-gnu"), {})

    def test_magicrun_fixture_temp_is_the_canonical_darwin_user_temp(self):
        if platform.system() != "Darwin":
            self.skipTest("darwin user temp")
        raw = subprocess.check_output(["/usr/bin/getconf", "DARWIN_USER_TEMP_DIR"], text=True).strip()
        canonical = os.path.realpath(raw)
        observed = subprocess.check_output(
            ["/bin/sh", str(libraries.FIXTURE_TEMP_RUNNER), "/usr/bin/printenv", "TMPDIR"],
            text=True,
        ).strip()
        self.assertEqual(observed, canonical)
        self.assertFalse(os.path.islink(canonical))
        self.assertNotEqual(observed, raw.rstrip("/"))

    def test_foundation_lane_is_explicit_and_does_not_replace_compatibility(self):
        self.assertTrue({"prepare", "check", "test", "test-compatibility", "test-foundation"}.issubset(libraries.LANES["magicvault"]))
        self.assertNotIn("test-foundation", libraries.LANES["magicrun"])

    def test_consumer_compatibility_lane_selects_only_named_test_targets(self):
        makefile = (Path(__file__).resolve().parents[1] / "Makefile").read_text(encoding="utf-8").replace('"$(MAGICIAN_CARGO)"', 'cargo')
        recipe = makefile.split("\ntest-magicvault-compatibility: setup-extracted-libraries\n", 1)[1].split("\n\n", 1)[0]
        cargo_commands = [line.strip() for line in recipe.splitlines() if line.strip().startswith("cargo ")]
        self.assertEqual(cargo_commands, [
            "cargo test -p magician-core --lib durable_io",
            "cargo test -p magician --test magicvault_facade",
            "cargo test -p magician --lib magician_v2::secrets",
        ])
        secrets_recipe = makefile.split("\ntest-magicvault-secrets:\n", 1)[1].split("\n\n", 1)[0]
        self.assertEqual(secrets_recipe.strip(), "cargo test --locked -p magician --lib magician_v2::secrets")

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.revision = "a" * 40
        self.url = "https://github.com/MagicBeansAI/MagicRun.git"
        self.write_manifest(self.url, self.revision)
        self.root_patch = patch.object(libraries, "ROOT", self.root)
        self.root_patch.start()
        self.addCleanup(self.root_patch.stop)

    def write_manifest(self, url, revision):
        (self.root / "Cargo.toml").write_text(
            '[workspace.dependencies]\n'
            f'tool-runtime-core = {{ git = "{url}", rev = "{revision}" }}\n',
            encoding="utf-8",
        )

    def create_cache_directory(self):
        _, _, checkout = libraries.checkout_location("magicrun")
        checkout.mkdir(parents=True)
        return checkout

    def test_exact_pin_is_selected_from_the_consumers_manifest(self):
        self.assertEqual(libraries.revision_for("magicrun"), (self.url, self.revision))

    def test_branch_or_short_revision_cannot_substitute_for_a_pin(self):
        for revision in ("main", "abcdef0"):
            with self.subTest(revision=revision):
                self.write_manifest(self.url, revision)
                with self.assertRaises(ValueError):
                    libraries.revision_for("magicrun")

    def test_unreviewed_repository_is_rejected(self):
        self.write_manifest("https://example.test/unreviewed.git", self.revision)
        with self.assertRaises(ValueError):
            libraries.revision_for("magicrun")

    def test_missing_cache_does_not_fetch_source(self):
        with patch.object(libraries.subprocess, "run") as run:
            with self.assertRaisesRegex(ValueError, "setup-extracted-libraries"):
                libraries.prepared_checkout("magicrun")
            run.assert_not_called()

    def test_clean_cached_pin_is_accepted_without_modification(self):
        checkout = self.create_cache_directory()
        with patch.object(libraries, "git", side_effect=[self.url, self.revision, ""]) as git:
            self.assertEqual(libraries.prepared_checkout("magicrun"), checkout)
            self.assertEqual([call.args[1] for call in git.call_args_list],
                             ["remote", "rev-parse", "status"])

    def test_dirty_or_wrong_revision_cache_is_not_reset(self):
        self.create_cache_directory()
        for responses in ([self.url, "b" * 40], [self.url, self.revision, " M source.rs"]):
            with self.subTest(responses=responses):
                with patch.object(libraries, "git", side_effect=responses) as git:
                    with self.assertRaises(ValueError):
                        libraries.prepared_checkout("magicrun")
                    self.assertNotIn("reset", [call.args[1] for call in git.call_args_list])

    def test_historical_conformance_labels_resolve_only_to_the_pinned_source(self):
        checkout = self.root / "upstream"
        roots = {}
        with patch.object(conformance, "prepared_checkout", return_value=checkout) as prepare:
            for filename in ("browser_profile_adapter.rs", "strategy_adapter.rs"):
                relative = "tool-runtime-core/src/" + filename
                self.assertEqual(conformance.evidence_path(relative, roots), checkout / relative)
            prepare.assert_called_once_with("magicrun")

    def test_product_evidence_stays_local(self):
        relative = "magician-mcp-client/src/lib.rs"
        with patch.object(conformance, "prepared_checkout") as prepare:
            self.assertEqual(conformance.evidence_path(relative, {}), conformance.ROOT / relative)
            prepare.assert_not_called()

    def test_missing_library_evidence_is_not_silently_skipped(self):
        with patch.object(conformance, "prepared_checkout", side_effect=ValueError("missing pin")):
            with self.assertRaisesRegex(ValueError, "missing pin"):
                conformance.evidence_path("tool-runtime-core/src/strategy_adapter.rs", {})

    def test_evidence_cannot_escape_its_source_root(self):
        for relative in ("../source.rs", "/source.rs", "tool-runtime-core/../../source.rs"):
            with self.subTest(relative=relative):
                with self.assertRaises(ValueError):
                    conformance.evidence_path(relative, {})


if __name__ == "__main__":
    unittest.main()
