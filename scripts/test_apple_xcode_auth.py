import json
from pathlib import Path
import tempfile
import unittest

from apple_xcode_auth import authentication_arguments


class XcodeAuthenticationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        self.repo = root / "repo"
        (self.repo / "magios").mkdir(parents=True)
        (self.repo / "magios/Signing.local.xcconfig").write_text("DEVELOPMENT_TEAM = PAIDTEAM01\n")
        self.key = root / "private key.p8"
        self.key.write_text("fixture key stays outside the repository")
        self.metadata = root / "metadata.json"
        self.metadata.write_text(json.dumps(dict(team_id="PAIDTEAM01", key_id="KEY123",
            issuer_id="issuer-123", private_key_path=str(self.key))))

    def args(self, *arguments):
        return authentication_arguments(arguments, repository=self.repo, metadata_path=self.metadata)

    def test_matching_local_team_uses_the_imported_key_without_reading_its_contents(self):
        self.assertEqual(self.args("build"), ["-authenticationKeyPath", str(self.key.resolve()),
            "-authenticationKeyID", "KEY123", "-authenticationKeyIssuerID", "issuer-123"])

    def test_explicit_team_and_credentials_take_precedence(self):
        self.assertEqual(self.args("DEVELOPMENT_TEAM=OTHERTEAM1", "build"), [])
        self.assertEqual(self.args("-authenticationKeyPath", "/explicit/key.p8", "build"), [])

    def test_missing_assets_keep_the_normal_xcode_account_flow(self):
        self.key.unlink()
        self.assertEqual(self.args("build"), [])
        self.metadata.unlink()
        self.assertEqual(self.args("build"), [])

    def test_credential_inside_the_repository_is_never_forwarded(self):
        unsafe = self.repo / "key.p8"
        unsafe.write_text("fixture")
        data = json.loads(self.metadata.read_text())
        data["private_key_path"] = str(unsafe)
        self.metadata.write_text(json.dumps(data))
        self.assertEqual(self.args("build"), [])

    def test_malformed_metadata_keeps_the_normal_xcode_account_flow(self):
        for text in ("not json", "[]", '{"team_id":"PAIDTEAM01"}'):
            self.metadata.write_text(text)
            self.assertEqual(self.args("build"), [])
