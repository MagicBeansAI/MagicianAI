"""Provider-free migration, reader and Rust/Python parity contracts."""
from __future__ import annotations

import copy
import json
import os
from pathlib import Path
import tempfile
import unittest

import yaml

from llm_catalog_config import materialize_router, selection_for_routing_handle
from magician_config_text import read_config_text
from migrate_llm_catalog import convert, render_tables, remove_legacy_router_tables, migrate_coding_default, materialize_migration_router


def legacy_root() -> dict:
    return {"llm": {"router": {
        "profiles": {
            "old-a": {"provider": "openai", "model": "gpt-6.1-sol", "api_key_env": "EXAMPLE_KEY",
                      "supports_tool_calling": True, "supports_vision": False,
                      "max_output_tokens": 4096, "context_window_tokens": 32768,
                      "reasoning": {"effort": "low"}, "metadata": {"tool_choice": "auto"}},
            "old-b": {"provider": "openai", "model": "gpt-6-luna", "api_key_env": "EXAMPLE_KEY",
                      "supports_tool_calling": True, "supports_reasoning": False, "max_output_tokens": 8192,
                      "metadata": {"tool_choice": "auto"}},
        },
        "operation_mapping": {"review": {"default": "old-a", "when_cloud": "old-b", "description": "Review evidence", "group": "Review", "engine": "pinned"}},
        "default_profile": "old-a",
    }}}


def normalize(value):
    if isinstance(value, dict):
        return {key: normalize(item) for key, item in value.items() if item is not None}
    if isinstance(value, list):
        return [normalize(item) for item in value]
    return value


