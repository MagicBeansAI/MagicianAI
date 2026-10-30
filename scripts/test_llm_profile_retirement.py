from pathlib import Path
import hashlib
import tempfile
import unittest

import yaml

from migrate_llm_catalog import convert
from prepare_llm_profile_retirement import backup_inventory, canonical_target, inventory_config, inventory_runtime, occurrences, selection_migration, record_owner, execution_lifecycle
from test_llm_model_catalog import legacy_root


class ProfileRetirementPreparationTests(unittest.TestCase):
    def test_execution_lifecycle_requires_matching_terminal_owners_without_leaking_bodies(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            relative = Path("scopes/p/w/tasks/t/executions/e")
            execution = root / relative
            execution.mkdir(parents=True)
            (execution / "state.json").write_text('{"status":"completed","body":"PRIVATE"}')
            (execution / "execution.json").write_text('{"execution":{"waiting_state":"Completed","title":"PRIVATE"}}')
            report = execution_lifecycle(root, relative)
            self.assertTrue(report["terminal_consistent"])
            self.assertEqual(set(report["source_sha256"]), {"state.json", "execution.json"})
            self.assertNotIn("PRIVATE", str(report))
            (execution / "execution.json").write_text('{"execution":{"waiting_state":"Running"}}')
            self.assertFalse(execution_lifecycle(root, relative)["terminal_consistent"])
            (execution / "execution.json").write_text('{"execution":{"waiting_state":"PRIVATE ERROR PAYLOAD"}}')
            unknown = execution_lifecycle(root, relative)
            self.assertEqual(unknown["execution_waiting_state"], "unknown")
            self.assertNotIn("PRIVATE", str(unknown))

    def test_storage_owner_keeps_snapshots_separate_from_current_definitions(self):
        prefix = "scopes/p/w/"
        self.assertEqual(record_owner(Path(prefix + "agent_runtime/agents/a/definition.agent.yaml")), "agent_definition_store")
        self.assertEqual(record_owner(Path(prefix + "tasks/t/executions/e/llm_routing_overrides.json")), "execution_snapshot_requires_lifecycle_audit")
        self.assertEqual(record_owner(Path(prefix + "coding_engine/runs/t/magician_data_v3/system/agent_templates/agents/a/definition.agent.yaml")), "coding_source_snapshot")
        self.assertEqual(record_owner(Path("system/agent_templates/agents/a/definition.agent.yaml")), "packaged_agent_template")
        self.assertEqual(record_owner(Path("system/llm_routing_overrides.json")), "install_routing_store")
        self.assertEqual(record_owner(Path("scopes/p/w/unknown/definition.agent.yaml")), "unclassified_owned_file_requires_review")

    def test_offline_map_preserves_transport_thinking_and_assignment_authority(self):
        catalog = {
            "models": {"provider-chat/model": {"selectable": True, "coding": True,
                "thinking": {"levels": ["none", "high"]}}},
            "assignments": {"memory_extract": {"model": "provider-chat/model", "thinking": "high",
                "policy": "background", "metadata": {"response_format": "json"}}},
            "aliases": {"old-chain": "old-fixed", "old-fixed": "memory_extract",
                        "old-auto": "model/provider-chat/model/auto"},
        }
        before = yaml.safe_dump(catalog)
        report = selection_migration(catalog, [{"id": "coding-old", "llm_profile": "old-fixed"}])
        fixed = report["routes"]["old-chain"]
        self.assertEqual(fixed["assignment"], "memory_extract")
        self.assertEqual(fixed["model_selection"], {"model_id": "provider-chat/model", "thinking": "high"})
        self.assertTrue(fixed["client_resolvable"])
        self.assertIsNone(report["routes"]["old-auto"]["assignment"])
        self.assertEqual(report["routes"]["old-auto"]["model_selection"]["thinking"], "auto")
        self.assertEqual(report["coding"]["coding-old"]["engine"], "pi")
        self.assertFalse(report["runtime_alias_resolver"])
        self.assertEqual(yaml.safe_dump(catalog), before)

    def test_offline_map_refuses_unavailable_preferences_and_unknown_models(self):
        catalog = {"models": {"p/m": {"selectable": False, "thinking": {"levels": ["high"]}}},
                   "assignments": {"work": {"model": "p/m", "thinking": "none"}},
                   "aliases": {"old": "work"}}
        report = selection_migration(catalog, [{"id": "old-code", "llm_profile": "old"}])
        self.assertFalse(report["routes"]["old"]["client_resolvable"])
        self.assertFalse(report["coding"]["old-code"]["client_resolvable"])
        catalog["assignments"]["work"]["model"] = "missing"
        with self.assertRaisesRegex(ValueError, "unknown model"):
            selection_migration(catalog, [])

    def test_alias_definition_is_not_an_active_reference(self):
        value = {"llm": {"router": {"catalog": {"aliases": {"old-a": "old-b"}},
                                  "default_profile": "old-a"}},
                 "pins": {"profile": "old-b"}}
        hits = occurrences(value, {"old-a", "old-b"})
        self.assertEqual({hit["field"] for hit in hits}, {"llm/router/default_profile", "pins/profile"})

    def test_cyclic_and_missing_aliases_cannot_be_reported_as_migratable(self):
        with self.assertRaisesRegex(ValueError, "cyclic"):
            canonical_target("a", {"a": "b", "b": "a"})
        with tempfile.TemporaryDirectory() as directory:
            config = Path(directory) / "magician-config.yaml"
            config.write_text(yaml.safe_dump({"llm": {"router": {"catalog": {
                "models": {"test": {}}, "aliases": {"old": "missing"}}}}}))
            with self.assertRaisesRegex(ValueError, "unresolved"):
                inventory_config(config)

    def test_config_inventory_preserves_inputs_and_fallback_contracts(self):
        root = legacy_root()
        root["llm"]["router"]["profiles"]["old-a"]["metadata"]["fallback_profile"] = "old-b"
        catalog, mappings = convert(root)
        root["llm"]["router"] = {"catalog": catalog, "operation_mapping": mappings}
        with tempfile.TemporaryDirectory() as directory:
            config = Path(directory) / "magician-config.yaml"
            config.write_text(yaml.safe_dump(root))
            before = config.read_bytes()
            report, known = inventory_config(config)
            self.assertEqual(config.read_bytes(), before)
            self.assertEqual(report["source_sha256"][str(config.resolve())], hashlib.sha256(before).hexdigest())
            self.assertIsNone(report["fallback_edges"][0]["legacy_target"])
            self.assertEqual(report["fallback_edges"][0]["target"], catalog["aliases"]["old-b"])
            self.assertEqual(report["fallback_edges"][0]["canonical_target"], catalog["aliases"]["old-b"])
            self.assertEqual(known, {"old-a", "old-b"})

    def test_runtime_scan_reads_enum_tags_but_excludes_history_and_record_text(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            active = root / "scopes" / "p" / "w" / "agents" / "agent"
            active.mkdir(parents=True)
            definition = active / "definition.agent.yaml"
            definition.write_text("llm_routing:\n  planning: !Profile\n    profile: old-a\nprompt: PRIVATE RECORD BODY\n")
            history = active / "history"
            history.mkdir()
            (history / "definition.agent.yaml").write_text("profile: old-b\n")
            before = definition.read_bytes()
            report = inventory_runtime(root, {"old-a", "old-b"})
            self.assertEqual(report["records_scanned"], 1)
            self.assertFalse(report["errors"])
            hits = report["records_with_legacy_references"][0]["references"]
            self.assertEqual(hits, [{"field": "llm_routing/planning/profile", "id": "old-a", "kind": "value"}])
            self.assertNotIn("PRIVATE RECORD BODY", str(report))
            self.assertEqual(definition.read_bytes(), before)
            self.assertTrue(report["not_covered"], "file inventory is not a full retirement proof")

    def test_private_snapshot_copies_verified_sources_without_changing_them(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "magician-config.yaml"
            source.write_text("source bytes\n")
            report = {"source_sha256": {str(source): hashlib.sha256(source.read_bytes()).hexdigest()}}
            result = backup_inventory(report, root / "backup")
            self.assertEqual(result["files"], 1)
            self.assertEqual((root / "backup/config/magician-config.yaml").read_bytes(), source.read_bytes())
            self.assertEqual((root / "backup").stat().st_mode & 0o777, 0o700)
            self.assertEqual((root / "backup/manifest.json").stat().st_mode & 0o777, 0o600)
            with self.assertRaises(FileExistsError):
                backup_inventory(report, root / "backup")

    def test_stale_source_refuses_snapshot_before_creating_destination(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "magician-config.yaml"
            source.write_text("changed\n")
            report = {"source_sha256": {str(source): hashlib.sha256(b"original\n").hexdigest()}}
            with self.assertRaisesRegex(ValueError, "changed after inventory"):
                backup_inventory(report, root / "backup")
            self.assertFalse((root / "backup").exists())


if __name__ == "__main__":
    unittest.main()
