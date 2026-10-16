"""Provider-free tests of the plane-door conformance lane's contracts."""
import io
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

if __package__:
    from . import eval_harness_plane as plane
else:
    import eval_harness_plane as plane


def stream(*frames):
    """An SSE body as the door writes it: `data:` lines, blank-line separated."""
    text = "".join(f"data: {json.dumps(frame)}\n\n" for frame in frames)
    return io.BytesIO(text.encode())


PROMPT = {"jsonrpc": "2.0", "id": "pltel_1", "method": "elicitation/create", "params": {
    "mode": "form", "message": "Which word should the result contain?",
    "requestedSchema": {"type": "object", "properties": {"value": {"type": "string", "maxLength": 4096}},
                        "required": ["value"]}}}


class SseContracts(unittest.TestCase):
    def test_frames_are_yielded_one_at_a_time_as_they_arrive(self):
        reader = plane.SseReader(stream({"a": 1}, {"b": 2}))
        self.assertEqual(reader.next(), {"a": 1})
        self.assertEqual(reader.next(), {"b": 2})
        self.assertIsNone(reader.next())

    def test_comment_and_event_lines_do_not_break_data(self):
        body = io.BytesIO(b": keepalive\n\nevent: message\ndata: {\"x\": 1}\n\n")
        self.assertEqual(plane.SseReader(body).next(), {"x": 1})


class AnswerContracts(unittest.TestCase):
    def test_a_text_question_is_answered_from_the_fixture_word(self):
        answer = plane.elicitation_answer(PROMPT, "amber-1234")
        self.assertEqual(answer, {"action": "accept", "content": {"value": "amber-1234"}})

    def test_choice_multi_choice_and_confirmation_fields_are_filled_by_shape(self):
        schema = {"type": "object", "properties": {
            "selected_id": {"type": "string", "oneOf": [{"const": "small"}, {"const": "large"}]},
            "selected_ids": {"type": "array", "items": {"anyOf": [{"const": "x"}, {"const": "y"}]}},
            "completed": {"type": "boolean"}, "guidance": {"type": "string"}},
            "required": ["selected_id", "selected_ids", "completed"]}
        prompt = {**PROMPT, "params": {**PROMPT["params"], "requestedSchema": schema}}
        content = plane.elicitation_answer(prompt, "amber-1234")["content"]
        self.assertEqual(content["selected_id"], "small")
        self.assertEqual(content["selected_ids"], ["x"])
        self.assertTrue(content["completed"])
        self.assertNotIn("guidance", content, "optional fields stay unanswered")


class ScriptedGrading(unittest.TestCase):
    def evidence(self, **overrides):
        base = {"session_established": True, "catalog": ["create_task", "run_task", "wait_for_run", "list_tasks"],
                "task_id": "task", "execution_id": "exec", "prompts": [PROMPT],
                "answer_status": 202, "cross_session_status": 400, "replay_status": 400,
                "status": "completed", "written": "amber-1234", "answer": "The word is amber-1234.",
                "revoked_call_status": 401, "word": "amber-1234"}
        base.update(overrides)
        return base

    def test_the_complete_round_trip_passes_every_gate(self):
        gates = plane.grade_scripted(self.evidence())
        self.assertTrue(all(gates.values()), gates)

    def test_the_run_must_ask_before_it_acts(self):
        self.assertFalse(plane.grade_scripted(self.evidence(prompts=[]))["elicitation_observed"])

    def test_the_effect_is_the_answered_word_in_the_file_and_the_reply(self):
        self.assertFalse(plane.grade_scripted(self.evidence(written="plum"))["effect"])
        self.assertFalse(plane.grade_scripted(self.evidence(answer="done"))["effect"])

    def test_the_door_must_refuse_other_sessions_replays_and_revoked_grants(self):
        self.assertFalse(plane.grade_scripted(self.evidence(cross_session_status=202))["cross_session_refused"])
        self.assertFalse(plane.grade_scripted(self.evidence(replay_status=202))["replay_refused"])
        self.assertFalse(plane.grade_scripted(self.evidence(revoked_call_status=200))["revoke_refuses_calls"])

    def test_the_catalog_must_advertise_the_run_tools(self):
        self.assertFalse(plane.grade_scripted(self.evidence(catalog=["create_task"]))["catalog_advertises_run_tools"])