class ModelCatalogTests(unittest.TestCase):
    def test_script_requests_use_server_model_metadata_and_refuse_unavailable_choices(self):
        overview = {"models": [{"id": "custom/model", "choices": {"auto": "opaque-auto", "high": "opaque-high"}}],
                    "profiles": [{"name": "operation/review", "model_selection": {"model_id": "custom/model", "thinking": "high"}}]}
        self.assertEqual(selection_for_routing_handle(overview, "operation/review"), {"model_id": "custom/model", "thinking": "high"})
        self.assertEqual(selection_for_routing_handle(overview, "opaque-auto"), {"model_id": "custom/model", "thinking": "auto"})
        for handle in ["missing", "", "opaque-low"]:
            with self.assertRaises(ValueError): selection_for_routing_handle(overview, handle)
        overview["models"][0]["installed"] = False
        with self.assertRaises(ValueError): selection_for_routing_handle(overview, "operation/review")

    def test_coding_models_are_qualified_once_and_catalog_upgrade_is_idempotent(self):
        root = legacy_root()
        root["coding"] = {"default_profile": "saved-coding", "profiles": [
            {"id": "saved-coding", "llm_profile": "old-a"},
            {"id": "disabled", "llm_profile": "old-b", "enabled": False}]}
        catalog, mapping = convert(root)
        qualified = [key for key, model in catalog["models"].items() if model.get("coding")]
        self.assertEqual(len(qualified), 1)
        root["llm"]["router"] = {"catalog": catalog, "operation_mapping": mapping}
        self.assertEqual(convert(root), (catalog, mapping))
        text = "coding:\n  default_profile: saved-coding\nchat:\n  default_profile: leave-this-alone\n"
        migrated = migrate_coding_default(text, root, catalog)
        self.assertIn(f"default_profile: pi/model/{qualified[0]}/auto", migrated)
        self.assertIn("default_profile: leave-this-alone", migrated)

    def test_migration_preserves_routes_restrictions_and_conditional_policy(self):
        root = legacy_root()
        root["llm"]["router"]["operation_mapping"]["simple"] = "old-b"
        original = copy.deepcopy(root)
        catalog, mapping = convert(root)
        self.assertEqual(root, original, "migration must not mutate its source")
        self.assertEqual(mapping, original["llm"]["router"]["operation_mapping"],
                         "trust declarations and context adapters bind exact selector identities")
        resolved = materialize_migration_router({"catalog": catalog, "operation_mapping": mapping})
        self.assertEqual(resolved["profiles"]["old-a"]["model"], "gpt-6.1-sol")
        self.assertFalse(resolved["profiles"]["old-a"]["supports_vision"])
        self.assertFalse(resolved["profiles"]["old-b"]["supports_reasoning"])
        self.assertEqual(resolved["profiles"]["old-a"]["context_window_tokens"], 32768)
        selector = resolved["operation_mapping"]["review"]
        self.assertEqual(selector["engine"], "pinned")
        self.assertEqual(selector["group"], "Review")
        self.assertEqual(resolved["profiles"][selector["when_cloud"]]["model"], "gpt-6-luna")

    def test_assignments_do_not_carry_authored_output_or_thinking_token_counts(self):
        catalog, _ = convert(legacy_root())
        for assignment in catalog["assignments"].values():
            self.assertNotIn("max_output_tokens", assignment)
            self.assertNotIn("max_reasoning_tokens", assignment)
        for model in catalog["models"].values():
            if model["model"] == "gpt-6.1-sol":
                self.assertNotIn("none", model["thinking"]["levels"])

    def test_fallback_preserves_the_target_assignment_through_saved_aliases(self):
        root = legacy_root()
        root["llm"]["router"]["profiles"]["old-a"]["metadata"]["fallback_profile"] = "old-b"
        catalog, mapping = convert(root)
        resolved = materialize_migration_router({"catalog": catalog, "operation_mapping": mapping})
        primary = resolved["profiles"]["old-a"]
        fallback_id = primary["metadata"]["fallback_profile"]
        self.assertEqual(fallback_id, catalog["aliases"]["old-b"], "fallback must use the same target assignment without an alias lookup")
        self.assertNotIn("fallback_profile", catalog["assignments"][catalog["aliases"]["old-a"]].get("metadata", {}))
        fallback = resolved["profiles"][fallback_id]
        self.assertEqual(fallback["model"], "gpt-6-luna")
        self.assertEqual(fallback["reasoning"]["effort"], "none")
        self.assertFalse(fallback["supports_reasoning"])
        self.assertEqual(fallback["metadata"]["tool_choice"], "auto")

    def test_fallback_cycles_and_conflicting_migration_metadata_are_refused(self):
        catalog, mapping = convert(legacy_root())
        primary, retry = catalog["aliases"]["old-a"], catalog["aliases"]["old-b"]
        catalog["assignments"][primary]["fallback"] = retry
        root = {"llm": {"router": {"catalog": catalog, "operation_mapping": mapping}}}
        self.assertEqual(convert(root)[0], catalog, "explicit fallback upgrade must be idempotent")
        catalog["assignments"][retry]["fallback"] = primary
        with self.assertRaisesRegex(ValueError, "cyclic"):
            convert(root)
        with self.assertRaisesRegex(ValueError, "cyclic"):
            strict = copy.deepcopy(root["llm"]["router"])
            strict["catalog"].pop("aliases")
            materialize_router(strict)
        del catalog["assignments"][retry]["fallback"]
        catalog["assignments"][primary]["metadata"]["fallback_profile"] = "old-a"
        with self.assertRaisesRegex(ValueError, "conflicting"):
            convert(root)

    def test_local_selection_has_a_stable_id_and_preserves_the_runtime_anchor(self):
        outputs = []
        for model in ("gemma4:12b", "qwen3.8:27b"):
            root = {"runtime": {"ollama": {}}, "llm": {"router": {
                "profiles": {"old-local": {"provider": "ollama", "model": model,
                    "api_base_url": "http://localhost:11434/api/generate", "context_window_tokens": 32768,
                    "supports_tool_calling": True, "max_output_tokens": 4096,
                    "metadata": {"options": {"num_ctx": 32768}, "tool_choice": "none"}}},
                "operation_mapping": {"extract": "old-local"}}}}
            catalog, mapping = convert(root)
            text = render_tables(catalog, mapping)
            self.assertIn("model: *local_generation_model", text)
            self.assertNotIn(model, text)
            outputs.append(text)
        self.assertEqual(outputs[0], outputs[1])

    def test_inline_tables_move_to_the_sibling_without_touching_other_sections(self):
        text = """runtime:
  model: &local_generation_model local
llm:
  router:
    default_profile: old-a
    profiles:
      old-a: {provider: openai, model: gpt-6.1-sol}
    adaptive_profiles:
      old-auto: {fast_profile: old-a, thinking_profile: old-a}
    operation_mapping: {review: old-a}
chat:
  profiles:
    unrelated: keep
"""
        main = yaml.safe_load(remove_legacy_router_tables(text))
        self.assertEqual(main["llm"]["router"], {"default_profile": "old-a"})
        self.assertEqual(main["chat"]["profiles"], {"unrelated": "keep"})
        self.assertIn("&local_generation_model", remove_legacy_router_tables(text))

    def test_invalid_alias_graph_is_refused(self):
        with self.assertRaisesRegex(ValueError, "cyclic"):
            materialize_migration_router({"catalog": {"aliases": {"a": "b", "b": "a"}}})

    def test_normal_readers_refuse_retired_tables_even_when_empty(self):
        for router in ({"profiles": {}}, {"adaptive_profiles": {}}, {"catalog": {"aliases": {}}}):
            with self.subTest(router=router), self.assertRaisesRegex(ValueError, "retired"):
                materialize_router(router)
            with tempfile.TemporaryDirectory() as directory:
                path = Path(directory) / "magician-config.yaml"
                raw = yaml.safe_dump({"llm": {"router": router}}, sort_keys=False)
                path.write_text(raw)
                with self.assertRaisesRegex(ValueError, "retired"):
                    read_config_text(path)
                self.assertEqual(read_config_text(path, expand_catalog=False), raw,
                                 "one-time inventories can still read inert old configuration")

    def test_normal_readers_refuse_reserved_metadata_from_every_catalog_owner(self):
        for owner in ("connections", "models", "assignments"):
            for key in ("fallback_profile", "_magician_model_settings"):
                catalog = {owner: {"test": {"metadata": {key: "injected"}}}}
                with self.subTest(owner=owner, key=key), self.assertRaises(ValueError):
                    materialize_router({"catalog": catalog})

    def test_feedback_is_catalog_owned_and_preserves_owner_overrides(self):
        catalog, mapping = convert(legacy_root())
        self.assertEqual(catalog["models"]["openai/gpt-6-luna"]["feedback"], "fast")
        catalog["models"]["openai/gpt-6-luna"]["feedback"] = "standard"
        root = {"llm": {"router": {"catalog": catalog, "operation_mapping": mapping}}}
        self.assertEqual(convert(root)[0]["models"]["openai/gpt-6-luna"]["feedback"], "standard")

    def test_off_upgrade_preserves_defaults_and_owner_subsets(self):
        catalog, mapping = convert(legacy_root())
        catalog["connections"]["sarvam"] = {"provider": "sarvam"}
        catalog["models"]["sarvam/test"] = {"connection": "sarvam", "model": "sarvam-105b",
            "thinking": {"mode": "effort", "levels": ["low", "medium", "high"], "default": "low"}}
        root = {"llm": {"router": {"catalog": catalog, "operation_mapping": mapping}}}
        upgraded, _ = convert(root)
        self.assertEqual(upgraded["models"]["sarvam/test"]["thinking"],
            {"mode": "effort", "levels": ["none", "low", "medium", "high"], "default": "low"})
        catalog["models"]["sarvam/test"]["thinking"]["levels"] = ["medium", "high"]
        self.assertEqual(convert(root)[0]["models"]["sarvam/test"]["thinking"]["levels"], ["medium", "high"])

    def test_auto_uses_defaults_without_generated_escalation_pairs(self):
        catalog, mapping = convert(legacy_root())
        catalog.pop("aliases")
        expanded = materialize_router({"catalog": catalog, "operation_mapping": mapping})
        self.assertNotIn("adaptive_profiles", expanded)
        self.assertEqual(expanded["profiles"]["model/openai/gpt-6-luna/auto"]["reasoning"]["effort"], "medium")
        self.assertNotIn("model/openai/gpt-6-luna/auto/initial", expanded["profiles"])

    def test_repository_config_has_one_catalog_and_complete_generated_mappings(self):
        path = Path(__file__).resolve().parents[1] / "magician-config.yaml"
        raw = yaml.safe_load(read_config_text(path, expand_catalog=False))["llm"]["router"]
        self.assertNotIn("profiles", raw)
        self.assertNotIn("adaptive_profiles", raw)
        expanded = materialize_router(raw)
        self.assertGreater(len(raw["catalog"]["models"]), 20)
        self.assertNotIn("adaptive_profiles", expanded)
        for model in raw["catalog"]["models"].values():
            self.assertIn(model["feedback"], {"fast", "standard", "premium"})
        for assignment in raw["catalog"]["assignments"].values():
            self.assertNotIn("fallback_profile", assignment.get("metadata", {}))
            if "fallback" in assignment:
                self.assertIn(assignment["fallback"], raw["catalog"]["assignments"])
        for selector in expanded["operation_mapping"].values():
            arms = [selector] if isinstance(selector, str) else [selector[key] for key in ("default", "when_cloud", "when_has_images") if key in selector]
            for name in arms:
                self.assertIn(name, expanded["profiles"])
        self.assertNotIn("aliases", raw["catalog"])
        self.assertEqual(len(raw["catalog"]["assignments"]), 42)
        self.assertEqual(expanded["profiles"]["operation/chat_history_compaction"]["metadata"]["tool_choice"], {"type": "none"})

    @unittest.skipUnless(os.environ.get("LLM_CATALOG_RUST_SNAPSHOT"), "run the catalog gate for Rust/Python parity")
    def test_rust_and_script_reader_resolve_identical_routes(self):
        rust = json.loads(Path(os.environ["LLM_CATALOG_RUST_SNAPSHOT"]).read_text())
        path = Path(__file__).resolve().parents[1] / "magician-config.yaml"
        python = yaml.safe_load(read_config_text(path))["llm"]["router"]
        for section in ("profiles", "operation_mapping"):
            self.assertEqual(normalize(rust[section]), normalize(python[section]), section)


if __name__ == "__main__":
    unittest.main()
