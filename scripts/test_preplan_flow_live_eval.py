from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import sys
from tempfile import TemporaryDirectory
import textwrap
import unittest


SCRIPT = Path(__file__).with_name("eval-preplan-flow-live.py")
SPEC = importlib.util.spec_from_file_location("preplan_flow_live_eval", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
EVAL = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = EVAL
SPEC.loader.exec_module(EVAL)
REPO_ROOT = Path(__file__).resolve().parents[1]


class PreplanFlowLiveEvalTests(unittest.TestCase):
    @staticmethod
    def parse_routing_fixture(mapping: str, *, split: bool = False):
        tables = """    profiles:
      primary:
        provider: openai
        model: test-primary
        fallback_profile: backup
      backup:
        provider: anthropic
        model: test-backup
      alternate:
        provider: openai
        model: test-alternate
# A YAML comment does not end the enclosing router mapping.
    operation_mapping:
""" + textwrap.indent(textwrap.dedent(mapping).strip(), "      ") + "\n"
        with TemporaryDirectory() as directory:
            path = Path(directory) / "magician-config.yaml"
            path.write_text("llm:\n  router:\n" + ("" if split else tables), encoding="utf-8")
            if split:
                path.with_name("llm-router.yaml").write_text(tables, encoding="utf-8")
            return EVAL.parse_routing_config(path)

    def test_routing_parser_accepts_scalar_and_structured_defaults_inline_and_split(self) -> None:
        mapping = """
            query_analysis: primary
            task_decomposition:
              description: "Break a request into tasks."
              default: 'primary' # the selected default
              when_has_images: alternate
              when_cloud: alternate
              group: Planning
              engine: pinned
            "entity_mapping":
              default: backup
            unrelated_operation:
              default: not-a-preplan-profile
        """
        for split in (False, True):
            with self.subTest(split=split):
                config = self.parse_routing_fixture(mapping, split=split)
                self.assertEqual(config.operation_mapping, {
                    "query_analysis": "primary",
                    "task_decomposition": "primary",
                    "entity_mapping": "backup",
                })
                self.assertEqual(
                    EVAL.allowed_profile_chain(config, "task_decomposition"),
                    ["primary", "backup"],
                )
                self.assertEqual(config.profiles["primary"].provider, "openai")
                self.assertEqual(config.profiles["backup"].model, "test-backup")

    def test_routing_parser_rejects_a_structured_selector_without_a_default(self) -> None:
        with self.assertRaisesRegex(EVAL.EvalFailure, "missing default profiles.*task_decomposition"):
            self.parse_routing_fixture("""
                query_analysis: primary
                task_decomposition:
                  description: "Metadata is not a routing binding."
                unrelated_operation:
                  default: backup
            """)

    def test_routing_parser_rejects_a_structured_default_with_no_profile(self) -> None:
        with self.assertRaisesRegex(EVAL.EvalFailure, "mapped profiles missing.*unknown-profile"):
            self.parse_routing_fixture("""
                query_analysis:
                  default: unknown-profile
            """)

    def test_ad_hoc_prompt_builds_one_safe_auto_approved_plan_case(self) -> None:
        case = EVAL.ad_hoc_prompt_case("  Compare two deployment approaches.  ")

        self.assertEqual(case["id"], "ad_hoc_prompt")
        self.assertEqual(case["description"], "Compare two deployment approaches.")
        self.assertEqual(case["approval_sequence"], ["approve"])
        self.assertEqual(case["expected_min_clarifications"], 0)
        self.assertEqual(case["expected_min_replans"], 0)
        self.assertTrue(case["ad_hoc_prompt"])

    def test_ad_hoc_prompt_rejects_empty_oversized_and_ambiguous_sources(self) -> None:
        with self.assertRaisesRegex(EVAL.EvalFailure, "non-empty planning request"):
            EVAL.ad_hoc_prompt_case("   ")
        with self.assertRaisesRegex(EVAL.EvalFailure, "safety limit"):
            EVAL.ad_hoc_prompt_case("x" * (EVAL.MAX_AD_HOC_PROMPT_CHARS + 1))
        with self.assertRaisesRegex(EVAL.EvalFailure, "alternative case sources"):
            EVAL.resolve_cases(Path("custom.json"), "question")

    def test_unattended_ad_hoc_prompt_can_answer_unpredicted_choices(self) -> None:
        case = EVAL.ad_hoc_prompt_case("Plan a reversible rollout")
        response = EVAL.answer_for_question(
            "fixtures",
            case,
            {
                "id": "choice-1",
                "question_text": "Which rollout style?",
                "options": [
                    {"id": "canary", "label": "Canary"},
                    {"id": "blue_green", "label": "Blue/green"},
                ],
            },
        )

        self.assertEqual(response.input_type, "choice")
        self.assertEqual(response.value["selected_id"], "canary")

    def test_transport_failure_names_the_exact_method_and_path(self) -> None:
        class FailingOpener:
            def open(self, request, timeout):
                raise TimeoutError("deadline")

        client = EVAL.Client("http://127.0.0.1:3002", "p", "w", 1.0)
        client.opener = FailingOpener()
        with self.assertRaisesRegex(
            EVAL.EvalFailure,
            r"during GET /api/magician/v3/tasks/task-1/plan",
        ):
            client.get_json("/api/magician/v3/tasks/task-1/plan")

    def test_repo_config_resolves_every_preplan_mapping_to_a_real_profile(self) -> None:
        config = EVAL.parse_routing_config(REPO_ROOT / "magician-config.yaml")
        self.assertIn("query_analysis", config.operation_mapping)
        self.assertIn("task_decomposition", config.operation_mapping)
        for operation, profile in config.operation_mapping.items():
            with self.subTest(operation=operation):
                definition = config.profiles[profile]
                self.assertTrue(definition.provider)
                self.assertTrue(definition.model)

    def test_eval_annotations_name_the_exact_report_leaf_directories(self) -> None:
        makefile = (REPO_ROOT / "Makefile").read_text(encoding="utf-8")
        self.assertIn(
            "## eval: kind=harness report=evals/preplan-flow/deterministic/latest\n"
            "## desc: Provider-free routing, fixture, projection-identity, lifecycle, and report regressions\n"
            "test-preplan-flow-eval-harness:",
            makefile,
        )
        self.assertIn(
            "## eval: kind=live requires=magician,provider_keys report=evals/preplan-flow/latest\n"
            "## desc: Real mapped-profile pre-plan, terminal-equivalent HITL, Plan-panel, Attention, replan, and telemetry gate\n"
            "test-preplan-flow-live-eval:",
            makefile,
        )
        self.assertIn(
            "PREPLAN_FLOW_EVAL_REPORT_DIR ?= $(COVERAGE_BASE_DIR)/evals/preplan-flow/deterministic/latest",
            makefile,
        )
        self.assertIn(
            "PREPLAN_LIVE_OUTPUT_DIR ?= $(COVERAGE_BASE_DIR)/evals/preplan-flow/latest",
            makefile,
        )

    def test_attention_match_requires_correlation_and_same_task(self) -> None:
        payload = {
            "requests": [
                {
                    "id": "runtime:hitl:q-1",
                    "task_id": "task-1",
                    "metadata": {
                        "correlation_id": "q-1",
                        "source": "clarification",
                    },
                }
            ]
        }
        self.assertIsNotNone(EVAL.find_attention(payload, "q-1", "task-1"))
        self.assertIsNone(EVAL.find_attention(payload, "q-1", "task-2"))
        self.assertIsNone(EVAL.find_attention(payload, "q-2", "task-1"))

    def test_attention_match_accepts_the_real_v3_pause_and_open_target_shape(self) -> None:
        payload = {
            "requests": [
                {
                    "id": "v3:attention:attention:clarification:task-1:exec-1:q-1",
                    "task_id": "task-1",
                    "summary": "Which launch date?",
                    "metadata": {
                        "pause_state_id": "q-1",
                        "execution_id": "exec-1",
                        "source": "clarification",
                        "hitl_request": {
                            "id": "q-1",
                            "prompt": "Which launch date?",
                            "identifiers": {"correlation_id": "q-1"},
                            "scope": {"task_id": "task-1", "execution_id": "exec-1"},
                        },
                    },
                }
            ]
        }
        item = EVAL.find_attention(payload, "q-1", "task-1")
        self.assertIsNotNone(item)
        correlation, execution, prompt = EVAL.attention_identity(item)
        self.assertEqual(correlation, "q-1")
        self.assertEqual(execution, "exec-1")
        self.assertEqual(prompt, "Which launch date?")

    def test_plan_panel_probe_reads_the_exact_plan_inspector_envelope(self) -> None:
        class FakeClient:
            def get_json(self, path, query=None):
                self.assert_path = path
                return {
                    "plan": {
                        "pending_questions": [
                            {"id": "q-1", "question_text": "Which launch date?"}
                        ]
                    }
                }, 4.0

        client = FakeClient()
        questions, latency = EVAL.plan_panel_questions(client, "task-1")

        self.assertEqual(client.assert_path, "/api/magician/v3/tasks/task-1/plan")
        self.assertEqual(questions[0]["id"], "q-1")
        self.assertEqual(latency, 4.0)

    def test_plan_panel_probe_treats_omitted_empty_questions_as_cleared(self) -> None:
        class FakeClient:
            def get_json(self, path, query=None):
                return {"plan": {"status": "planning"}}, 3.0

        questions, latency = EVAL.plan_panel_questions(FakeClient(), "task-1")

        self.assertEqual(questions, [])
        self.assertEqual(latency, 3.0)

    def test_resolved_event_pairing_rejects_wrong_correlation(self) -> None:
        event = {
            "event_type": "HitlResolved",
            "data": {"correlation_id": "q-1", "source": "clarification"},
        }
        self.assertTrue(EVAL.event_matches(event, "q-1", "hitl.resolved"))
        self.assertFalse(EVAL.event_matches(event, "q-2", "hitl.resolved"))

    def test_plan_approval_prompt_gate_allows_title_but_requires_same_intent(self) -> None:
        self.assertTrue(
            EVAL.approval_prompts_are_equivalent(
                "This task plan is ready for review.",
                "Plan ready for review: Publish release note",
            )
        )
        self.assertFalse(
            EVAL.approval_prompts_are_equivalent(
                "This task plan is ready for review.",
                "Task needs attention",
            )
        )

    def test_fixture_answer_uses_question_semantics_then_safe_default(self) -> None:
        case = {
            "id": "case",
            "answer_rules": [
                {"contains_any": ["budget"], "answer": "USD 25,000"},
            ],
            "default_answer": "State the assumption.",
        }
        self.assertEqual(
            EVAL.fixture_answer(case, {"question_text": "What is the budget?"}),
            "USD 25,000",
        )
        self.assertEqual(
            EVAL.fixture_answer(case, {"question_text": "Who owns it?"}),
            "State the assumption.",
        )

    @staticmethod
    def scripted_reader(*answers: str):
        remaining = iter(answers)

        def read(_prompt: str) -> str:
            return next(remaining)

        return read

    def test_terminal_choice_renders_descriptions_and_posts_the_option_id(self) -> None:
        output: list[str] = []
        response = EVAL.terminal_hitl_response(
            {
                "input_type": "choice",
                "prompt": "Which environment?",
                "input_schema": {
                    "options": [
                        {"id": "staging", "label": "Staging"},
                        {
                            "id": "production",
                            "label": "Production",
                            "description": "Uses the customer-facing service",
                        },
                    ]
                },
            },
            read_line=self.scripted_reader("2"),
            emit=output.append,
        )

        self.assertEqual(response.input_type, "choice")
        self.assertEqual(
            response.value,
            {"type": "choice", "selected_id": "production"},
        )
        self.assertTrue(any("customer-facing" in line for line in output))

    def test_fixture_choice_maps_a_label_to_its_canonical_value(self) -> None:
        response = EVAL.answer_for_question(
            "fixtures",
            {"id": "choice-case", "default_answer": "Production"},
            {
                "question_text": "Which environment?",
                "options": [
                    {"value": "staging", "label": "Staging"},
                    {"value": "production", "label": "Production"},
                ],
            },
        )
        self.assertEqual(response.input_type, "choice")
        self.assertEqual(
            response.value,
            {"type": "choice", "selected_id": "production"},
        )

    def test_terminal_choice_supports_required_input_and_freeform_other(self) -> None:
        required = EVAL.terminal_hitl_response(
            {
                "input_type": "choice",
                "prompt": "How should it proceed?",
                "input_schema": {
                    "options": [
                        {
                            "id": "custom",
                            "label": "Custom",
                            "requires_input": True,
                        }
                    ]
                },
            },
            read_line=self.scripted_reader("1", "Use the canary cluster"),
            emit=lambda _line: None,
        )
        self.assertEqual(
            required.value,
            {
                "type": "choice",
                "selected_id": "custom",
                "other_value": "Use the canary cluster",
            },
        )

        other = EVAL.terminal_hitl_response(
            {
                "input_type": "choice",
                "prompt": "Choose a region",
                "input_schema": {
                    "options": [{"id": "us", "label": "United States"}],
                    "allow_other": True,
                },
            },
            read_line=self.scripted_reader("Singapore"),
            emit=lambda _line: None,
        )
        self.assertEqual(
            other.value,
            {"type": "choice", "selected_id": "other", "other_value": "Singapore"},
        )

    def test_terminal_multi_choice_validates_bounds_and_deduplicates(self) -> None:
        output: list[str] = []
        response = EVAL.terminal_hitl_response(
            {
                "input_type": "multi_choice",
                "prompt": "Select two regions",
                "input_schema": {
                    "options": [
                        {"id": "us", "label": "United States"},
                        {"id": "in", "label": "India"},
                        {"id": "sg", "label": "Singapore"},
                    ],
                    "min_selections": 2,
                    "max_selections": 2,
                },
            },
            read_line=self.scripted_reader("1", "1, Singapore, 1"),
            emit=output.append,
        )

        self.assertEqual(
            response.value,
            {"type": "multi_choice", "selected_ids": ["us", "sg"]},
        )
        self.assertTrue(any("Select between" in line for line in output))

    def test_terminal_text_guidance_and_password_preserve_their_wire_types(self) -> None:
        output: list[str] = []
        text = EVAL.terminal_hitl_response(
            {
                "input_type": "text",
                "prompt": "Explain",
                "input_schema": {"multiline": True, "max_length": 8},
            },
            read_line=self.scripted_reader("too long!", ".done", "ok", ".done"),
            emit=output.append,
        )
        guidance_output: list[str] = []
        guidance = EVAL.terminal_hitl_response(
            {
                "input_type": "guidance",
                "prompt": "How should I continue?",
                "input_schema": {
                    "context": "The live source timed out",
                    "suggestions": ["Use the cached source", "Try a mirror"],
                },
            },
            read_line=self.scripted_reader("Try the cached source", "then verify", ".done"),
            emit=guidance_output.append,
        )
        password = EVAL.terminal_hitl_response(
            {
                "input_type": "password",
                "prompt": "Authentication required",
                "input_schema": {"placeholder": "API token"},
            },
            read_secret=self.scripted_reader("super-secret"),
            emit=output.append,
        )

        self.assertEqual(text.value, {"type": "text", "value": "ok"})
        self.assertEqual(
            guidance.value,
            {"type": "guidance", "advice": "Try the cached source\nthen verify"},
        )
        self.assertEqual(password.value, {"type": "password", "value": "super-secret"})
        self.assertNotIn("super-secret", "\n".join(output))
        self.assertTrue(any("live source timed out" in line for line in guidance_output))
        self.assertTrue(any("Try a mirror" in line for line in guidance_output))

    def test_terminal_confirmation_external_action_and_files_are_typed(self) -> None:
        confirmation_output: list[str] = []
        confirmation = EVAL.terminal_hitl_response(
            {
                "input_type": "confirmation",
                "prompt": "Delete the snapshot?",
                "input_schema": {
                    "confirm_label": "Delete",
                    "deny_label": "Keep",
                    "destructive": True,
                },
            },
            read_line=self.scripted_reader("Keep"),
            emit=confirmation_output.append,
        )
        external = EVAL.terminal_hitl_response(
            {
                "input_type": "external_action",
                "prompt": "Verify the account",
                "input_schema": {
                    "instructions": "Open the verification email.",
                    "done_label": "Verification complete",
                },
            },
            read_line=self.scripted_reader("done", "Email was verified"),
            emit=lambda _line: None,
        )
        files = EVAL.terminal_hitl_response(
            {
                "input_type": "file_path",
                "prompt": "Choose evidence",
                "input_schema": {"multiple": True, "filter": "*.pdf"},
            },
            read_line=self.scripted_reader("/tmp/a.pdf", "/tmp/b.pdf", ".done"),
            emit=lambda _line: None,
        )

        self.assertEqual(
            confirmation.value,
            {"type": "confirmation", "confirmed": False},
        )
        self.assertTrue(any("destructive" in line for line in confirmation_output))
        self.assertEqual(
            external.value,
            {"type": "external_action_completed", "guidance": "Email was verified"},
        )
        self.assertEqual(
            files.value,
            {"type": "file_path", "paths": ["/tmp/a.pdf", "/tmp/b.pdf"]},
        )

    def test_terminal_specialized_authorizations_show_the_grant_facts(self) -> None:
        tool_output: list[str] = []
        tool = EVAL.terminal_hitl_response(
            {
                "input_type": "tool_authorization",
                "prompt": "Authorize the tool",
                "input_schema": {
                    "tool_name": "browser.navigate",
                    "params_summary": "url=https://example.com",
                },
            },
            read_line=self.scripted_reader("2"),
            emit=tool_output.append,
        )
        sandbox_output: list[str] = []
        sandbox = EVAL.terminal_hitl_response(
            {
                "input_type": "sandbox_override",
                "prompt": "Authorize command",
                "input_schema": {
                    "command": "touch /tmp/evidence",
                    "violation": "write outside the workspace",
                    "allowed_roots": ["/tmp"],
                },
            },
            read_line=self.scripted_reader("deny"),
            emit=sandbox_output.append,
        )

        self.assertEqual(
            tool.value,
            {"type": "choice", "selected_id": "allow_always"},
        )
        self.assertTrue(any("browser.navigate" in line for line in tool_output))
        self.assertEqual(sandbox.value, {"type": "choice", "selected_id": "deny"})
        self.assertTrue(any("touch /tmp/evidence" in line for line in sandbox_output))
        self.assertTrue(any("/tmp" in line for line in sandbox_output))

    def test_terminal_diff_approval_can_select_exact_files(self) -> None:
        output: list[str] = []
        response = EVAL.terminal_hitl_response(
            {
                "input_type": "diff_approval",
                "prompt": "Review changes",
                "input_schema": {
                    "rationale": "Apply the bounded parser fix",
                    "transaction_id": "txn-1",
                    "files": [
                        {
                            "path": "src/a.rs",
                            "status": "M",
                            "additions": 2,
                            "deletions": 1,
                            "unified_diff": "@@ -1 +1 @@",
                        },
                        {"path": "src/b.rs", "status": "A", "additions": 3},
                    ]
                },
            },
            read_line=self.scripted_reader("selected", "1,src/b.rs"),
            emit=output.append,
        )

        self.assertEqual(response.value, {"type": "choice", "selected_id": "apply"})
        self.assertEqual(response.selected_paths, ("src/a.rs", "src/b.rs"))
        self.assertTrue(any("@@ -1 +1 @@" in line for line in output))
        self.assertTrue(any("bounded parser fix" in line for line in output))
        self.assertTrue(any("txn-1" in line for line in output))

    def test_terminal_abort_and_unknown_type_fail_closed(self) -> None:
        aborted = EVAL.terminal_hitl_response(
            {"input_type": "text", "prompt": "Anything else?"},
            read_line=self.scripted_reader(":abort no longer needed"),
            emit=lambda _line: None,
        )
        self.assertEqual(
            aborted.value,
            {"type": "aborted", "reason": "no longer needed"},
        )
        with self.assertRaisesRegex(EVAL.EvalFailure, "unsupported input_type"):
            EVAL.terminal_hitl_response(
                {"input_type": "future_widget", "prompt": "Unknown"},
                read_line=self.scripted_reader("anything"),
                emit=lambda _line: None,
            )
        with self.assertRaisesRegex(EVAL.EvalFailure, "must use confirmation"):
            EVAL.terminal_hitl_response(
                {"source": "approval", "input_type": "text", "prompt": "Approve?"},
                read_line=self.scripted_reader("yes"),
                emit=lambda _line: None,
            )

    def test_respond_hitl_preserves_typed_value_and_selected_paths(self) -> None:
        class FakeClient:
            def __init__(self) -> None:
                self.path = ""
                self.body = None

            def post_json(self, path, body, expected):
                self.path = path
                self.body = body
                return {"accepted": True}, 4.5, 200

        client = FakeClient()
        latency = EVAL.respond_hitl(
            client,
            "pause-1",
            "agentic",
            "task-1",
            "exec-1",
            {"type": "choice", "selected_id": "apply"},
            "terminal",
            "diff_approval",
            ("src/a.rs",),
        )

        self.assertEqual(latency, 4.5)
        self.assertEqual(client.body["input_type"], "diff_approval")
        self.assertEqual(client.body["selected_paths"], ["src/a.rs"])
        self.assertEqual(client.body["channel"], "terminal_live_eval")

    def test_profile_gate_accepts_configured_fallback_and_rejects_drift(self) -> None:
        config = EVAL.parse_routing_config(REPO_ROOT / "magician-config.yaml")
        operation = "query_analysis"
        profile = config.operation_mapping[operation]
        definition = config.profiles[profile]
        good = [{
            "operation": operation,
            "profile": profile,
            "provider": definition.provider,
            "model": definition.model,
            "success": True,
        }]
        gates = EVAL.llm_routing_gates("case", good, config, 1.0)
        self.assertTrue(next(g for g in gates if g.name == "llm.current_profile_mapping_honored").passed)

        bad = [{**good[0], "profile": "unmapped-profile"}]
        gates = EVAL.llm_routing_gates("case", bad, config, 1.0)
        self.assertFalse(next(g for g in gates if g.name == "llm.current_profile_mapping_honored").passed)

    def test_report_preserves_projection_and_resolution_evidence(self) -> None:
        config = EVAL.parse_routing_config(REPO_ROOT / "magician-config.yaml")
        observation = EVAL.HitlObservation(
            correlation_id="q-1",
            source="clarification",
            prompt="Which date?",
            task_id="task-1",
            execution_id="planexec-1",
            plan_panel_visible=True,
            attention_visible=True,
            plan_panel_latency_ms=2.0,
            attention_latency_ms=3.0,
            resolved_event_visible=True,
            plan_panel_cleared=True,
            attention_cleared=True,
        )
        result = EVAL.CaseResult(
            case_id="case",
            task_id="task-1",
            clarification_count=1,
            approval_count=1,
            final_status="approved",
            hitl=[observation],
        )
        gates = [EVAL.Gate("projection", True, "visible and cleared", "case")]
        with TemporaryDirectory() as temporary_directory:
            output = Path(temporary_directory)
            payload = EVAL.write_report(output, "self-test", config, [result], gates)
            self.assertTrue(payload["passed"])
            self.assertEqual(payload["summary"]["plan_panel_hitl"], 1)
            self.assertEqual(payload["summary"]["attention_hitl"], 1)
            self.assertEqual(payload["summary"]["resolved_hitl"], 1)
            raw = json.loads((output / "report.json").read_text(encoding="utf-8"))
            self.assertTrue(raw["cases"][0]["hitl"][0]["attention_cleared"])
            self.assertFalse(raw["runtime_metrics_available"])
            self.assertEqual(raw["measurement_kind"], "synthetic_contract")
            self.assertIsNone(raw["summary"]["duration_ms"])
            self.assertIsNone(raw["summary"]["api_latency_ms"])
            self.assertIsNone(raw["summary"]["llm_calls"])
            self.assertIsNone(raw["summary"]["cost_usd"])
            html = (output / "report.html").read_text(encoding="utf-8")
            self.assertIn("Pre-plan deterministic harness", html)
            self.assertIn("Provider-free contract harness", html)
            self.assertIn("intentionally unavailable—not zero", html)
            self.assertNotIn("$0.000000", html)
            self.assertIn("Plan output graphs", html)
            self.assertIn("Current pre-plan routing", html)

    def test_live_report_preserves_measured_runtime_telemetry(self) -> None:
        config = EVAL.parse_routing_config(REPO_ROOT / "magician-config.yaml")
        result = EVAL.CaseResult(
            case_id="case-live",
            input_prompt="Compare two deployment approaches.",
            task_id="task-live",
            final_status="approved",
            duration_ms=1234.0,
            llm_calls=[{
                "input_tokens": 120,
                "output_tokens": 30,
                "reasoning_tokens": 5,
                "cost_usd": 0.012345,
            }],
            plan_graph={
                "steps": [
                    {"id": "a", "task": "Collect requirements", "confidence": 0.9},
                    {"id": "b", "task": "Produce answer", "confidence": 0.8},
                ],
                "edges": [{"from": "a", "to": "b", "reason": "requirements ready"}],
                "unresolved_inputs": [],
                "confidence": 0.85,
                "provenance": {"strategy": "live-test"},
            },
        )
        gates = [EVAL.Gate("api.observed", True, "HTTP 200", "case-live", 4.5)]
        with TemporaryDirectory() as temporary_directory:
            output = Path(temporary_directory)
            payload = EVAL.write_report(output, "fixtures", config, [result], gates)

            self.assertTrue(payload["runtime_metrics_available"])
            self.assertEqual(payload["measurement_kind"], "runtime")
            self.assertEqual(payload["summary"]["duration_ms"], 1234.0)
            self.assertEqual(payload["summary"]["api_latency_ms"], 4.5)
            self.assertEqual(payload["summary"]["llm_calls"], 1)
            self.assertEqual(payload["summary"]["cost_usd"], 0.012345)
            html = (output / "report.html").read_text(encoding="utf-8")
            self.assertIn("Pre-plan live evaluation", html)
            self.assertIn("Measured runtime evaluation", html)
            self.assertIn("1.2s", html)
            self.assertIn("4.5 ms", html)
            self.assertIn("$0.012345", html)
            self.assertIn("Captured Plan API output", html)
            self.assertIn("Compare two deployment approaches.", html)
            self.assertIn("Collect requirements", html)
            self.assertIn("requirements ready", html)
            self.assertIn('class="plan-graph-svg"', html)

    def test_self_test_fixture_does_not_claim_runtime_observations(self) -> None:
        results, gates, config = EVAL.synthetic_self_test(
            REPO_ROOT / "magician-config.yaml"
        )
        payload = EVAL.report_payload("self-test", config, results, gates)

        self.assertEqual(results[0].final_status, "synthetic_contract")
        self.assertIsNone(results[0].duration_ms)
        self.assertTrue(all(gate.latency_ms is None for gate in gates))
        self.assertFalse(any(gate.name == "llm.task_execution_linkage" for gate in gates))
        self.assertTrue(any(gate.name == "self_test.routing_fixture_present" for gate in gates))
        self.assertEqual(len(results[0].plan_graph["steps"]), 4)
        self.assertIsNone(payload["summary"]["llm_calls"])
        self.assertIsNone(payload["summary"]["input_tokens"])
        self.assertIsNone(payload["summary"]["cost_usd"])

    def test_cleanup_cancels_active_plan_before_removing_files(self) -> None:
        class FakeClient:
            def __init__(self) -> None:
                self.calls: list[tuple[str, str, object, object]] = []

            def request(self, method, path, body=None, query=None):
                self.calls.append((method, path, body, query))
                if len(self.calls) == 1:
                    return 400, {"error": "task_active:planning"}, 1.0
                if len(self.calls) == 2:
                    return 200, {"status": "cancelled"}, 2.0
                return 200, {"files_removed": True}, 3.0

        client = FakeClient()
        gates = EVAL.cleanup_task(client, "case", "task-active")

        self.assertEqual(
            [call[0] for call in client.calls],
            ["DELETE", "PUT", "DELETE"],
        )
        self.assertEqual(client.calls[1][2], {"status": "cancelled"})
        self.assertTrue(all(gate.passed for gate in gates))

    def test_tool_catalog_projection_loads_the_scoped_pack_surface_once(self) -> None:
        source = (
            REPO_ROOT / "magician-bin/src/adapters/local_tool_services.rs"
        ).read_text(encoding="utf-8")
        start = source.index("    async fn all_tools(")
        end = source.index("    async fn category_tool_counts(", start)
        body = source[start:end]

        self.assertEqual(body.count("visible_enabled_tool_surface(context)"), 1)
        self.assertNotIn("tool_to_info(&tool, context)", body)
        self.assertIn("tool_to_info(&tool, guide)", body)

    def test_progress_projection_reuses_the_attention_api_feed_store(self) -> None:
        progress = (
            REPO_ROOT / "magician/src/magician_v2/artifact_v2/progress.rs"
        ).read_text(encoding="utf-8")
        production_progress = progress.split(
            '#[cfg(any(test, feature = "test-fixtures"))]', 1
        )[0]
        service = (
            REPO_ROOT / "magician/src/magician_v2/artifact_v2/service.rs"
        ).read_text(encoding="utf-8")
        host = (REPO_ROOT / "magician-bin/src/main.rs").read_text(
            encoding="utf-8"
        )

        self.assertIn("attention_store: FeedStore", production_progress)
        self.assertNotIn(
            "FeedStore::open_workspace(workspace.clone())", production_progress
        )
        self.assertIn("attention_store: FeedStore", service)
        self.assertIn("feed_store.clone(),", host)
        self.assertIn(
            "published_hitl_is_immediately_visible_through_shared_attention_store",
            progress,
        )


if __name__ == "__main__":
    unittest.main()
