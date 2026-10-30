import unittest
from unittest.mock import patch
from scripts.audit_rust_dependencies import audit_lockfile, main


class AuditRunnerTests(unittest.TestCase):
    def test_clean_report_uses_the_committed_lockfile(self):
        with patch("scripts.audit_rust_dependencies.subprocess.run") as run:
            run.return_value.returncode = 0
            run.return_value.stdout = '{"vulnerabilities":{"count":0,"list":[]}}'
            run.return_value.stderr = ""
            self.assertEqual(audit_lockfile("Cargo.lock"), [])
            args = run.call_args.args[0]
            self.assertIn("ARGS=audit --json --file Cargo.lock", args)
            self.assertNotIn("generate-lockfile", " ".join(args))

    def test_infrastructure_failure_cannot_pass_as_a_clean_report(self):
        with patch("scripts.audit_rust_dependencies.subprocess.run") as run:
            run.return_value.returncode = 2
            run.return_value.stdout = '{"vulnerabilities":{"count":0,"list":[]}}'
            run.return_value.stderr = ""
            with self.assertRaisesRegex(ValueError, "Audit failed"):
                audit_lockfile("Cargo.lock")

    def test_invalid_json_is_blocking(self):
        with patch("scripts.audit_rust_dependencies.subprocess.run") as run:
            run.return_value.returncode = 0
            run.return_value.stdout = "registry unavailable"
            run.return_value.stderr = ""
            with self.assertRaisesRegex(ValueError, "Could not audit"):
                audit_lockfile("Cargo.lock")

    def test_a_vulnerability_blocks_but_both_lockfiles_are_audited(self):
        finding = {"advisory": {"id": "RUSTSEC-TEST", "title": "Synthetic finding"},
                   "package": {"name": "fixture", "version": "1.0.0"}}
        with patch("scripts.audit_rust_dependencies.audit_lockfile", side_effect=[[finding], []]) as audit:
            self.assertTrue(main())
            self.assertEqual([call.args[0] for call in audit.call_args_list],
                             ["Cargo.lock", "desktop/src-tauri/Cargo.lock"])


if __name__ == "__main__":
    unittest.main()
