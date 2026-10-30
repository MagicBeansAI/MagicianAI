"""Config-owner retirement contracts; all writes use disposable directories."""
import copy
import json
from pathlib import Path
import tempfile
import unittest

import yaml

from magician_config_text import read_config_text
from migrate_llm_catalog import render_tables
from prepare_llm_profile_retirement import InventoryLoader
from retire_llm_profile_config import convert, prepare, rewrite_main


def fixture():
    return {
        "runtime": {"model": "fixture-local"},
        "policy": {"level": 1},
        "coding": {"default_profile": "old-coding", "profiles": [{"id": "old-coding", "llm_profile": "old"}]},
        "app_platform": {"processing": {"local_profile": "old", "profiles": {"old": {"class": "loopback_managed"}}}},
        "llm": {"router": {"default_profile": "old", "operation_mapping": {"test": "old"}, "catalog": {
            "connections": {"local": {"provider": "ollama"}},
            "models": {"local/model": {"model": "fixture-local", "label": "Fixture", "connection": "local", "coding": True}},
            "policies": {},
            "assignments": {"operation/test": {"model": "local/model", "fallback": "migration/retry"},
                            "migration/retry": {"model": "local/model"}, "unused": {"model": "local/model"}},
            "aliases": {"old": "operation/test"},
        }}},
    }


class RetirementConfigTest(unittest.TestCase):
    def test_coding_table_removal_preserves_adjacent_fields_and_comments(self):
        for text in [
            "coding: {profiles: [], default_profile: old}\n",
            "coding: {default_profile: old, profiles: []}\n",
            "coding: {profiles: []}\n",
            "coding:\n  profiles: []\n# Following owner comment.\nnext: true\n",
            "coding:\n  profiles:\n    - id: old\n      llm_profile: old\n# Following owner comment.\nnext: true\n",
        ]:
            with self.subTest(text=text):
                expected = yaml.safe_load(text)
                expected["coding"].pop("profiles")
                result = rewrite_main(text, {})
                self.assertEqual(yaml.safe_load(result), expected)
                if "# Following owner comment." in text:
                    self.assertIn("# Following owner comment.", result)

    def test_explicit_source_workloads_are_kept_and_unknown_ones_are_refused(self):
        result, plan, _ = convert(fixture(), {"unused": "workload/shadow_baseline"}, ["unused"])
        self.assertIn("workload/shadow_baseline", result["llm"]["router"]["catalog"]["assignments"])
        self.assertEqual(plan["explicit_source_assignment_roots"], ["workload/shadow_baseline"])
        with self.assertRaisesRegex(ValueError, "unknown"):
            convert(fixture(), {}, ["missing"])

    def test_config_roots_keep_fallback_closure_and_rewrite_binding_keys_together(self):
        source = fixture()
        original = copy.deepcopy(source)
        result, plan, _ = convert(source, {"migration/retry": "workload/retry"})
        self.assertEqual(source, original)
        router = result["llm"]["router"]
        self.assertEqual(set(router["catalog"]["assignments"]), {"operation/test", "workload/retry"})
        self.assertEqual(router["catalog"]["assignments"]["operation/test"]["fallback"], "workload/retry")
        self.assertEqual(router["operation_mapping"]["test"], "operation/test")
        self.assertEqual(result["app_platform"]["processing"]["local_profile"], "operation/test")
        self.assertEqual(set(result["app_platform"]["processing"]["profiles"]), {"operation/test"})
        self.assertEqual(result["coding"]["default_profile"], "pi/model/local/model/auto")
        self.assertNotIn("profiles", result["coding"])
        self.assertNotIn("aliases", router["catalog"])
        self.assertEqual(plan["remaining_hashed_assignments"], [])
        self.assertFalse(plan["activation_ready"])

    def test_unsafe_collisions_cycles_and_unknown_renames_are_refused(self):
        source = fixture()
        source["app_platform"]["processing"]["profiles"]["operation/test"] = {"class": "external"}
        with self.assertRaisesRegex(ValueError, "collision"):
            convert(source, {})
        source = fixture()
        source["llm"]["router"]["catalog"]["assignments"]["migration/retry"]["fallback"] = "operation/test"
        with self.assertRaisesRegex(ValueError, "cyclic"):
            convert(source, {})
        with self.assertRaisesRegex(ValueError, "retained"):
            convert(fixture(), {"unused": "workload/unused"})

    def test_exact_emitted_pair_preserves_comments_anchors_tags_and_source_bytes(self):
        with tempfile.TemporaryDirectory() as tmp:
            base = Path(tmp)
            source = fixture()
            main = base / "magician-config.yaml"
            main.write_text('''# Owner comment remains.
runtime:
  model: &local_generation_model fixture-local
policy: !Exact {level: 1}
coding:
  default_profile: old-coding # Keep this explanation.
  profiles:
    - id: old-coding
      llm_profile: old
app_platform:
  processing:
    local_profile: old
    profiles:
      old: {class: loopback_managed}
llm:
  router:
    default_profile: old
''')
            router = source["llm"]["router"]
            tables = base / "llm-router.yaml"
            tables.write_text(render_tables(router["catalog"], router["operation_mapping"]))
            before = (main.read_bytes(), tables.read_bytes())
            output = base / "preview"
            plan = prepare(main, output, {"migration/retry": "workload/retry"})
            self.assertEqual((main.read_bytes(), tables.read_bytes()), before)
            emitted = yaml.load(read_config_text(output / main.name, expand_catalog=False), Loader=InventoryLoader)
            self.assertEqual(emitted, convert(source, {"migration/retry": "workload/retry"})[0])
            result = (output / main.name).read_text()
            self.assertIn("# Owner comment remains.", result)
            self.assertIn("# Keep this explanation.", result)
            self.assertIn("&local_generation_model", result)
            self.assertIn("!Exact", result)
            self.assertNotIn("aliases:", (output / tables.name).read_text())
            self.assertEqual(json.loads((output / "plan.json").read_text()), plan)
            self.assertEqual(plan["script_reader_operation_parity"]["operations"], 1)
            self.assertEqual(plan["script_reader_operation_parity"]["selector_arms"], 1)
            self.assertEqual(plan["script_reader_operation_parity"]["request_or_fallback_contract_differences"], [])
            self.assertEqual((output.stat().st_mode & 0o777), 0o700)
            self.assertEqual(((output / main.name).stat().st_mode & 0o777), 0o600)
            with self.assertRaisesRegex(ValueError, "new directory"):
                prepare(main, output, {})


if __name__ == "__main__":
    unittest.main()
