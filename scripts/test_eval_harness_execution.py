"""Provider-free tests of the agentic harness eval's evidence requirements."""
from copy import deepcopy
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

if __package__:
    from . import eval_harness_execution as evaluation
else:
    import eval_harness_execution as evaluation


SOURCE = Path("/tmp/fixture/input.txt")
DESTINATION = Path("/tmp/fixture/result.json")
MISSING = Path("/tmp/fixture/missing.txt")
MARKER = "hidden-fixture-marker"
WRITTEN = {"marker": MARKER, "line_count": 4}


def event(kind, payload, execution="parent", time_ms=3000):
    return {"task_id": "task", "execution_id": execution, "event_type": kind,
            "payload": {"timestamp_ms": time_ms, **payload}}


def call(action, path, execution="parent", success=True, time_ms=3000):
    return event("tool.succeeded" if success else "tool.failed",
                 {"iteration": 0, "target": f'pack:files(action="{action}",path="{path}")'},
                 execution, time_ms)


def fixture(case="delegate_tools", engine="codex"):
    events = [] if engine == "magician" else [event("execution.progress", {
        "kind": "harness_turn_settled", "engine": engine, "input_tokens": 10, "output_tokens": 2})]
    events += [call("read", SOURCE)] if case != "delegate_tools" else []
    if case in ("local_write", "delegate_tools"):
        events.append(call("write", DESTINATION))
    if case == "tool_recovery":
        events.insert(0, call("read", MISSING, success=False, time_ms=1000))
    return {"case": case, "engine": engine, "task_id": "task", "execution_id": "parent",
            "status": "completed", "answer": f"{MARKER}: 4 lines", "events": events,
            "children": [{"state": {"task_id": "task", "execution_id": "child",
                "parent_execution_id": "parent", "agent_id": evaluation.DEFAULT_DELEGATE,
                "relationship_type": "delegate", "status": "completed",
                "completed_at": "1970-01-01T00:00:02Z"},
                "events": [call("read", SOURCE, execution="child", time_ms=1500)],
                "answer": f"{MARKER}: 4 lines"}]}


def grade(result):
    written = WRITTEN if result["case"] in ("delegate_tools", "local_write") else None
    return evaluation.grade_case(result, MARKER, SOURCE, DESTINATION, MISSING, written)


