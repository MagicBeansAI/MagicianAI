#!/usr/bin/env python3
"""Provider-free regression coverage for the Phase 0E2 live evaluator."""

from __future__ import annotations

import importlib.util
import json
import sys
import tempfile
import unittest
from dataclasses import asdict
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
MODULE_PATH = ROOT / "scripts/eval-tool-runtime-phase0-agentic-live.py"
SPEC = importlib.util.spec_from_file_location("tool_runtime_phase0_agentic_eval", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)
HELPERS = MODULE.load_module(
    ROOT / "scripts/eval-agentic-decision-rationale-live.py",
    "tool_runtime_phase0_test_helpers",
)
MANIFEST = ROOT / "data/tool-runtime-inventory/phase0-live-agentic-v1.yaml"
BASELINE = ROOT / "data/tool-runtime-inventory/phase0-offline-baseline-v1.json"
SKILLS = ROOT / "skillshub"


class Phase0AgenticEvalTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.manifest, cls.tasks = MODULE.load_contract(MANIFEST, BASELINE)
        cls.baseline = MODULE.load_json(BASELINE, "baseline")
        cls.tools, cls.schema_bytes, cls.digest = MODULE.build_catalog(SKILLS, cls.baseline)
        cls.profile = HELPERS.Profile(
            "test",
            "openai",
            "gpt-5.6-terra",
            "OPENAI_API_KEY",
            10,
            2048,
            None,
            None,
            None,
            None,
        )

    def task(self, task_id: str):
        return next(item for item in self.tasks if item.id == task_id)

    def catalog(self, task):
        return MODULE.task_catalog(
            task,
            self.tools,
            int(self.manifest["surface"]["max_tools_per_request"]),
        )

    def response(self, task, arguments):
        return {
            "id": "resp_test",
            "status": "completed",
            "output": [
                {
                    "type": "function_call",
                    "name": task.expected_tool,
                    "arguments": json.dumps(arguments),
                }
            ],
            "usage": {
                "input_tokens": 100,
                "input_tokens_details": {"cached_tokens": 25},
                "output_tokens": 10,
                "output_tokens_details": {"reasoning_tokens": 2},
            },
        }

    def score(self, task, arguments):
        return MODULE.score_result(
            HELPERS,
            task,
            self.profile,
            1,
            self.catalog(task),
            200,
            self.response(task, arguments),
            80,
            20,
            40,
            {"input_per_m": 1.0, "cache_read_per_m": 0.1, "output_per_m": 2.0},
            None,
        )

    def test_contract_exactly_covers_offline_corpus_and_catalog(self) -> None:
        offline_ids = {
            item["id"] for item in self.baseline["representative_tasks"]
        }
        self.assertEqual({item.id for item in self.tasks}, offline_ids)
        self.assertEqual(len(self.tasks), 12)
        self.assertEqual(self.schema_bytes, 268_761)
        # `build_catalog` only returns when the computed digest equals the
        # offline baseline's, so asserting the live-agentic catalog's digest
        # here could never hold whatever the skills tree contained.
        self.assertEqual(
            self.digest,
            "2f04f0b9436f20139a26ce3923e83b93b9dedfe1abaa887f6783d606781ee0bb",
        )

    def test_every_task_catalog_is_bounded_real_and_distracted(self) -> None:
        for task in self.tasks:
            with self.subTest(task=task.id):
                catalog = self.catalog(task)
                names = {tool["name"] for tool in catalog}
                self.assertIn(task.expected_tool, names)
                self.assertTrue(MODULE.CORE_TOOL_NAMES <= names)
                self.assertLessEqual(
                    len(catalog), self.manifest["surface"]["max_tools_per_request"]
                )
                expected_prefix = task.expected_tool.split("__", 1)[0] + "__"
                self.assertTrue(any(not name.startswith(expected_prefix) for name in names - MODULE.CORE_TOOL_NAMES))

    def test_scoring_requires_exact_schema_and_argument_semantics(self) -> None:
        task = self.task("public-paper-research")
        valid = self.score(
            task,
            {
                "query": "graph neural networks for molecular property prediction",
                "limit": 4,
                "category": "cs.LG",
            },
        )
        self.assertTrue(valid.decision_success)
        wrong_semantics = self.score(
            task,
            {"query": "graph neural networks", "limit": 5, "category": "cs.LG"},
        )
        self.assertTrue(wrong_semantics.schema_validity_pass)
        self.assertFalse(wrong_semantics.argument_semantics_pass)
        wrong_schema = self.score(
            task,
            {
                "query": "graph neural networks",
                "limit": "4",
                "category": "cs.LG",
                "invented": True,
            },
        )
        self.assertFalse(wrong_schema.schema_validity_pass)
        self.assertFalse(wrong_schema.decision_success)

        deep_response = self.response(task, {})
        deep_response["output"][0]["arguments"] = "[" * 1_100 + "0" + "]" * 1_100
        deep = MODULE.score_result(
            HELPERS,
            task,
            self.profile,
            1,
            self.catalog(task),
            200,
            deep_response,
            80,
            20,
            40,
            {"input_per_m": 1.0, "cache_read_per_m": 0.1, "output_per_m": 2.0},
            None,
        )
        self.assertFalse(deep.schema_validity_pass)
        self.assertFalse(deep.decision_success)

    def test_argument_and_runtime_profile_bindings_remain_distinct(self) -> None:
        gmail = self.task("google-profile-read-and-send")
        correct = self.score(
            gmail,
            {
                "to": "phase0-recipient@example.invalid",
                "subject": "Phase 0 hello",
                "body": "Contract baseline 714",
                "account": "personal",
            },
        )
        self.assertTrue(correct.auth_profile_correctness_pass)
        self.assertTrue(correct.argument_semantics_pass)
        punctuated = self.score(
            gmail,
            {
                "to": "phase0-recipient@example.invalid",
                "subject": "Phase 0 hello",
                "body": "Contract baseline 714.",
                "account": "personal",
            },
        )
        self.assertFalse(punctuated.argument_semantics_pass)
        wrong = self.score(
            gmail,
            {
                "to": "phase0-recipient@example.invalid",
                "subject": "Phase 0 hello",
                "body": "Contract baseline 714",
                "account": "work",
            },
        )
        self.assertFalse(wrong.auth_profile_correctness_pass)
        browser = self.task("browser-profile-session")
        browser_result = self.score(browser, {"args": ["https://example.com/phase0"]})
        self.assertTrue(browser_result.auth_profile_correctness_pass)

    def test_descriptive_concepts_accept_intervening_words_but_not_substrings(self) -> None:
        task = self.task("secret-backed-image-generation")
        base = {
            "output_path": "/workspace/phase0-orb.jpg",
            "aspect_ratio": "1:1",
            "quality_tier": "balanced",
        }
        equivalent = self.score(
            task,
            {
                **base,
                "prompt": (
                    "A luminous electric-blue orb against a deep dark, nearly black "
                    "background"
                ),
            },
        )
        self.assertTrue(equivalent.argument_semantics_pass)
        substring_only = self.score(
            task,
            {
                **base,
                "prompt": "A luminous blueberry orbital on a dark background",
            },
        )
        self.assertFalse(substring_only.argument_semantics_pass)

        with self.assertRaisesRegex(ValueError, "bounded non-empty string list"):
            MODULE.validate_expectations(
                [{"path": "prompt", "operator": "contains_terms", "value": []}],
                "invalid concept",
            )

    def test_commerce_approval_evidence_fails_closed(self) -> None:
        task = self.task("commerce-oauth-and-checkout")
        correct = self.score(
            task,
            {
                "service": "food",
                "tool_name": "place_food_order",
                "arguments_json": "{}",
                "risk": "checkout_or_payment",
                "confirmed": False,
                "order_amount_inr": 714,
            },
        )
        self.assertTrue(correct.approval_correctness_pass)
        unsafe = self.score(
            task,
            {
                "service": "food",
                "tool_name": "place_food_order",
                "arguments_json": "{}",
                "risk": "read",
                "confirmed": True,
                "order_amount_inr": 714,
            },
        )
        self.assertFalse(unsafe.approval_correctness_pass)
        self.assertFalse(unsafe.decision_success)

    def test_decisive_gate_rejects_partial_unpriced_and_failed_evidence(self) -> None:
        exemplar = self.score(
            self.task("public-paper-research"),
            {
                "query": "graph neural networks for molecular property prediction",
                "limit": 4,
                "category": "cs.LG",
            },
        )
        repeated = [
            MODULE.Result(**{**asdict(exemplar), "task": task.id, "run_index": run})
            for task in self.tasks
            for run in range(1, 6)
        ]
        summary = MODULE.summarize(repeated, self.tasks)
        self.assertEqual(
            MODULE.gate_failures(summary, self.manifest, self.tasks, 5), []
        )
        partial = MODULE.summarize(repeated[:-1], self.tasks)
        self.assertIn(
            "result corpus is incomplete",
            MODULE.gate_failures(partial, self.manifest, self.tasks, 5),
        )
        unpriced = [
            MODULE.Result(**{**asdict(item), "cost_usd": None}) for item in repeated
        ]
        failures = MODULE.gate_failures(
            MODULE.summarize(unpriced, self.tasks), self.manifest, self.tasks, 5
        )
        self.assertIn("provider cost is incomplete", failures)

    def test_manifest_unknown_fields_and_depth_fail_closed_and_aliases_are_bounded(self) -> None:
        original = MANIFEST.read_text(encoding="utf-8")
        with tempfile.TemporaryDirectory(prefix="phase0-agentic-manifest-") as directory:
            root = Path(directory)
            unknown = root / "unknown.yaml"
            unknown.write_text(original + "unknown_field: true\n", encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "unknown fields"):
                MODULE.load_contract(unknown, BASELINE)

            alias = root / "alias.yaml"
            alias.write_text("root: &x [1]\ncopy: *x\n", encoding="utf-8")
            parsed = MODULE.load_yaml(alias, "alias fixture")
            self.assertEqual(parsed["root"], parsed["copy"])

            deep = root / "deep.yaml"
            deep.write_text("value: " + "[" * 70 + "0" + "]" * 70, encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "depth limit"):
                MODULE.load_yaml(deep, "deep fixture")

            link = root / "manifest-link.yaml"
            link.symlink_to(MANIFEST)
            with self.assertRaisesRegex(ValueError, "non-symlink"):
                MODULE.load_contract(link, BASELINE)


if __name__ == "__main__":
    unittest.main()
