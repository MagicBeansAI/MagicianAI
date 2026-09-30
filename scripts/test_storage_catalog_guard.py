"""Schema and drift tests for the canonical storage catalog guard."""

from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("storage_catalog_guard.py")
SPEC = importlib.util.spec_from_file_location("storage_catalog_guard", SCRIPT)
assert SPEC and SPEC.loader
guard = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(guard)


MINIMAL_OWNER = {
    "id": "example_store",
    "class": "authoritative",
    "tier": 1,
    "scope": "tenant",
    "capability": "domain_repository",
    "owner_module": "magician/src/magician_v2/storage_governance/mod.rs",
    "lifecycle_owner": "example_store",
    "current_layout": "scopes/{principal}/{workspace}/example.json",
    "writers": ["magician/src/magician_v2/storage_governance/mod.rs"],
    "readers": ["magician/src/magician_v2/storage_governance/mod.rs"],
    "governance_id": None,
    "readiness": {"state": "discovered", "evidence": {}},
    "authority": {
        "state": "local_active",
        "canonical_profile": "local_embedded",
        "fencing_generation": 0,
        "last_migration_id": None,
    },
    "legacy_source": {"state": "retained"},
}


def catalog_doc(owners, rust_ids=None, io_files=None):
    return {
        "schema_version": 1,
        "authority": {
            "source_of_truth": "catalog_yaml",
            "governance_inventory": "projection",
        },
        "measurements": {
            "captured_at": "2026-08-31",
            "commit": "test",
            "notes": "fixture",
            "provisional_plan_bound": "60-120 closure packets",
            "owner_count": len(owners),
            "governance_entry_count": len(rust_ids or []),
            "direct_io_files": io_files or [],
        },
        "owners": owners,
    }


class StorageCatalogSchemaTests(unittest.TestCase):
    def test_rejects_owner_missing_required_fields(self):
        broken = dict(MINIMAL_OWNER)
        del broken["lifecycle_owner"]
        failures = guard.validate_catalog(catalog_doc([broken]), rust_governance_ids=set())
        self.assertTrue(any("lifecycle_owner" in item for item in failures))

    def test_rejects_non_discovered_readiness_without_evidence(self):
        owner = dict(MINIMAL_OWNER)
        owner["readiness"] = {"state": "characterized", "evidence": {}}
        failures = guard.validate_catalog(catalog_doc([owner]), rust_governance_ids=set())
        self.assertTrue(any("characterized" in item and "evidence" in item for item in failures))

    def test_discovered_may_have_empty_evidence(self):
        failures = guard.validate_catalog(
            catalog_doc([MINIMAL_OWNER]), rust_governance_ids=set()
        )
        self.assertEqual(failures, [])

    def test_characterized_requires_characterized_key(self):
        owner = dict(MINIMAL_OWNER)
        owner["readiness"] = {
            "state": "characterized",
            "evidence": {"wrapped_local": "src/local.rs"},
        }
        failures = guard.validate_catalog(catalog_doc([owner]), rust_governance_ids=set())
        self.assertTrue(
            any("missing evidence characterized" in item for item in failures)
        )

    def test_characterized_with_required_key_passes(self):
        owner = dict(MINIMAL_OWNER)
        owner["readiness"] = {
            "state": "characterized",
            "evidence": {"characterized": "tests/char.rs"},
        }
        failures = guard.validate_catalog(catalog_doc([owner]), rust_governance_ids=set())
        self.assertEqual(failures, [])

    def test_remote_ready_requires_every_predecessor_key(self):
        owner = dict(MINIMAL_OWNER)
        keys = guard.required_readiness_evidence_keys("remote_ready")
        evidence = {key: f"tests/{key}.rs" for key in keys}
        evidence.pop("restore_qualified")
        owner["readiness"] = {"state": "remote_ready", "evidence": evidence}
        failures = guard.validate_catalog(catalog_doc([owner]), rust_governance_ids=set())
        self.assertTrue(
            any("missing evidence restore_qualified" in item for item in failures)
        )

    def test_duplicate_owner_ids_fail(self):
        failures = guard.validate_catalog(
            catalog_doc([MINIMAL_OWNER, dict(MINIMAL_OWNER)]),
            rust_governance_ids=set(),
        )
        self.assertTrue(any("duplicate" in item for item in failures))

    def test_unknown_class_or_tier_fails(self):
        owner = dict(MINIMAL_OWNER)
        owner["class"] = "host-durable"
        owner["tier"] = 9
        failures = guard.validate_catalog(catalog_doc([owner]), rust_governance_ids=set())
        self.assertTrue(any("class" in item for item in failures))
        self.assertTrue(any("tier" in item for item in failures))