class EvidenceContracts(unittest.TestCase):
    def test_positive_matrix_uses_the_same_gates_for_every_engine(self):
        for engine in evaluation.ENGINES:
            for case in evaluation.CASES:
                with self.subTest(engine=engine, case=case):
                    self.assertTrue(all(grade(fixture(case, engine)).values()))

    def test_claimed_delegation_without_a_child_fails(self):
        result = fixture()
        result["children"] = []
        self.assertFalse(grade(result)["child_linked"])
        self.assertFalse(grade(result)["parent_continued_after_child"])

    def test_child_must_belong_to_this_parent_task_and_target(self):
        for key, wrong in (("parent_execution_id", "other"), ("task_id", "other"),
                           ("agent_id", "other"), ("relationship_type", "root")):
            with self.subTest(key=key):
                result = fixture()
                result["children"][0]["state"][key] = wrong
                self.assertFalse(grade(result)["child_linked"])

    def test_queued_failed_or_partial_child_is_not_completion(self):
        for status in ("queued", "running", "failed", "cancelled"):
            result = fixture()
            result["children"][0]["state"]["status"] = status
            self.assertFalse(grade(result)["child_completed"])
        result = fixture()
        result["children"][0]["state"]["completion_kind"] = "partial"
        self.assertFalse(grade(result)["child_completed"])

    def test_child_must_read_the_actual_fixture(self):
        result = fixture()
        result["children"][0]["events"] = [call("read", "/tmp/something-else", execution="child")]
        self.assertFalse(grade(result)["child_tool_and_result"])
        result["children"][0]["events"] = [call("read", str(SOURCE) + ".other", execution="child")]
        self.assertFalse(grade(result)["child_tool_and_result"])

    def test_child_trace_cannot_borrow_another_execution_or_task(self):
        for field in ("execution_id", "task_id"):
            result = fixture()
            result["children"][0]["events"][0][field] = "unrelated"
            self.assertFalse(grade(result)["child_tool_and_result"])

    def test_child_must_publish_the_hidden_result(self):
        result = fixture()
        result["children"][0]["answer"] = "Accepted delegation, will work on it"
        self.assertFalse(grade(result)["child_tool_and_result"])

    def test_parent_must_resume_after_child_completion(self):
        result = fixture()
        result["events"][-1]["payload"]["timestamp_ms"] = 1000
        self.assertFalse(grade(result)["parent_continued_after_child"])
        result["children"][0]["state"]["completed_at"] = None
        self.assertFalse(grade(result)["parent_continued_after_child"])

    def test_parent_cannot_do_the_child_read_itself(self):
        result = fixture()
        result["events"].append(call("read", SOURCE))
        self.assertFalse(grade(result)["parent_did_not_read_child_input"])

    def test_file_effect_without_a_governed_write_fails(self):
        result = fixture("local_write")
        result["events"] = result["events"][:-1]
        self.assertFalse(grade(result)["fixture_write"])

    def test_wrong_file_contents_fail_even_with_successful_calls(self):
        result = fixture("local_write")
        self.assertFalse(evaluation.grade_case(result, MARKER, SOURCE, DESTINATION, MISSING,
                                               {"marker": "wrong", "line_count": 4})["fixture_write"])

    def test_unrelated_parent_events_cannot_satisfy_a_fixture(self):
        result = fixture("local_tools")
        result["events"][-1]["execution_id"] = "other-parent"
        self.assertFalse(grade(result)["fixture_read"])

    def test_recovery_requires_the_failed_call_then_the_successful_read(self):
        result = fixture("tool_recovery")
        result["events"][0]["payload"]["timestamp_ms"] = 5000
        self.assertFalse(grade(result)["recovered_after_error"])
        result["events"].pop(0)
        self.assertFalse(grade(result)["failed_call_observed"])

    def test_an_unrelated_error_does_not_count_as_recovery(self):
        result = fixture("tool_recovery")
        result["events"][0] = call("read", "/tmp/unrelated", success=False)
        self.assertFalse(grade(result)["failed_call_observed"])

    def test_mixed_engine_or_missing_usage_fails(self):
        result = fixture("local_tools")
        other = deepcopy(result["events"][0])
        other["payload"]["engine"] = "grok"
        result["events"].append(other)
        self.assertFalse(grade(result)["selected_engine"])
        for usage in (None, -1, True):
            result = fixture("local_tools")
            result["events"][0]["payload"]["input_tokens"] = usage
            self.assertFalse(grade(result)["token_usage"])

    def test_builtin_cannot_silently_run_a_foreign_harness(self):
        result = fixture("local_tools")
        result["engine"] = "magician"
        self.assertFalse(grade(result)["selected_engine"])

    def test_parent_partial_completion_fails(self):
        result = fixture()
        result["completion_kind"] = "partial"
        self.assertFalse(grade(result)["complete_delivery"])

    def test_real_target_spellings_match(self):
        targets = (f'pack:files(action="read",path="{SOURCE}")',
                   f'pack:files__read(path="{SOURCE}")',
                   f'pack:read_file(path="{SOURCE}")', f'pack:grep(path="{SOURCE}")')
        for target in targets:
            row = event("tool.succeeded", {"target": target})
            self.assertEqual(evaluation.file_calls([row], "read", SOURCE), [row])
        wrong = event("tool.succeeded", {"target": f'pack:shell(command="read {SOURCE}")'})
        self.assertFalse(evaluation.file_calls([wrong], "read", SOURCE))

    def test_delegate_prompt_requests_real_delegation_and_hides_answer(self):
        prompt = evaluation.fixture_prompt("delegate_tools", SOURCE, DESTINATION, MISSING,
                                           evaluation.DEFAULT_DELEGATE)
        self.assertIn(evaluation.DEFAULT_DELEGATE, prompt)
        self.assertIn("continue as the parent", prompt)
        self.assertNotIn(MARKER, prompt)
        self.assertNotIn("4", prompt)