class CliLaunchContracts(unittest.TestCase):
    def test_each_cli_gets_the_door_and_the_grant_the_way_its_engine_does(self):
        with tempfile.TemporaryDirectory() as home:
            for engine in ("claude_code", "codex", "grok", "agy"):
                spec = plane.cli_launch(engine, "create the task", "http://127.0.0.1:3002/api/magician/v2/plane/mcp",
                                        "plt_secret", Path(home))
                self.assertIn(engine.split("_")[0] if engine != "claude_code" else "claude", spec["argv"][0])
                self.assertIn("create the task", spec["argv"])
                joined = " ".join(spec["argv"])
                self.assertNotIn("plt_secret", joined, f"{engine}: the grant must never ride argv")
                self.assertNotIn("MAGICIAN_BEARER_TOKEN", spec["env"], f"{engine}: only the plt_ grant reaches the CLI")
        # Claude: the grant sits in the mcp-config file, native tools stripped.
        with tempfile.TemporaryDirectory() as home:
            spec = plane.cli_launch("claude_code", "p", "http://d/mcp", "plt_secret", Path(home))
            config = json.loads(Path(spec["argv"][spec["argv"].index("--mcp-config") + 1]).read_text())
            self.assertEqual(config["mcpServers"]["magician-plane"]["headers"]["Authorization"], "Bearer plt_secret")
            self.assertIn("--strict-mcp-config", spec["argv"])
            self.assertEqual(spec["argv"][spec["argv"].index("--tools") + 1], "")
            # The engine reads claude's final `json` result; the lane needs the
            # tool trace, which only the verbose stream carries.
            self.assertEqual(spec["argv"][spec["argv"].index("--output-format") + 1], "stream-json")
            self.assertIn("--verbose", spec["argv"])
        # Codex and grok: the grant rides an env var named in an isolated home's config.toml.
        for engine, var in (("codex", "CODEX_HOME"), ("grok", "GROK_HOME")):
            with tempfile.TemporaryDirectory() as home:
                spec = plane.cli_launch(engine, "p", "http://d/mcp", "plt_secret", Path(home))
                self.assertEqual(spec["env"]["MAGICIAN_PLANE_GRANT"], "plt_secret")
                config = (Path(spec["env"][var]) / "config.toml").read_text()
                self.assertIn('url = "http://d/mcp"', config)
                self.assertIn('bearer_token_env_var = "MAGICIAN_PLANE_GRANT"', config)
        # agy: registration goes through `agy mcp add --header`, before the positionals.
        with tempfile.TemporaryDirectory() as home:
            spec = plane.cli_launch("agy", "p", "http://d/mcp", "plt_secret", Path(home))
            add = next(command for command in spec["setup"] if command[:3] == ["agy", "mcp", "add"])
            self.assertLess(add.index("--header"), add.index("magician_plane"))
            self.assertEqual(spec["teardown"][0][:3], ["agy", "mcp", "remove"])


class CliGrading(unittest.TestCase):
    def test_the_task_must_exist_and_the_cli_must_have_called_the_plane(self):
        stdout = json.dumps({"type": "tool_use", "name": "mcp__magician-plane__create_task"})
        gates = plane.grade_cli({"returncode": 0, "stdout": stdout, "stderr": ""}, "HC-abc", ["HC-abc", "other"])
        self.assertTrue(all(gates.values()), gates)
        self.assertFalse(plane.grade_cli({"returncode": 0, "stdout": stdout, "stderr": ""}, "HC-abc", ["other"])["task_created"])
        self.assertFalse(plane.grade_cli({"returncode": 0, "stdout": "DONE", "stderr": ""}, "HC-abc", ["HC-abc"])["via_plane"])
        self.assertFalse(plane.grade_cli({"returncode": 2, "stdout": stdout, "stderr": "boom"}, "HC-abc", ["HC-abc"])["cli_exit_ok"])


class RunnerContracts(unittest.TestCase):
    def args(self, output):
        return SimpleNamespace(self_test=False, api_base_url="http://localhost:1", engines=None, cases=None, runs=1,
                               turn_timeout_secs=10, http_timeout_secs=1, output_dir=Path(output),
                               plane_answer_word=None)

    def test_outage_records_the_whole_matrix_and_starts_nothing(self):
        class Offline(Exception):
            pass
        with tempfile.TemporaryDirectory() as directory:
            client = Mock()
            with patch.object(plane, "login", side_effect=Offline("service offline")):
                self.assertEqual(plane.main(self.args(directory), lambda *_: client, Offline, lambda engine: (True, "")), 2)
            report = json.loads((Path(directory) / "report.json").read_text())
            self.assertEqual(len(report["results"]), len(plane.MATRIX))
            self.assertEqual(report["summary"]["inconclusive"], len(report["results"]))
            client.json.assert_not_called()


if __name__ == "__main__":
    unittest.main()
