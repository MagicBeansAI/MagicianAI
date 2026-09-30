from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path


SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))

import setup_bot_envs  # noqa: E402


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