class StorageCatalogGovernanceDriftTests(unittest.TestCase):
    def test_rust_id_missing_from_catalog_fails(self):
        failures = guard.validate_catalog(
            catalog_doc([MINIMAL_OWNER]),
            rust_governance_ids={"analytics_duckdb"},
        )
        self.assertTrue(any("analytics_duckdb" in item for item in failures))

    def test_catalog_governance_id_missing_from_rust_fails(self):
        owner = dict(MINIMAL_OWNER)
        owner["governance_id"] = "analytics_duckdb"
        failures = guard.validate_catalog(
            catalog_doc([owner]),
            rust_governance_ids=set(),
        )
        self.assertTrue(any("analytics_duckdb" in item for item in failures))

    def test_matching_governance_ids_pass(self):
        owner = dict(MINIMAL_OWNER)
        owner["governance_id"] = "analytics_duckdb"
        failures = guard.validate_catalog(
            catalog_doc([owner], rust_ids=["analytics_duckdb"]),
            rust_governance_ids={"analytics_duckdb"},
        )
        self.assertEqual(failures, [])


class StorageCatalogDirectIoTests(unittest.TestCase):
    def test_unlisted_on_disk_connection_open_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            src = root / "crate" / "src"
            src.mkdir(parents=True)
            (src / "store.rs").write_text(
                "fn open() { Connection::open(path)?; }\n",
                encoding="utf-8",
            )
            scanned = guard.scan_on_disk_connection_opens(root, ["crate/src"])
            failures = guard.validate_direct_io(
                catalog_doc([MINIMAL_OWNER], io_files=[]),
                scanned,
            )
            self.assertTrue(any("crate/src/store.rs" in item for item in failures))

    def test_allowlisted_file_passes(self):
        failures = guard.validate_direct_io(
            catalog_doc(
                [MINIMAL_OWNER],
                io_files=[
                    {
                        "path": "crate/src/store.rs",
                        "owner_id": "example_store",
                        "kind": "sqlite",
                    }
                ],
            ),
            {"crate/src/store.rs"},
        )
        self.assertEqual(failures, [])

    def test_allowlist_entry_without_owner_fails(self):
        failures = guard.validate_direct_io(
            catalog_doc(
                [MINIMAL_OWNER],
                io_files=[
                    {
                        "path": "crate/src/orphan.rs",
                        "owner_id": "not_a_real_owner",
                        "kind": "sqlite",
                    }
                ],
            ),
            {"crate/src/orphan.rs"},
        )
        self.assertTrue(any("not_a_real_owner" in item for item in failures))


class StorageCatalogLiveRepositoryTests(unittest.TestCase):
    def test_live_catalog_passes_the_guard(self):
        self.assertEqual(guard.run(), [])

    def test_live_governance_copies_agree(self):
        live = guard.extract_live_governance_ids()
        fixture = guard.extract_fixture_governance_ids()
        self.assertEqual(live, fixture)
        self.assertGreaterEqual(len(live), 30)

    def test_owner_modules_exist(self):
        catalog = guard.load_catalog()
        missing = guard.missing_owner_paths(catalog)
        self.assertEqual(missing, [])


if __name__ == "__main__":
    unittest.main()
