#!/usr/bin/env python3
"""Offline safety checks for the explicit mobile protocol qualification lane."""
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "mobile_probe", Path(__file__).with_name("qualify-mobile-connectivity.py"))
probe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(probe)
OWNER = "http://127.0.0.1:3004"
PUBLIC = "https://mobile.example.test"
ACCESS = {"client_id": "test-access-id", "client_secret": "test-access-secret"}


class MobileProtocol(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.state = Path(self.temp.name) / "probe.json"
        self.kind = None
        self.devices = {}

    def remote(self, origin, path, *, method="GET", body=None, headers=None, expected=200):
        headers = headers or {}
        if path == "/api/magician/v2/devices":
            self.assertEqual(origin, OWNER)
            self.assertEqual(headers, {})
            return {"principal": "owner", "workspace": "test"}
        if path.endswith("/enrollment"):
            self.assertEqual(origin, OWNER)
            self.assertEqual(headers, {})
            self.kind = body["client_kind"]
            return {"enrollment_id": "ticket", "enrollment_uri":
                    f"magican://connect?kind={self.kind}&base={PUBLIC}&secret=one-time"}
        self.assertEqual(origin, PUBLIC)
        if path.endswith("/enrollment/exchange") or path.endswith("/pair"):
            if path.endswith("/exchange"):
                # QR exchange must work before a client has Access credentials.
                self.assertEqual(headers, {})
                self.assertEqual(body["secret"], "one-time")
            else:
                self.kind = "esp32"
                self.assertEqual(headers, probe.access_headers({"cloudflare_access": ACCESS}))
            if expected == 410:
                return {}
            grant = {"device_id": body["device_id"], "principal": "owner", "workspace": "test",
                     "token": "test-device-token", "client_kind": self.kind,
                     "capabilities": ["mobile_client"]}
            if self.kind != "esp32":
                grant["cloudflare_access"] = ACCESS
            self.devices[body["device_id"]] = grant
            return grant
        self.assertEqual(path, "/api/magician/v2/devices/me")
        self.assertEqual(headers["CF-Access-Client-Secret"], ACCESS["client_secret"])
        if "Authorization" not in headers or headers["Authorization"] == "Bearer invalid-probe-token":
            self.assertEqual(expected, 401)
            return {}
        self.assertEqual(headers["X-Principal"], "forged-mobile-probe")
        return self.devices[headers["X-Magician-Device-Id"]]

    def enroll(self):
        with patch.object(probe, "request", side_effect=self.remote), contextlib.redirect_stdout(io.StringIO()):
            probe.enroll(OWNER, self.state, PUBLIC)

    def test_https_clients_and_loopback_owner_use_separate_credentials(self):
        self.enroll()
        state = probe.read_state(self.state, OWNER, PUBLIC)
        self.assertEqual({d["kind"] for d in state["devices"]}, {"ios", "android", "esp32"})
        self.assertEqual(self.state.stat().st_mode & 0o777, 0o600)
        self.assertFalse(state["hardware_acceptance"])
        for d in state["devices"]:
            self.assertEqual(d["cloudflare_access"], ACCESS)
        with self.assertRaises(probe.harness.ProbeError):
            probe.read_state(self.state, OWNER, "https://another.example.test")
        with self.assertRaises(probe.harness.ProbeError):
            probe.read_state(self.state, OWNER)

    def test_grant_saved_for_revocation_before_later_assertion_fails(self):
        def fail_after_grant(origin, path, **kwargs):
            result = self.remote(origin, path, **kwargs)
            if path.endswith("/exchange"):
                result["capabilities"] = ["apps_automation"]
            return result
        with patch.object(probe, "request", side_effect=fail_after_grant):
            with self.assertRaises(probe.harness.ProbeError):
                probe.enroll(OWNER, self.state, PUBLIC)
        state = probe.read_state(self.state, OWNER, PUBLIC)
        self.assertEqual(len(state["devices"]), 1)
        self.assertEqual(state["devices"][0]["token"], "test-device-token")

    def test_revoke_uses_owner_and_checks_public_credential_rejection(self):
        self.enroll()
        calls = []
        def revoke(origin, path, **kwargs):
            calls.append((origin, path))
            if kwargs.get("method") == "DELETE":
                self.assertEqual(origin, OWNER)
                self.assertIsNone(kwargs.get("headers"))
            else:
                self.assertEqual(origin, PUBLIC)
                self.assertEqual(kwargs["expected"], 401)
                self.assertIn("Authorization", kwargs["headers"])
            return {}
        with patch.object(probe, "request", side_effect=revoke), patch("sys.argv", [
                "probe", "revoke", "--origin", OWNER, "--public-origin", PUBLIC,
                "--state-file", str(self.state)]), contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(probe.main(), 0)
        self.assertEqual(len(calls), 6)

    def test_authenticated_owner_bearer_never_reaches_public_clients_or_saved_state(self):
        owner_token = "mag_owner-probe-only"
        owner_calls = []
        def authenticated_owner(origin, path, **kwargs):
            headers = kwargs.get("headers") or {}
            if origin == OWNER:
                self.assertEqual(headers, {"Authorization": "Bearer " + owner_token})
                owner_calls.append(path)
                kwargs["headers"] = None
            else:
                self.assertNotIn(owner_token, json.dumps(kwargs))
            return self.remote(origin, path, **kwargs)
        with patch.object(probe, "request", side_effect=authenticated_owner), \
                patch.dict(os.environ, {"PROBE_OWNER": owner_token}), patch("sys.argv", [
                    "probe", "enroll", "--origin", OWNER, "--public-origin", PUBLIC,
                    "--owner-token-env", "PROBE_OWNER", "--state-file", str(self.state)]), \
                contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(probe.main(), 0)
        self.assertEqual(len(owner_calls), 3)
        self.assertNotIn(owner_token, self.state.read_text())

    def test_missing_owner_bearer_stops_before_requests_or_state_creation(self):
        with patch.object(probe, "request") as request, patch.dict(os.environ, {}, clear=True), \
                patch("sys.argv", ["probe", "enroll", "--origin", OWNER,
                    "--owner-token-env", "PROBE_OWNER", "--state-file", str(self.state)]), \
                contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(probe.main(), 1)
        request.assert_not_called()
        self.assertFalse(self.state.exists())

    def test_authenticated_revocation_keeps_owner_bearer_on_loopback(self):
        self.enroll()
        owner_token = "mag_owner-revoke-only"
        deleted = []
        checked = []

        def revoke(origin, path, **kwargs):
            headers = kwargs.get("headers") or {}
            if kwargs.get("method") == "DELETE":
                self.assertEqual(origin, OWNER)
                self.assertEqual(headers, {"Authorization": "Bearer " + owner_token})
                deleted.append(path)
            else:
                self.assertEqual(origin, PUBLIC)
                self.assertEqual(kwargs["expected"], 401)
                self.assertEqual(headers["Authorization"], "Bearer test-device-token")
                self.assertNotIn(owner_token, json.dumps(kwargs))
                checked.append(path)
            return {}

        with patch.object(probe, "request", side_effect=revoke), \
                patch.dict(os.environ, {"PROBE_OWNER": owner_token}), patch("sys.argv", [
                    "probe", "revoke", "--origin", OWNER, "--public-origin", PUBLIC,
                    "--owner-token-env", "PROBE_OWNER", "--state-file", str(self.state)]), \
                contextlib.redirect_stdout(io.StringIO()) as output:
            self.assertEqual(probe.main(), 0)
        self.assertEqual(len(deleted), 3)
        self.assertEqual(len(checked), 3)
        self.assertNotIn(owner_token, self.state.read_text())
        self.assertNotIn(owner_token, output.getvalue())

    def test_refuses_other_devices_or_public_plaintext(self):
        self.enroll()
        data = json.loads(self.state.read_text())
        data["devices"][0]["device_id"] = "real-phone"
        self.state.write_text(json.dumps(data))
        with self.assertRaises(probe.harness.ProbeError):
            probe.read_state(self.state, OWNER, PUBLIC)
        for url in ("http://mobile.example.test", "https://user:secret@example.test", PUBLIC + "/path"):
            with self.subTest(url=url), self.assertRaises(probe.harness.ProbeError):
                probe.public_origin(url)


if __name__ == "__main__":
    unittest.main()
