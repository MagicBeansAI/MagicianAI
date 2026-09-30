#!/usr/bin/env python3
"""Provider-free regressions for the Phase 0 LLM observability contracts."""

from __future__ import annotations

import importlib.util
import json
import tempfile
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]


def load_script(name: str, filename: str):
    spec = importlib.util.spec_from_file_location(name, REPO_ROOT / "scripts" / filename)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {filename}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


audit_module = load_script("audit_llm_trace_coverage", "audit-llm-trace-coverage.py")
baseline_module = load_script(
    "eval_llm_observability_phase0", "eval-llm-observability-phase0.py"
)


class CoverageContractTests(unittest.TestCase):
    def test_repository_contract_is_complete(self) -> None:
        result = audit_module.audit(REPO_ROOT)
        self.assertEqual(result["errors"], [], json.dumps(result, indent=2))
        self.assertEqual(result["status"], "passed")
        self.assertGreaterEqual(result["entry_count"], 15)
        self.assertGreaterEqual(result["operation_count"], 60)

    def test_phase1_coverage_ledger_does_not_retain_pre_adapter_gap_claims(self) -> None:
        ledger = json.loads(
            (
                REPO_ROOT
                / "data/magician_v2/llm_observability/coverage-ledger-v1.json"
            ).read_text(encoding="utf-8")
        )
        by_id = {entry["id"]: entry for entry in ledger["entries"]}

        self.assertIn("exact per-model-call receipts", by_id["chat-multi-llm-direct"]["current_capture"])
        self.assertIn("typed parent relation", by_id["logical-chunking"]["current_capture"])
        self.assertIn("non-billing parent summary", by_id["logical-chunking"]["current_capture"])
        self.assertIn("independent supporting child call", by_id["local-prep-nested-call"]["current_capture"])
        stale_claims = " ".join(
            by_id[entry_id]["current_capture"]
            for entry_id in (
                "chat-multi-llm-direct",
                "logical-chunking",
                "local-prep-nested-call",
            )
        )
        self.assertNotIn("incomplete", stale_claims)
        self.assertNotIn("not represented", stale_claims)
        self.assertNotIn("no independent", stale_claims)

        operation_router = (
            REPO_ROOT
            / "magician/src/magician_v2/query_analysis/operation_llm_router.rs"
        ).read_text(encoding="utf-8")
        runner = (
            REPO_ROOT / "magician/src/magician_v2/llm_chunking/runner.rs"
        ).read_text(encoding="utf-8")
        self.assertIn("struct RuntimeLogicalChunkTelemetry", operation_router)
        self.assertIn("logical_chunk_summary", operation_router)
        self.assertIn("trace_receipt: magicllm::LlmTraceReceipt", runner)
        self.assertIn("trace_context: Option<magicllm::LlmTraceContext>", runner)

    def test_browser_realtime_coverage_is_a_real_scoped_usage_bridge(self) -> None:
        ledger = json.loads(
            (
                REPO_ROOT
                / "data/magician_v2/llm_observability/coverage-ledger-v1.json"
            ).read_text(encoding="utf-8")
        )
        browser_row = next(
            entry for entry in ledger["entries"]
            if entry["id"] == "browser-openai-realtime"
        )
        provider = (
            REPO_ROOT / "ui/unified-ui/src/lib/media/voice/providers/openai.ts"
        ).read_text(encoding="utf-8")
        client = (
            REPO_ROOT / "ui/unified-ui/src/lib/media/voice/realtimeVoiceClient.ts"
        ).read_text(encoding="utf-8")
        handler = (
            REPO_ROOT / "magician-api/src/voice_control_handler.rs"
        ).read_text(encoding="utf-8")
        orchestrator = (
            REPO_ROOT / "magician-media/src/media_rails/voice_orchestrator.rs"
        ).read_text(encoding="utf-8")

        self.assertIn("response.created", provider)
        self.assertIn("parseRealtimeUsage", provider)
        self.assertIn("'response.started'", client)
        self.assertIn("usage,", client)
        self.assertIn('"speech.stopped" | "response.started"', handler)
        self.assertIn("fn realtime_usage_from_control(", handler)
        self.assertIn("serde_json::from_value(value).ok()", handler)
        self.assertIn("|| input_tokens.is_some()", orchestrator)
        self.assertRegex(
            orchestrator,
            r"correlation\s*\.\s*chat_turn_id\s*\.\s*clone_from\(\s*"
            r"&state\.current_chat_turn_id\s*\)",
        )
        self.assertIn("durable canonical call", browser_row["current_capture"])
        self.assertNotIn("not guaranteed", browser_row["current_capture"])

    def test_operation_mapping_parser_ignores_nested_profile_fields(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            config = Path(directory) / "config.yaml"
            config.write_text(
                "llm:\n"
                "  router:\n"
                "    operation_mapping:\n"
                "      chat_completion:\n"
                "        default: fast\n"
                "        nested_field: ignored\n"
                "      task_decomposition:\n"
                "        default: deep\n"
                "    next_setting: true\n",
                encoding="utf-8",
            )
            self.assertEqual(
                audit_module.operation_mapping_keys(config),
                {"chat_completion", "task_decomposition"},
            )

    def test_discovery_probe_reports_only_matching_source_paths(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "nested").mkdir()
            (root / "nested/owned.rs").write_text("ConfiguredRouter\n", encoding="utf-8")
            (root / "nested/other.rs").write_text("unrelated\n", encoding="utf-8")
            self.assertEqual(
                audit_module.discovered_paths(root, ["nested"], "*.rs", "ConfiguredRouter"),
                {"nested/owned.rs"},
            )

    def test_aggregate_population_uses_all_scopes_and_zero_row_schema(self) -> None:
        def scope(rows: int, principal_count: int, provider_count: int):
            return {
                "llm_calls": {
                    "rows": rows,
                    "population": {
                        "principal": {"present": True, "count": principal_count},
                        "provider": {"present": True, "count": provider_count},
                    },
                }
            }

        aggregate = baseline_module.aggregate_population(
            [scope(3, 3, 3), scope(2, 1, 2)]
        )
        self.assertEqual(aggregate["principal"]["count"], 4)
        self.assertEqual(aggregate["principal"]["rate"], 0.8)
        self.assertEqual(aggregate["provider"]["rate"], 1.0)
        self.assertFalse(aggregate["model"]["present"])

    def test_scope_discovery_is_stable_and_filterable(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "scopes/zeta/default").mkdir(parents=True)
            (root / "scopes/alpha/work").mkdir(parents=True)
            discovered = baseline_module.discover_scopes(root, None, None)
            self.assertEqual(
                [(principal, workspace) for principal, workspace, _ in discovered],
                [("alpha", "work"), ("zeta", "default")],
            )
            selected = baseline_module.discover_scopes(root, "owner", "private")
            self.assertEqual(selected[0][:2], ("owner", "private"))

    def test_report_html_escapes_scope_and_error_values(self) -> None:
        population = {
            field: {"present": False, "count": 0, "rate": 0.0}
            for field in baseline_module.CALL_POPULATION_FIELDS
        }
        report = {
            "status": "partial",
            "generated_at": "now",
            "scopes": [
                {
                    "principal": "<owner>",
                    "workspace": "default&work",
                    "llm_calls": {"rows": 0, "file_count": 0, "bytes": 0},
                    "llm_dispatch": {"rows": 0, "file_count": 0, "bytes": 0},
                }
            ],
            "aggregate_call_population": population,
            "future_fields_absent": ["llm_call_id"],
            "errors": ["bad <partition>"],
        }
        rendered = baseline_module.render_html(report)
        self.assertIn("&lt;owner&gt;/default&amp;work", rendered)
        self.assertIn("bad &lt;partition&gt;", rendered)
        self.assertNotIn("bad <partition>", rendered)


@unittest.skipUnless(importlib.util.find_spec("duckdb"), "duckdb is not installed")
class HistoricalParquetFixtureTests(unittest.TestCase):
    def test_historical_schema_reports_missing_columns_without_reading_content(self) -> None:
        import duckdb

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "llm_calls" / "dt=2026-07-22"
            root.mkdir(parents=True)
            output = root / "fixture.parquet"
            connection = duckdb.connect()
            connection.execute(
                "COPY (SELECT 1::BIGINT AS timestamp_ms, 'openai'::VARCHAR AS provider, "
                "'chat_completion'::VARCHAR AS operation, 12::BIGINT AS latency_ms) "
                f"TO '{str(output).replace(chr(39), chr(39) * 2)}' (FORMAT PARQUET)"
            )
            result = baseline_module.dataset_report(
                connection,
                "llm_calls_fixture",
                root.parent,
                baseline_module.CALL_POPULATION_FIELDS,
            )
            self.assertIsNone(result["error"])
            self.assertEqual(result["rows"], 1)
            self.assertEqual(result["population"]["provider"]["rate"], 1.0)
            self.assertFalse(result["population"]["model"]["present"])
            self.assertNotIn("prompt", result["population"])


if __name__ == "__main__":
    unittest.main()
