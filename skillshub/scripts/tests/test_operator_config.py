from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch


SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))

import operator_config  # noqa: E402
import setup_gws_accounts  # noqa: E402


class GoogleWorkspaceProfileConfigTests(unittest.TestCase):
    def load_from(self, source: str):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        path = Path(temporary.name) / "operator-config.yaml"
        path.write_text(source)
        return patch.object(operator_config, "CONFIG_FILE", path)

    def test_selectable_and_fixed_profiles_remain_separate_but_both_get_auth_dirs(self):
        source = """
gws_accounts:
  - name: work
    expected_email: work@example.com
gws_fixed_profiles:
  - name: presto
    expected_email: presto@example.com
"""
        with self.load_from(source):
            self.assertEqual(operator_config.gws_account_names(), ["work"])
            self.assertEqual(
                operator_config.load_gws_fixed_profiles(),
                [("presto", "presto@example.com")],
            )
            self.assertEqual(operator_config.gws_profile_names(), ["work", "presto"])

    def test_duplicate_profile_name_across_classes_fails_closed(self):
        source = """
gws_accounts:
  - name: presto
gws_fixed_profiles:
  - name: presto
"""
        with self.load_from(source):
            with self.assertRaisesRegex(ValueError, "must be unique"):
                operator_config.gws_profile_names()

    def test_bootstrap_materializes_selectable_and_fixed_auth_directories(self):
        source = """
gws_accounts:
  - name: work
gws_fixed_profiles:
  - name: presto
"""
        destination = tempfile.TemporaryDirectory()
        self.addCleanup(destination.cleanup)
        client = Path(destination.name) / "client_secret.json"
        client.write_text('{"installed":{}}')
        with (
            self.load_from(source),
            patch.object(setup_gws_accounts, "find_client_secret", return_value=client),
            patch.object(
                sys,
                "argv",
                [
                    "setup_gws_accounts.py",
                    "--scope",
                    "owner/default",
                    "--data-root",
                    destination.name,
                ],
            ),
        ):
            self.assertEqual(setup_gws_accounts.main(), 0)
        for name in ("work", "presto"):
            installed = (
                Path(destination.name)
                / "scopes"
                / "owner"
                / "default"
                / "auth"
                / f"gws-{name}"
                / "client_secret.json"
            )
            self.assertEqual(installed.read_text(), '{"installed":{}}')

    def test_invalid_fixed_profile_shape_uses_bounded_field_diagnostic(self):
        with self.load_from("gws_fixed_profiles: invalid\n"):
            with self.assertRaisesRegex(ValueError, "`gws_fixed_profiles` must be a list"):
                operator_config.load_gws_fixed_profiles()


class SecretReferenceConfigTests(unittest.TestCase):
    def load_from(self, source: str):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        path = root / "operator-config.yaml"
        path.write_text(source)
        return root, patch.object(operator_config, "CONFIG_FILE", path)

    def test_exact_references_resolve_from_runtime_dotenv_without_copying_placeholders(self):
        root, configured = self.load_from(
            "secrets:\n  PROD_KEY: '${PROD_KEY}'\n  DEV_KEY: '$DEV_KEY'\n  MISSING: '${MISSING}'\n"
        )
        (root / ".env").write_text("PROD_KEY=production\n")
        (root / ".env.development").write_text(
            "PROD_KEY=development\nDEV_KEY=development-only\n"
        )
        with (
            configured,
            patch.object(operator_config, "runtime_root", return_value=root),
            patch.dict("os.environ", {}, clear=True),
        ):
            secrets = operator_config.load_secrets()
        self.assertEqual(secrets["PROD_KEY"], "production")
        self.assertEqual(secrets["DEV_KEY"], "development-only")
        self.assertEqual(secrets["MISSING"], "")

    def test_process_environment_wins_and_literal_values_remain_compatible(self):
        root, configured = self.load_from(
            "secrets:\n  REFERENCED: '${REFERENCED}'\n  LITERAL: legacy-value\n"
        )
        (root / ".env").write_text("REFERENCED=production\n")
        with (
            configured,
            patch.object(operator_config, "runtime_root", return_value=root),
            patch.dict("os.environ", {"REFERENCED": "explicit"}, clear=True),
        ):
            secrets = operator_config.load_secrets()
        self.assertEqual(secrets["REFERENCED"], "explicit")
        self.assertEqual(secrets["LITERAL"], "legacy-value")


if __name__ == "__main__":
    unittest.main()