class RunnerContracts(unittest.TestCase):
    def args(self, output):
        return SimpleNamespace(self_test=False, api_base_url="http://localhost:1", engines=None,
            cases=None, runs=1, turn_timeout_secs=10, http_timeout_secs=1, output_dir=Path(output),
            execution_delegate_agent=evaluation.DEFAULT_DELEGATE,
            execution_web_research_case=evaluation.DEFAULT_WEB_RESEARCH_CASE)

    def test_outage_records_the_whole_matrix_and_starts_no_tasks(self):
        class Offline(Exception):
            pass
        with tempfile.TemporaryDirectory() as directory:
            client = Mock()
            with patch.object(evaluation, "login", side_effect=Offline("service offline")):
                self.assertEqual(evaluation.main(self.args(directory), lambda *_: client, Offline), 2)
            import json
            report = json.loads((Path(directory) / "report.json").read_text())
            self.assertEqual(len(report["results"]), len(evaluation.ENGINES) * len(evaluation.CASES))
            self.assertFalse(report["summary"]["ok"])
            self.assertEqual(report["summary"]["inconclusive"], len(evaluation.ENGINES) * len(evaluation.CASES))
            client.json.assert_not_called()

    def test_child_collection_uses_scoped_execution_outputs(self):
        client = Mock()
        child = fixture()["children"][0]
        client.json.side_effect = [
            {"executions": [child["state"], {"execution_id": "other", "parent_execution_id": "unrelated"}]},
            {"state": {**child["state"], "primary_execution_output_id": "out"}},
            {"outputs": {"outputs": [{"output_id": "out", "relative_path": "child/result.md"}]}},
        ]
        client.json.side_effect = [(payload, 0) for payload in client.json.side_effect]
        client.request.return_value = (200, f"{MARKER}: 4 lines".encode(), 0)
        with patch.object(evaluation, "events_for", return_value=child["events"]):
            rows = evaluation.collect_children(client, "task", "parent", 1)
        self.assertEqual(len(rows), 1)
        self.assertIn(MARKER, rows[0]["answer"])
        self.assertEqual(client.json.call_args.args[1], "/api/magician/v3/tasks/task/executions/child/outputs")

    def test_missing_install_is_reported_for_every_case(self):
        with tempfile.TemporaryDirectory() as directory:
            client = Mock()
            client.json.return_value = ({"engines": []}, 0)
            with patch.object(evaluation, "login"), patch.object(evaluation, "run_case") as run:
                self.assertEqual(evaluation.main(self.args(directory), lambda *_: client, ConnectionError), 1)
                run.assert_not_called()

    def test_invalid_case_fails_before_login(self):
        with tempfile.TemporaryDirectory() as directory:
            args = self.args(directory)
            args.cases = ["does-not-exist"]
            client_type = Mock()
            with self.assertRaises(ValueError):
                evaluation.main(args, client_type, ConnectionError)
            client_type.assert_not_called()


if __name__ == "__main__":
    unittest.main()


def web_research_report(root_status="completed", failing=(), profile_failing=(), verdict=None):
    """A web-researcher eval report.json as its writer shapes it: one case, and
    the suite's gates each tagged with the case id."""
    # Coverage gates decide full-vs-partial; the rest are assertions.
    coverage = {"answer.minimum_content", "answer.required_terms", "citations.minimum", "citations.reachable"}
    names = ("execution.completed", "execution.web_researcher_lineage", "answer.minimum_content",
             "answer.required_terms", "citations.minimum", "citations.reachable",
             "answer.supported_by_opened_pages", "tools.paired_lifecycle",
             "tools.no_unrecovered_failures", "tools.required_vector_capability",
             "telemetry.linked_llm_calls")
    gates = [{"name": name, "passed": name not in failing, "detail": "", "coverage": name in coverage,
              "case_id": "direct_openai_sarvam_pricing"} for name in names]
    gates += [{"name": name, "passed": name not in profile_failing, "detail": "", "coverage": False,
               "case_id": "direct_openai_sarvam_pricing"} for name in evaluation.WEB_RESEARCH_PROFILE_GATES]
    gates.append({"name": "routing.eval_profile", "passed": True, "detail": "op-eval", "coverage": False, "case_id": "suite"})
    completion_kind = "partial" if verdict in ("partial_pass", "weak_partial") else None
    if verdict is None:
        # The eval's own rule over every gate, profile gates included.
        verdict = "full_pass" if not failing and not profile_failing else "fail"
    return {"gates": gates, "cases": [{"case_id": "direct_openai_sarvam_pricing", "fixture_id": "direct_openai_sarvam_pricing",
                                       "task_id": "task", "root_execution_id": "parent", "root_status": root_status,
                                       "verdict": verdict, "completion_kind": completion_kind, "honesty": "honest",
                                       "citations": ["https://openai.com/api/pricing/", "https://www.sarvam.ai/"]}]}


def web_hands(engine="codex", tool="content_search"):
    events = [] if engine == "magician" else [event("execution.progress", {
        "kind": "harness_turn_settled", "engine": engine, "input_tokens": 10, "output_tokens": 2})]
    events.append(event("tool.succeeded", {"iteration": 0, "target": f'pack:{tool}(query="sarvam pricing")'}))
    return events


WEB_FIXTURE = {"id": "direct_openai_sarvam_pricing", "required_tool_groups": [["content_search"]],
               "min_paired_tool_calls": 1, "max_failed_tool_calls": 0}


