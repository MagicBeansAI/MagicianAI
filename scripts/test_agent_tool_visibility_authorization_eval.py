#!/usr/bin/env python3

import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
MODULE_PATH = ROOT / "scripts/eval-agent-tool-visibility-authorization-live.py"
SPEC = importlib.util.spec_from_file_location("agent_tool_visibility_authorization_eval", MODULE_PATH)
EVAL = importlib.util.module_from_spec(SPEC)
assert SPEC and SPEC.loader
sys.modules[SPEC.name] = EVAL
SPEC.loader.exec_module(EVAL)
HELPERS = EVAL.load_helpers(ROOT)


def native_tool(name, target=None):
    parameters = {"type": "object", "properties": {}, "additionalProperties": False}
    if name == "delegate_to_agent":
        parameters = {
            "type": "object",
            "properties": {
                "delegation_targets": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "target_agent_id": {
                                "type": "string",
                                "enum": [target or "worker"],
                            },
                            "context": {"type": "string"},
                        },
                    },
                }
            },
        }
    return {
        "name": name,
        "description": f"{name} test tool",
        "parameters": parameters,
        "is_control_tool": name in {"yield", "delegate_to_agent"},
    }


def valid_export():
    scenarios = []
    for scenario in EVAL.SCENARIOS:
        if scenario.expected_tool == "delegate_to_agent":
            candidate = [native_tool("delegate_to_agent", scenario.expected_target)]
        elif scenario.expected_tool:
            candidate = [native_tool(scenario.expected_tool)]
        else:
            candidate = [native_tool("search_memory")]
        scenarios.append(
            {
                "name": scenario.name,
                "snapshot_id": f"snapshot-{scenario.name}",
                "baseline_tools": candidate + [native_tool("baseline_extra")],
                "candidate_tools": candidate,
                "dispatch_tool_names": [tool["name"] for tool in candidate],
                "deferred_tool_names": [],
                "delegation_targets": (
                    [scenario.expected_target]
                    if scenario.expected_tool == "delegate_to_agent"
                    else []
                ),
                "handover_targets": [],
            }
        )
    return {
        "schema_version": 1,
        "generated_by": "magician::effective_tool_policy_snapshot",
        "scenarios": scenarios,
    }


