#!/usr/bin/env python3
"""Custody persistence and replacement preflight; never starts a real runtime."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "provision", Path(__file__).with_name("prepare-container-keyring.py"))
provision = importlib.util.module_from_spec(spec)
spec.loader.exec_module(provision)


class Custody(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.data = self.root / "runtime"
        self.data.mkdir()
        self.home = self.root / "custody"

    def prepare(self):
        return provision.provision(self.data, self.home)

    def test_recreation_reuses_secret_and_state_outside_runtime(self):
        first = self.prepare()
        password = Path(first["password_file"]).read_bytes()
        marker = Path(first["state_dir"]) / "encrypted-fixture"
        marker.write_bytes(b"existing-keyring")
        (self.data / "system").mkdir()
        (self.data / "system" / "paired-devices.json").write_text("sealed-fixture")
        self.assertEqual(first, self.prepare())
        self.assertEqual(password, Path(first["password_file"]).read_bytes())
        self.assertEqual(marker.read_bytes(), b"existing-keyring")
        self.assertNotIn(password.decode(), json.dumps(first))
        self.assertFalse(Path(first["state_dir"]).is_relative_to(self.data))
        self.assertEqual(Path(first["password_file"]).stat().st_mode & 0o777, 0o600)
        self.assertIn(first["password_file"] + ":/run/secrets/magician-keyring-password:ro", first["launch_args"])

    def test_runtime_alias_resolves_to_same_custody(self):
        first = self.prepare()
        alias = self.root / "alias"
        alias.symlink_to(self.data)
        self.assertEqual(first, provision.provision(alias, self.home))

    def test_other_runtime_gets_distinct_credentials(self):
        first = self.prepare()
        other = self.root / "second-runtime"
        other.mkdir()
        second = provision.provision(other, self.home)
        self.assertNotEqual(first["state_dir"], second["state_dir"])
        self.assertNotEqual(Path(first["password_file"]).read_bytes(), Path(second["password_file"]).read_bytes())

    def test_engine_switch_requires_explicit_migration(self):
        self.prepare()
        with self.assertRaisesRegex(provision.ProvisionError, "migration"):
            provision.provision(self.data, self.home, "docker")

    def test_docker_uses_nonroot_host_uid_for_private_bind_mounts(self):
        plan = provision.provision(self.data, self.home, "docker")
        args = plan["launch_args"]
        self.assertEqual(args[args.index("--user")+1], f"{os.getuid()}:{os.getgid()}")
        self.assertTrue(any(arg.endswith(":/etc/passwd:ro") for arg in args))

    def test_docker_account_map_preserves_image_users_and_resolves_host_uid(self):
        passwd = 'root:x:0:0:root:/root:/bin/sh\nmagician:x:997:997:runtime:/home/magician:/bin/sh\n'
        groups = 'root:x:0:\nmagician:x:997:\n'
        users, mapped_groups = provision.docker_accounts(passwd, groups, 1000, 1000)
        self.assertTrue(users.startswith(passwd))
        self.assertIn('magician-host-1000:x:1000:1000:', users)
        self.assertTrue(mapped_groups.startswith(groups))
        self.assertEqual(provision.docker_accounts(users, mapped_groups, 1000, 1000), (users, mapped_groups))
        with self.assertRaises(provision.ProvisionError):
            provision.docker_accounts(passwd, groups, 0, 0)

    def test_system_state_requires_explicit_migration(self):
        (self.data / "system").mkdir()
        (self.data / "system" / "paired-devices.json").write_text("old-seal")
        with self.assertRaisesRegex(provision.ProvisionError, "existing keyring"):
            self.prepare()
        self.assertEqual((self.data / "system" / "paired-devices.json").read_text(), "old-seal")
        self.assertEqual(list(self.home.glob("*/unlock.secret")), [])

    def test_existing_system_migration_stays_pending_until_completed(self):
        (self.data / "system").mkdir()
        (self.data / "system" / "state.json").write_text("sealed-fixture")
        plan = provision.provision(
            self.data, self.home, migration_source="macos-keychain")
        with self.assertRaisesRegex(provision.ProvisionError, "incomplete"):
            self.prepare()
        provision.complete_migration(plan)
        self.assertEqual(plan, self.prepare())
        record = json.loads((Path(plan["state_dir"]).parent / "manifest.json").read_text())
        self.assertEqual(record["migration"], {
            "source": "macos-keychain", "status": "complete"})

    def test_migration_sends_values_only_over_stdin(self):
        (self.data / "system").mkdir()
        (self.data / "system" / "state.json").write_text("sealed-fixture")
        plan = provision.provision(
            self.data, self.home, migration_source="macos-keychain")
        values = [
            {"service": "service-one", "account": "account-one", "value": "private-one"},
            {"service": "service-two", "account": "account-two", "value": "private-two"},
        ]
        result = subprocess.CompletedProcess(
            [], 0, stdout=b"MAGICIAN_KEYRING_MIGRATION_OK\n", stderr=b"")
        with patch.object(provision, "load_macos_keychain_entries", return_value=values), \
                patch.object(provision.subprocess, "run", return_value=result) as run:
            provision.migrate_macos_keychain(plan, "engine", "image")
        command = run.call_args.args[0]
        self.assertNotIn("private-one", " ".join(command))
        self.assertNotIn("private-two", " ".join(command))
        self.assertIn(b"private-one", run.call_args.kwargs["input"])
        self.assertIn(b"private-two", run.call_args.kwargs["input"])

    def test_missing_secret_never_rotates_or_regenerates(self):
        plan = self.prepare()
        secret = Path(plan["password_file"])
        secret.unlink()
        with self.assertRaises(FileNotFoundError):
            self.prepare()
        self.assertFalse(secret.exists())

    def test_changed_secret_is_refused_without_repair(self):
        secret = Path(self.prepare()["password_file"])
        secret.write_bytes(b"other-secret" * 8)
        with self.assertRaisesRegex(provision.ProvisionError, "changed"):
            self.prepare()
        self.assertEqual(secret.read_bytes(), b"other-secret" * 8)

    def test_missing_manifest_never_reseeds(self):
        secret = Path(self.prepare()["password_file"])
        original = secret.read_bytes()
        (secret.parent / "manifest.json").unlink()
        with self.assertRaises(FileNotFoundError):
            self.prepare()
        self.assertEqual(secret.read_bytes(), original)

    def test_missing_state_is_not_recreated(self):
        state = Path(self.prepare()["state_dir"])
        state.rmdir()
        with self.assertRaises(FileNotFoundError):
            self.prepare()
        self.assertFalse(state.exists())

    def test_public_symlink_and_hardlink_secrets_are_refused(self):
        secret = Path(self.prepare()["password_file"])
        secret.chmod(0o644)
        with self.assertRaises(provision.ProvisionError):
            self.prepare()
        secret.chmod(0o600)
        alias = secret.parent / "copy"
        os.link(secret, alias)
        with self.assertRaises(provision.ProvisionError):
            self.prepare()
        alias.unlink()
        secret.rename(alias)
        secret.symlink_to(alias)
        with self.assertRaises(provision.ProvisionError):
            self.prepare()

    def test_custody_inside_runtime_is_refused(self):
        with self.assertRaises(provision.ProvisionError):
            provision.provision(self.data, self.data / "keys")
        self.assertFalse((self.data / "keys").exists())

    def test_existing_public_custody_is_not_chmodded(self):
        self.home.mkdir(mode=0o755)
        with self.assertRaises(provision.ProvisionError):
            self.prepare()
        self.assertEqual(self.home.stat().st_mode & 0o777, 0o755)

    def test_manifest_cannot_be_retargeted_to_another_runtime(self):
        secret = Path(self.prepare()["password_file"])
        manifest = secret.parent / "manifest.json"
        value = json.loads(manifest.read_text())
        value["runtime_root"] = str(self.root / "other")
        manifest.write_text(json.dumps(value))
        with self.assertRaises(provision.ProvisionError):
            self.prepare()

    def inspection(self, plan, apple):
        mounts = [(plan["runtime_root"], "/data", False),
                  (plan["state_dir"], "/keyring", False),
                  (plan["password_file"], "/run/secrets/magician-keyring-password", True)]
        env = ["MAGICIAN_ROOT_DIR=/data", "MAGICIAN_KEYRING_STATE_DIR=/keyring",
               "MAGICIAN_KEYRING_PASSWORD_FILE=/run/secrets/magician-keyring-password"]
        if apple:
            return [{"configuration": {"id": "fixture", "initProcess": {"environment": env},
                     "mounts": [{"source": source, "destination": target,
                                 "options": ["ro"] if ro else []} for source, target, ro in mounts]}}]
        bundle = str(Path(plan['state_dir']).parent)
        mounts += [(bundle+'/passwd', '/etc/passwd', True), (bundle+'/group', '/etc/group', True)]
        return [{"Config": {"Env": env, "User": f"{os.getuid()}:{os.getgid()}"}, "Mounts": [
            {"Source": source, "Destination": target, "RW": not ro, "Type": "bind"}
            for source, target, ro in mounts]}]

    def test_both_runtimes_accept_only_same_root_and_readonly_secret_mount(self):
        plan = self.prepare()
        for apple in (True, False):
            with self.subTest(apple=apple):
                runtime = "apple-container" if apple else "docker"
                listing = json.dumps([{"configuration": {"id": "fixture"}}]) if apple else "fixture\n"
                inspection = self.inspection(plan, apple)
                with patch.object(provision, "run_cli", side_effect=[listing, json.dumps(inspection)]):
                    provision.validate_existing(plan, runtime, "engine", "fixture")
                if apple:
                    inspection[0]["configuration"]["mounts"][2]["options"] = []
                else:
                    inspection[0]["Mounts"][2]["RW"] = True
                with patch.object(provision, "run_cli", side_effect=[listing, json.dumps(inspection)]):
                    with self.assertRaisesRegex(provision.ProvisionError, "migration"):
                        provision.validate_existing(plan, runtime, "engine", "fixture")

    def test_inspection_error_never_means_container_missing(self):
        plan = self.prepare()
        with patch.object(provision, "run_cli", side_effect=provision.ProvisionError("offline")):
            with self.assertRaises(provision.ProvisionError):
                provision.validate_existing(plan, "apple-container", "engine", "fixture")

    def test_unknown_inventory_never_authorizes_replacement(self):
        plan = self.prepare()
        for output in ('{}', '[{"id":"fixture"}]', '[null]'):
            with patch.object(provision, "run_cli", return_value=output):
                with self.assertRaisesRegex(provision.ProvisionError, "inventory"):
                    provision.validate_existing(plan, "apple-container", "engine", "fixture")

    def test_extra_mounts_are_not_silently_dropped_on_recreation(self):
        plan = self.prepare()
        inspection = self.inspection(plan, True)
        inspection[0]["configuration"]["mounts"].append({
            "source": "/extra-owned-data", "destination": "/extra", "options": []})
        with patch.object(provision, "run_cli", side_effect=[json.dumps(inspection), json.dumps(inspection)]):
            with self.assertRaisesRegex(provision.ProvisionError, "migration"):
                provision.validate_existing(plan, "apple-container", "engine", "fixture")

    def test_guest_preflight_is_bounded_and_never_launches_supervisor(self):
        plan = self.prepare()
        result = subprocess.CompletedProcess([], 0, stdout=b'')
        with patch.object(provision.subprocess, "run", return_value=result) as run:
            provision.verify_image(plan, "engine", "image")
        args = run.call_args.args[0]
        self.assertEqual(args[args.index("--cpus") + 1], "1")
        self.assertEqual(args[args.index("--memory") + 1], "512m")
        self.assertEqual(args[args.index("--entrypoint") + 1], "python3")
        self.assertNotIn("magic-supervisor", " ".join(args))
        self.assertNotIn(Path(plan["password_file"]).read_text(), " ".join(args))

    def test_docker_named_or_anonymous_volumes_require_explicit_migration(self):
        plan = self.prepare()
        for kind in ("volume", "tmpfs"):
            with self.subTest(kind=kind):
                inspection = self.inspection(plan, False)
                inspection[0]["Mounts"].append({"Type": kind, "Destination": "/extra",
                    "Source": "/var/lib/docker/volumes/private", "RW": True})
                with patch.object(provision, "run_cli", side_effect=["fixture\n", json.dumps(inspection)]):
                    with self.assertRaisesRegex(provision.ProvisionError, "migration"):
                        provision.validate_existing(plan, "docker", "engine", "fixture")

    def test_duplicate_mount_destinations_are_refused(self):
        plan = self.prepare()
        for apple in (True, False):
            with self.subTest(apple=apple):
                inspection = self.inspection(plan, apple)
                mounts = inspection[0]["configuration"]["mounts"] if apple else inspection[0]["Mounts"]
                mounts.append(dict(mounts[0]))
                listing = json.dumps([{"configuration": {"id": "fixture"}}]) if apple else "fixture\n"
                with patch.object(provision, "run_cli", side_effect=[listing, json.dumps(inspection)]):
                    with self.assertRaisesRegex(provision.ProvisionError, "ambiguous"):
                        provision.validate_existing(plan, "apple-container" if apple else "docker", "engine", "fixture")


if __name__ == "__main__":
    unittest.main()