class WebResearchContracts(unittest.TestCase):
    """The direct web research case reuses the web-researcher eval's own gates
    and adds the lane's proof of which engine thought the run."""

    def test_the_case_is_registered_with_the_lane(self):
        self.assertIn("direct_web_research", evaluation.CASES)

    def test_a_clean_report_under_a_harness_passes_every_lane_gate(self):
        gates = evaluation.grade_web_research("codex", web_research_report(), web_hands("codex"))
        self.assertTrue(all(gates.values()), gates)

    def test_the_eval_s_own_verdict_is_the_effect(self):
        report = web_research_report(failing=("citations.reachable",))
        gates = evaluation.grade_web_research("codex", report, web_hands("codex"))
        self.assertFalse(gates["effect"])
        self.assertTrue(gates["selected_engine"])

    def test_an_honest_partial_is_an_effect_but_not_complete_delivery(self):
        # The web-researcher eval's own rule: on a terminal the runtime marked
        # partial, coverage gates decide full-vs-partial and do not fail the
        # case. The lane reads the verdict the same way — the research
        # happened, governed, on the right engine — and says separately that
        # the delivery was not complete, as it does for a partial delegation.
        report = web_research_report(failing=("citations.minimum", "citations.reachable"), verdict="partial_pass")
        gates = evaluation.grade_web_research("codex", report, web_hands("codex"))
        self.assertTrue(gates["effect"])
        self.assertFalse(gates["complete_delivery"])
        full = evaluation.grade_web_research("codex", web_research_report(), web_hands("codex"))
        self.assertTrue(full["complete_delivery"])
        # A partial whose open items the eval judged resolvable is its weak form:
        # still research that happened, still not complete delivery.
        weak = web_research_report(failing=("citations.minimum",), verdict="weak_partial")
        weak["cases"][0]["honesty"] = "resolvable"
        gates = evaluation.grade_web_research("codex", weak, web_hands("codex"))
        self.assertTrue(gates["effect"])
        self.assertFalse(gates["complete_delivery"])
        # A partial the fixture requires to be full is a failure, as the eval says.
        gates = evaluation.grade_web_research("codex", report, web_hands("codex"), {**WEB_FIXTURE, "require_full": True})
        self.assertFalse(gates["effect"])

    def test_a_harness_s_tool_gates_are_read_off_the_journal(self):
        # The eval reads tool calls from the analytics fact registry, which a
        # plane call inside a harness turn never writes (lane-1 finding #4), so
        # its three tools.* gates report zero calls under every harness. The
        # journal is the harness's record: the same requirements are read there.
        report = web_research_report(failing=("tools.paired_lifecycle", "tools.required_vector_capability"))
        gates = evaluation.grade_web_research("codex", report, web_hands("codex"), WEB_FIXTURE)
        self.assertTrue(gates["effect"], gates)
        # Claude Code, measured 2026-09-18: raw http fetches, never content_search.
        gates = evaluation.grade_web_research("codex", report, web_hands("codex", tool="http"), WEB_FIXTURE)
        self.assertFalse(gates["effect"])
        # A failed governed call over the fixture's allowance fails too...
        events = web_hands("codex") + [event("tool.failed", {"iteration": 0, "target": "pack:content_read(url=\"x\")"})]
        self.assertFalse(evaluation.grade_web_research("codex", report, events, WEB_FIXTURE)["effect"])
        # ...unless it recovered the way the eval defines it: a later success
        # of the same tool (content_search / content_read), or for a read, an
        # answer the judge found supported by another opened page.
        recovered = events + [event("tool.succeeded", {"iteration": 0, "target": "pack:content_read(url=\"y\")"}, time_ms=4000)]
        self.assertTrue(evaluation.grade_web_research("codex", report, recovered, WEB_FIXTURE)["effect"])
        events[-1]["payload"]["target"] = "pack:http(url=\"x\")"
        self.assertFalse(evaluation.grade_web_research("codex", report, events + [recovered[-1]], WEB_FIXTURE)["effect"],
                         "only the research pack's own tools recover by a later success")
        # Magician's runs do write facts; the eval's own tool gates stand.
        report = web_research_report(failing=("tools.required_vector_capability",))
        self.assertFalse(evaluation.grade_web_research("magician", report, web_hands("magician"), WEB_FIXTURE)["effect"])

    def test_the_eval_s_summary_verdict_gate_is_not_double_counted(self):
        report = web_research_report()
        report["gates"].append({"name": "outcome.verdict", "passed": False, "detail": "verdict=fail", "coverage": False,
                                "case_id": "direct_openai_sarvam_pricing"})
        self.assertTrue(evaluation.grade_web_research("codex", report, web_hands("codex"), WEB_FIXTURE)["effect"])

    def test_a_terminal_that_claimed_full_but_missed_coverage_fails(self):
        report = web_research_report(failing=("citations.minimum",))
        self.assertFalse(evaluation.grade_web_research("codex", report, web_hands("codex"))["effect"])

    def test_a_harness_is_proven_by_its_settles_not_by_router_profiles(self):
        # A harness engine never makes a router `agentic_decision` call, so the
        # web-researcher eval's two profile gates cannot hold under it; the
        # lane's settle proof stands in for them.
        report = web_research_report(profile_failing=tuple(evaluation.WEB_RESEARCH_PROFILE_GATES))
        gates = evaluation.grade_web_research("codex", report, web_hands("codex"))
        self.assertTrue(gates["selected_engine"])
        self.assertTrue(gates["effect"])
        # But a settle from another engine, or none at all, is not proof.
        self.assertFalse(evaluation.grade_web_research("grok", report, web_hands("codex"))["selected_engine"])
        self.assertFalse(evaluation.grade_web_research("codex", report, web_hands("magician"))["selected_engine"])

    def test_magician_is_proven_by_the_router_profile_gates_and_no_settle(self):
        gates = evaluation.grade_web_research("magician", web_research_report(), web_hands("magician"))
        self.assertTrue(gates["selected_engine"])
        report = web_research_report(profile_failing=("telemetry.eval_decision_profile_observed",))
        self.assertFalse(evaluation.grade_web_research("magician", report, web_hands("magician"))["selected_engine"])
        self.assertFalse(evaluation.grade_web_research("magician", web_research_report(), web_hands("codex"))["selected_engine"])

    def test_the_run_must_use_governed_hands(self):
        events = [row for row in web_hands("codex") if row["event_type"] != "tool.succeeded"]
        self.assertFalse(evaluation.grade_web_research("codex", web_research_report(), events)["governed_tools"])

    def test_an_incomplete_root_fails_completed(self):
        report = web_research_report(root_status="failed", failing=("execution.completed",))
        self.assertFalse(evaluation.grade_web_research("codex", report, web_hands("codex"))["completed"])

    def test_the_subprocess_runs_one_fixture_case_and_keeps_its_task(self):
        args = SimpleNamespace(api_base_url="http://127.0.0.1:3002", execution_web_research_case="direct_openai_sarvam_pricing",
                               http_timeout_secs=120.0)
        command = evaluation.web_research_command(args, Path("/tmp/out"))
        self.assertIn("--case", command)
        self.assertEqual(command[command.index("--case") + 1], "direct_openai_sarvam_pricing")
        self.assertEqual(command[command.index("--output-dir") + 1], "/tmp/out")
        self.assertNotIn("--delete-tasks", command, "the lane reads the task's journal after the subprocess")
        self.assertTrue(command[1].endswith("eval-web-researcher-live.py"))