class AuthorizationEvalHarnessTests(unittest.TestCase):
    def test_accepts_only_complete_production_resolver_catalogs(self):
        catalogs = EVAL.validated_catalogs(valid_export())
        self.assertEqual(set(catalogs), {scenario.name for scenario in EVAL.SCENARIOS})

        wrong_source = valid_export()
        wrong_source["generated_by"] = "python-fixture"
        with self.assertRaisesRegex(ValueError, "production policy snapshot"):
            EVAL.validated_catalogs(wrong_source)

        mismatched = valid_export()
        mismatched["scenarios"][0]["dispatch_tool_names"] = ["forged_tool"]
        with self.assertRaisesRegex(ValueError, "provider/dispatch mismatch"):
            EVAL.validated_catalogs(mismatched)

    def test_operation_profiles_follow_production_fast_and_brainstorm_mappings(self):
        config = {
            "llm": {
                "router": {
                    "operation_mapping": {
                        "chat_completion": {"default": "chat-adaptive"},
                        "brainstorm_facilitation": "brainstorm-mini",
                    },
                    "adaptive_profiles": {
                        "chat-adaptive": {
                            "fast_profile": "chat-fast",
                            "thinking_profile": "chat-thinking",
                        },
                    },
                    "profiles": {
                        "chat-fast": {"provider": "openai", "model": "gpt-5.6-terra"},
                        "chat-thinking": {"provider": "openai", "model": "gpt-5.6-terra"},
                        "brainstorm-mini": {"provider": "openai", "model": "gpt-5.6-terra"},
                    },
                }
            }
        }
        chat = EVAL.resolve_profile(HELPERS, config, "chat_completion", None)
        brainstorm = EVAL.resolve_profile(HELPERS, config, "brainstorm_facilitation", None)
        self.assertEqual(chat.name, "chat-fast")
        self.assertEqual(brainstorm.name, "brainstorm-mini")
        self.assertEqual(brainstorm.model, "gpt-5.6-terra")

    def test_realtime_profile_and_pricing_follow_the_voice_controller_mapping(self):
        config = {
            "llm": {
                "router": {
                    "realtime_voice": {
                        "profiles": {
                            "voice-mini": {
                                "provider": "openai_realtime",
                                "model": "gpt-realtime-2.1-mini",
                            }
                        },
                        "operation_mapping": {"voice_controller": "voice-mini"},
                    }
                }
            }
        }
        profile = EVAL.resolve_realtime_profile(HELPERS, config)
        self.assertEqual(profile.name, "voice-mini")
        self.assertEqual(profile.model, "gpt-realtime-2.1-mini")
        self.assertEqual(profile.api_key_env, "OPENAI_API_KEY")
        self.assertEqual(
            EVAL.realtime_text_pricing_row(profile.model),
            {"input_per_m": 0.60, "cache_read_per_m": 0.06, "output_per_m": 2.40},
        )
        self.assertEqual(
            EVAL.realtime_websocket_url(profile),
            "wss://api.openai.com/v1/realtime?model=gpt-realtime-2.1-mini",
        )

    def test_realtime_usage_is_normalized_without_double_billing_cached_tokens(self):
        normalized = EVAL.realtime_usage_as_responses_usage(
            {
                "usage": {
                    "input_tokens": 120,
                    "output_tokens": 15,
                    "input_token_details": {
                        "text_tokens": 120,
                        "cached_tokens_details": {"text_tokens": 40, "audio_tokens": 0},
                    },
                    "output_token_details": {"text_tokens": 15},
                }
            }
        )
        self.assertEqual(normalized["usage"]["input_tokens"], 120)
        self.assertEqual(
            normalized["usage"]["input_tokens_details"]["cached_tokens"], 40
        )
        cost = HELPERS.compute_cost(
            EVAL.realtime_text_pricing_row("gpt-realtime-2.1-mini"),
            120,
            40,
            15,
        )
        self.assertAlmostEqual(cost, (80 * 0.60 + 40 * 0.06 + 15 * 2.40) / 1_000_000)

    def test_live_matrix_has_two_stage_task_and_actual_realtime_voice_gates(self):
        by_name = {scenario.name: scenario for scenario in EVAL.SCENARIOS}
        self.assertEqual(
            by_name["task_deferred_browser_discovery"].candidate_expected_tool,
            "tool_search",
        )
        self.assertEqual(
            by_name["task_loaded_browser_family_action"].expected_tool,
            "browser__open",
        )
        self.assertEqual(
            by_name["voice_loaded_browser_family_action"].realtime_catalog_transition_from,
            "voice_deferred_browser_discovery",
        )
        voice = [
            scenario
            for scenario in EVAL.SCENARIOS
            if scenario.surface == "realtime_voice"
        ]
        self.assertGreaterEqual(len(voice), 4)
        self.assertTrue(all(scenario.operation == "voice_controller" for scenario in voice))
        self.assertEqual(
            by_name["ordinary_wildcard_excludes_loom"].expected_target,
            "web-researcher",
        )
        self.assertEqual(
            by_name["explicit_authorized_delegate"].expected_target,
            "image-analyst",
        )

    def test_current_turn_context_scoring_requires_the_marker_and_no_tool_call(self):
        scenario = next(
            item
            for item in EVAL.SCENARIOS
            if item.name == "voice_current_turn_context_precedes_response"
        )
        profile = HELPERS.Profile(
            "voice-mini",
            "openai_realtime",
            "gpt-realtime-2.1-mini",
            "OPENAI_API_KEY",
            30,
            4096,
            None,
            None,
            None,
            None,
        )
        response = {
            "output": [
                {
                    "type": "message",
                    "content": [{"type": "output_text", "text": "It is ORBIT-4729."}],
                }
            ],
            "usage": {"input_tokens": 10, "output_tokens": 5},
        }
        result = EVAL.score(
            HELPERS,
            scenario,
            "candidate",
            profile,
            "snapshot",
            [],
            1,
            200,
            response,
            100,
            None,
            EVAL.realtime_text_pricing_row(profile.model),
            None,
            first_output_ms=40,
            session_update_ms=20,
        )
        self.assertTrue(result.exact_selection_pass)
        self.assertTrue(result.selected_action_allowed)
        self.assertEqual(result.transport, "openai_realtime_websocket")
        self.assertEqual(result.first_output_ms, 40)
        self.assertEqual(result.session_update_ms, 20)

    def test_baseline_gate_fails_closed_for_every_regression_axis(self):
        baseline = {
            "http_success_rate": 1.0,
            "exact_selection_rate": 1.0,
            "catalog_confinement_rate": 0.0,
            "allowed_selection_rate": 1.0,
            "median_total_ms": 100.0,
            "input_tokens": 100,
            "total_cost_usd": 0.01,
            "cache_normalized_total_cost_usd": 0.01,
            "pricing_complete": True,
            "cache_normalized_pricing_complete": True,
        }
        candidate = {
            "http_success_rate": 0.9,
            "exact_selection_rate": 0.9,
            "catalog_confinement_rate": 0.9,
            "allowed_selection_rate": 0.9,
            "median_total_ms": 2000.0,
            "input_tokens": 120,
            "total_cost_usd": 0.02,
            "cache_normalized_total_cost_usd": 0.02,
            "pricing_complete": True,
            "cache_normalized_pricing_complete": True,
        }
        metrics = {
            "baseline": baseline,
            "candidate": candidate,
            "catalogs": {
                "scenarios": [
                    {
                        "scenario": "regressed",
                        "candidate_schema_bytes": 101,
                        "baseline_schema_bytes": 100,
                        "schema_byte_delta": 1,
                    }
                ]
            },
        }
        failures = EVAL.gate_failures(metrics, 0.25, 50, 0.10)
        joined = "\n".join(failures)
        self.assertIn("exact_selection_rate", joined)
        self.assertIn("schema grew", joined)
        self.assertIn("input tokens", joined)
        self.assertIn("cache-normalized cost", joined)
        self.assertIn("median latency", joined)

    def test_cost_gate_is_stable_when_only_baseline_receives_cache_credit(self):
        baseline = {
            "http_success_rate": 1.0,
            "exact_selection_rate": 1.0,
            "catalog_confinement_rate": 0.0,
            "allowed_selection_rate": 1.0,
            "median_total_ms": 100.0,
            "input_tokens": 200,
            "total_cost_usd": 0.001,
            "cache_normalized_total_cost_usd": 0.020,
            "pricing_complete": True,
            "cache_normalized_pricing_complete": True,
        }
        candidate = {
            "http_success_rate": 1.0,
            "exact_selection_rate": 1.0,
            "catalog_confinement_rate": 1.0,
            "allowed_selection_rate": 1.0,
            "median_total_ms": 100.0,
            "input_tokens": 100,
            "total_cost_usd": 0.010,
            "cache_normalized_total_cost_usd": 0.010,
            "pricing_complete": True,
            "cache_normalized_pricing_complete": True,
        }
        metrics = {
            "baseline": baseline,
            "candidate": candidate,
            "catalogs": {
                "scenarios": [
                    {
                        "scenario": "cache-threshold",
                        "candidate_schema_bytes": 50,
                        "baseline_schema_bytes": 100,
                        "schema_byte_delta": -50,
                    }
                ]
            },
        }
        self.assertEqual(EVAL.gate_failures(metrics, 0.25, 50, 0.10), [])

    def test_per_scenario_gate_cannot_be_hidden_by_aggregate_savings(self):
        healthy = {
            "http_success_rate": 1.0,
            "exact_selection_rate": 1.0,
            "catalog_confinement_rate": 1.0,
            "allowed_selection_rate": 1.0,
            "median_total_ms": 100.0,
            "input_tokens": 100,
            "total_cost_usd": 0.01,
            "cache_normalized_total_cost_usd": 0.01,
            "pricing_complete": True,
            "cache_normalized_pricing_complete": True,
        }
        regressed = {
            **healthy,
            "median_total_ms": 1000.0,
            "input_tokens": 101,
            "cache_normalized_total_cost_usd": 0.02,
        }
        metrics = {
            "baseline": healthy,
            "candidate": healthy,
            "catalogs": {"scenarios": []},
            "scenario_comparisons": [
                {
                    "scenario": "masked-regression",
                    "baseline": healthy,
                    "candidate": regressed,
                }
            ],
        }
        failures = "\n".join(EVAL.gate_failures(metrics, 0.25, 50, 0.10))
        self.assertIn("masked-regression candidate input tokens", failures)
        self.assertIn("masked-regression candidate cache-normalized cost", failures)
        self.assertIn("masked-regression candidate median latency", failures)

    def test_thinking_map_payload_uses_structured_output_and_nonthinking_mini(self):
        profile = HELPERS.Profile(
            "brainstorm-mini",
            "openai",
            "gpt-5.6-terra",
            "OPENAI_API_KEY",
            30,
            1200,
            None,
            None,
            "low",
            None,
        )
        scenario = next(item for item in EVAL.SCENARIOS if item.expects_frontier)
        tools = [EVAL.response_tool(native_tool("search_memory"))]
        payload = EVAL.build_payload(profile, scenario, tools, 1200)
        self.assertEqual(payload["reasoning"], {"effort": "none"})
        self.assertEqual(payload["tool_choice"], "auto")
        self.assertTrue(payload["text"]["format"]["strict"])
        self.assertEqual(payload["text"]["format"]["schema"]["required"], ["moves"])

    def test_chat_and_task_dynamic_context_stays_outside_candidate_static_prefix(self):
        profile = HELPERS.Profile(
            "chat-fast",
            "openai",
            "gpt-5.6-terra",
            "OPENAI_API_KEY",
            30,
            1200,
            None,
            None,
            None,
            None,
        )
        for scenario_name in (
            "chat_current_turn_context_quality_parity",
            "task_checkpoint_context_quality_parity",
        ):
            scenario = next(
                item for item in EVAL.SCENARIOS if item.name == scenario_name
            )
            candidate = EVAL.build_payload(
                profile, scenario, [], 1200, variant="candidate", run_index=4
            )
            baseline = EVAL.build_payload(
                profile, scenario, [], 1200, variant="baseline", run_index=4
            )

            self.assertNotIn("checkpoint_generation=4", candidate["instructions"])
            self.assertIn("checkpoint_generation=4", baseline["instructions"])
            self.assertIn(
                "checkpoint_generation=4", candidate["input"][0]["content"][0]["text"]
            )
            self.assertEqual(candidate["input"][-1]["content"][0]["text"], scenario.user_prompt)
            self.assertNotIn("tools", candidate)
            self.assertNotIn("tool_choice", candidate)

    def test_aggregate_runner_rejects_unknown_target_instead_of_skipping_everything(self):
        with tempfile.TemporaryDirectory(prefix="authorization-runner-test-") as report_dir:
            env = os.environ.copy()
            env["LIVE_EVAL_ONLY"] = "misspelled-authorization-eval"
            env["LIVE_EVAL_REPORT_DIR"] = report_dir
            completed = subprocess.run(
                ["bash", str(ROOT / "scripts/run-live-evals-with-report.sh"), "--dry-run"],
                cwd=ROOT,
                env=env,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=False,
            )
        self.assertEqual(completed.returncode, 2)
        self.assertIn("unknown evaluator", completed.stderr)

    def test_targeted_provider_free_runner_records_authorization_status(self):
        with tempfile.TemporaryDirectory(prefix="authorization-runner-test-") as report_dir:
            env = os.environ.copy()
            env["LIVE_EVAL_ONLY"] = "agent-tool-visibility-authorization"
            env["LIVE_EVAL_REPORT_DIR"] = report_dir
            env["LIVE_EVAL_PYTHON"] = sys.executable
            completed = subprocess.run(
                ["bash", str(ROOT / "scripts/run-live-evals-with-report.sh"), "--self-test"],
                cwd=ROOT,
                env=env,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=False,
            )
            manifest = json.loads(
                (Path(report_dir) / "latest.json").read_text(encoding="utf-8")
            )
            dashboard = (Path(report_dir) / "latest.html").read_text(encoding="utf-8")
        self.assertEqual(completed.returncode, 0, completed.stderr)
        authorization = next(
            suite
            for suite in manifest["suites"]
            if suite["name"] == "Tool visibility authorization"
        )
        self.assertEqual(authorization["state"], "passed")
        self.assertIn("Tool visibility authorization", dashboard)

    def test_saved_report_can_be_regated_and_rendered_without_provider_calls(self):
        common = {
            "scenario": "ordinary_incidental_tutor_text",
            "surface": "chat",
            "operation": "chat_completion",
            "profile": "test-profile",
            "model": "gpt-5.6-terra",
            "snapshot_id": "snapshot",
            "run_index": 1,
            "status_code": 200,
            "selected_tool": "search_memory",
            "selected_target": None,
            "exact_selection_pass": True,
            "selected_action_allowed": True,
            "tool_decision_ms": 50,
            "cached_tokens": 0,
            "output_tokens": 5,
            "reasoning_tokens": 0,
            "error": None,
        }
        results = [
            {
                **common,
                "variant": "baseline",
                "catalog_confined": False,
                "schema_bytes": 100,
                "total_ms": 100,
                "input_tokens": 20,
                "cost_usd": 0.000025,
                "cache_normalized_cost_usd": 0.000025,
            },
            {
                **common,
                "variant": "candidate",
                "catalog_confined": True,
                "schema_bytes": 50,
                "total_ms": 90,
                "input_tokens": 10,
                "cost_usd": 0.000015,
                "cache_normalized_cost_usd": 0.000015,
            },
        ]
        catalogs = {
            "scenarios": [
                {
                    "scenario": "ordinary_incidental_tutor_text",
                    "snapshot_id": "snapshot",
                    "baseline_tool_count": 2,
                    "candidate_tool_count": 1,
                    "baseline_schema_bytes": 100,
                    "candidate_schema_bytes": 50,
                    "schema_byte_delta": -50,
                    "schema_reduction_pct": 0.5,
                }
            ],
            "baseline_schema_bytes": 100,
            "candidate_schema_bytes": 50,
            "schema_byte_delta": -50,
            "schema_reduction_pct": 0.5,
        }
        with tempfile.TemporaryDirectory(prefix="authorization-render-test-") as report_dir:
            report_path = Path(report_dir) / "report.json"
            report_path.write_text(
                json.dumps(
                    {
                        "catalog_source": "magician::effective_tool_policy_snapshot",
                        "profiles": {},
                        "runs": 1,
                        "comparison_tolerances": {
                            "latency_ratio": 0.25,
                            "latency_ms": 750,
                            "cost_ratio": 0.10,
                        },
                        "summary": {"catalogs": catalogs},
                        "gate_failures": ["stale"],
                        "results": results,
                    }
                ),
                encoding="utf-8",
            )
            completed = subprocess.run(
                [sys.executable, str(MODULE_PATH), "--render-report", str(report_path)],
                cwd=ROOT,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=False,
            )
            rendered = json.loads(report_path.read_text(encoding="utf-8"))
            html = (Path(report_dir) / "report.html").read_text(encoding="utf-8")
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertEqual(rendered["gate_failures"], [])
        self.assertEqual(len(rendered["summary"]["scenario_comparisons"]), 1)
        self.assertIn("Per-scenario baseline", html)


if __name__ == "__main__":
    unittest.main()
