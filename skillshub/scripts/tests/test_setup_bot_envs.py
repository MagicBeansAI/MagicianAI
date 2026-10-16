from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path


SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))

import setup_bot_envs  # noqa: E402


class AgentMailBotEnvTests(unittest.TestCase):
    def test_materializes_signing_secret_without_logging_it(self):
        with tempfile.TemporaryDirectory() as directory:
            log = []
            setup_bot_envs.write_agentmail(Path(directory), {
                "AGENT_MAIL_KEY": "test-api-key",
                "AGENTMAIL_WEBHOOK_SECRET": "whsec_test-only-canary",
            }, False, log)
            target = Path(directory) / "agentmail/.env.development"
            self.assertIn("AGENTMAIL_WEBHOOK_SECRET=whsec_test-only-canary\n", target.read_text())
            self.assertNotIn("whsec_test-only-canary", "\n".join(log))
            self.assertFalse(any("refuse to start" in line for line in log))

    def test_api_key_alone_reports_missing_verified_webhook_configuration(self):
        with tempfile.TemporaryDirectory() as directory:
            log = []
            setup_bot_envs.write_agentmail(Path(directory), {"AGENT_MAIL_KEY": "test-key"}, False, log)
            self.assertTrue(any("AGENTMAIL_WEBHOOK_SECRET or AGENTMAIL_WEBHOOK_URL" in line for line in log))

    def test_registration_url_satisfies_setup_and_existing_settings_are_preserved(self):
        with tempfile.TemporaryDirectory() as directory:
            log = []
            target = Path(directory) / "agentmail/.env.development"
            setup_bot_envs.write_agentmail(Path(directory), {
                "AGENT_MAIL_KEY": "test-key", "AGENTMAIL_WEBHOOK_URL": "https://example.invalid",
            }, False, log)
            original = target.read_text()
            self.assertFalse(any("refuse to start" in line for line in log))
            self.assertEqual(setup_bot_envs.write_agentmail(Path(directory), {
                "AGENT_MAIL_KEY": "new-key", "AGENTMAIL_WEBHOOK_SECRET": "new-secret",
            }, False, log), 0)
            self.assertEqual(target.read_text(), original)


class KapsoBotEnvTests(unittest.TestCase):
    def test_materializes_webhook_secret_into_scoped_bot_env(self):
        with tempfile.TemporaryDirectory() as directory:
            log: list[str] = []
            written = setup_bot_envs.write_kapso(
                Path(directory),
                {
                    "KAPSO_API_KEY": "kps_test",
                    "KAPSO_PHONE_NUMBER_ID": "123456",
                    "KAPSO_WEBHOOK_SECRET": "wh_sec_test",
                },
                False,
                log,
            )

            self.assertEqual(written, 1)
            env = (Path(directory) / "kapso" / ".env.development").read_text()
            self.assertIn("KAPSO_WEBHOOK_SECRET=wh_sec_test\n", env)
            self.assertNotIn("wh_sec_test", "\n".join(log))

    def test_missing_webhook_secret_is_explicit_and_warned(self):
        with tempfile.TemporaryDirectory() as directory:
            log: list[str] = []
            setup_bot_envs.write_kapso(
                Path(directory),
                {
                    "KAPSO_API_KEY": "kps_test",
                    "KAPSO_PHONE_NUMBER_ID": "123456",
                },
                False,
                log,
            )

            env = (Path(directory) / "kapso" / ".env.development").read_text()
            self.assertIn("KAPSO_WEBHOOK_SECRET=\n", env)
            self.assertTrue(any("will refuse to start" in line for line in log))


if __name__ == "__main__":
    unittest.main()