class EngineSwitchContracts(unittest.TestCase):
    def test_a_restore_outlives_a_service_restart(self):
        # Measured 2026-09-18: another session restarted the service during
        # a case, the restore hit connection refused, and the persisted run
        # engine stayed `grok`. A restore waits for the service to come back
        # and tries again before giving up.
        client = Mock()
        client.json.side_effect = [({"current": "magician", "run_model": "default"}, 0),  # save
                                   ({}, 0),                                              # select
                                   ConnectionRefusedError("refused"),                     # restore #1
                                   ConnectionRefusedError("refused"),                     # readiness probe
                                   ({}, 0),                                              # readiness probe ok
                                   ({}, 0)]                                              # restore #2
        switch = evaluation.GlobalEngineSwitch(client, retry_wait_secs=0.0, retry_window_secs=1.0)
        switch.save()
        switch.select("grok")
        switch.restore()
        self.assertIsNone(switch.restore_error)
        puts = [call for call in client.json.call_args_list if call.args[0] == "PUT"]
        self.assertEqual(puts[-1].kwargs["body"], {"harness_engine": "magician", "harness_model": "default"})

    def test_a_restore_that_never_reaches_the_service_is_reported(self):
        client = Mock()
        client.json.side_effect = [({"current": "magician", "run_model": "default"}, 0), ({}, 0)] + \
            [ConnectionRefusedError("refused")] * 50
        switch = evaluation.GlobalEngineSwitch(client, retry_wait_secs=0.0, retry_window_secs=0.05)
        switch.save()
        switch.select("grok")
        switch.restore()
        self.assertIn("refused", switch.restore_error)

