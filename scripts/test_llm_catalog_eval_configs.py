"""Eval snapshots must remain valid catalog configuration, not dispatch dumps."""
from __future__ import annotations

import importlib.util
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

import yaml

from magician_config_text import read_config_text

SCRIPT = Path(__file__).with_name("eval-ollama-logical-chunking.py")
SPEC = importlib.util.spec_from_file_location("catalog_logical_chunk_eval", SCRIPT)
assert SPEC and SPEC.loader
EVAL = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = EVAL
SPEC.loader.exec_module(EVAL)
CONFIG = Path(__file__).resolve().parents[1] / "magician-config.yaml"


class CatalogEvalConfigTests(unittest.TestCase):
    def test_candidate_clones_only_the_selected_local_model_and_keeps_catalog_authority(self):
        source = {path: path.read_bytes() for path in (CONFIG, CONFIG.with_name("llm-router.yaml"))}
        before = EVAL.router(CONFIG)
        with tempfile.TemporaryDirectory() as directory:
            snapshot, _links = EVAL.materialize_eval_config(CONFIG, Path(directory), "local-candidate-fixture")
            raw = yaml.safe_load(read_config_text(snapshot, expand_catalog=False))
            router = raw["llm"]["router"]
            self.assertIn("catalog", router)
            self.assertNotIn("profiles", router)
            self.assertNotIn("adaptive_profiles", router)
            self.assertNotIn("aliases", router["catalog"])
            after = EVAL.router(snapshot)
            candidates = {row[2] for row in EVAL.SPECS}
            selected_models = set()
            for name in candidates:
                selected = router["catalog"]["assignments"][name]["model"]
                selected_models.add(selected)
                self.assertEqual(router["catalog"]["models"][selected]["model"], "local-candidate-fixture")
                self.assertEqual(after["profiles"][name]["provider"], "ollama")
                self.assertEqual(after["profiles"][name]["model"], "local-candidate-fixture")
            self.assertEqual(len(selected_models), 1, "shared model facts are cloned once")
            for name, definition in before["profiles"].items():
                if name not in candidates:
                    self.assertEqual(after["profiles"][name], definition, name)
        self.assertEqual({path: path.read_bytes() for path in source}, source)

    def test_a_cloud_assignment_cannot_be_rewritten_as_a_local_candidate(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            with patch.object(EVAL, "SPECS", (("query_analysis", "fixture", "operation/query_analysis", ()),)):
                with self.assertRaisesRegex(SystemExit, "must be an Ollama assignment"):
                    EVAL.materialize_eval_config(CONFIG, output, "local-candidate-fixture")
            self.assertFalse((output / "eval-runtime").exists())


if __name__ == "__main__":
    unittest.main()
