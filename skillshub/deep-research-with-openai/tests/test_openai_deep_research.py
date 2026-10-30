import contextlib
import importlib.util
import io
import os
import subprocess
import sys
import textwrap
from importlib.machinery import SourceFileLoader
import json
import pathlib
import unittest


SCRIPT = pathlib.Path(__file__).parents[1] / "bin" / "openai-deep-research"
LOADER = SourceFileLoader("openai_deep_research", str(SCRIPT))
SPEC = importlib.util.spec_from_loader("openai_deep_research", LOADER)
MODULE = importlib.util.module_from_spec(SPEC)
LOADER.exec_module(MODULE)


class OpenAIDeepResearchAdapterTest(unittest.TestCase):
    def test_normalizer_deduplicates_citations_and_bounds_unknown_usage(self):
        response = {
            "model": "o4-mini-deep-research",
            "usage": "invalid",
            "output": [{
                "type": "message",
                "content": [{
                    "type": "output_text",
                    "text": "answer",
                    "annotations": [
                        {"type": "url_citation", "title": "A", "url": "https://example.test/a"},
                        {"type": "url_citation", "title": "A2", "url": "https://example.test/a"},
                        {"type": "url_citation", "title": "bad", "url": "file:///secret"},
                    ],
                }],
            }],
        }
        output = MODULE.normalize(response, "resp_123", 2)
        self.assertEqual(output["answer"], "answer")
        self.assertEqual(output["sources"], [{"title": "A", "url": "https://example.test/a"}])
        self.assertEqual(output["usage"], {})

    def test_post_uses_fixed_origin_and_keeps_key_out_of_body(self):
        captured = {}

        class Response:
            def __enter__(self): return self
            def __exit__(self, *_): return False
            def read(self, _): return b'{"id":"resp_1","status":"queued"}'

        class Opener:
            def open(self, request, timeout):
                captured.update(url=request.full_url, headers=dict(request.header_items()), body=request.data, timeout=timeout)
                return Response()

        original = MODULE.OPENER
        MODULE.OPENER = Opener()
        try:
            result = MODULE.post_response("secret-key", {"model": "m", "input": "q"})
        finally:
            MODULE.OPENER = original
        self.assertEqual(result["id"], "resp_1")
        self.assertEqual(captured["url"], MODULE.RESPONSES_URL)
        self.assertEqual(captured["headers"]["Authorization"], "Bearer secret-key")
        self.assertNotIn(b"secret-key", captured["body"])
        self.assertEqual(json.loads(captured["body"])["input"], "q")


    def test_error_detail_keeps_code_and_message_and_redacts_keys(self):
        detail = MODULE.error_detail({"code": "invalid_api_key", "message": "Incorrect API key provided: sk-proj-ab12****cd34."})
        self.assertEqual(detail, "invalid_api_key: Incorrect API key provided: [redacted].")
        self.assertEqual(MODULE.error_detail(None), "")

    def test_retired_deep_research_models_name_their_replacement(self):
        self.assertIn(MODULE.DEFAULT_MODEL, MODULE.retired_model_hint("o4-mini-deep-research"))
        self.assertEqual(MODULE.retired_model_hint(MODULE.DEFAULT_MODEL), "")


    def setUp(self):
        MODULE.RUN.update(response_id="", status="")

    def run_main(self, params):
        captured = io.StringIO()
        original_input = MODULE.read_input
        MODULE.read_input = lambda: params
        os.environ["OPENAI_API_KEY"] = "test-key"
        try:
            with contextlib.redirect_stdout(captured):
                code = MODULE.main()
        finally:
            MODULE.read_input = original_input
            del os.environ["OPENAI_API_KEY"]
        return code, json.loads(captured.getvalue())

    def test_default_polling_waits_past_ten_minutes_and_still_finishes(self):
        from unittest.mock import patch
        clock = [0]
        def sleep(seconds): clock[0] += seconds
        def get(api_key, response_id):
            status = "completed" if clock[0] >= 1200 else "in_progress"
            return {"id": response_id, "status": status, "model": MODULE.DEFAULT_MODEL, "output": []}
        with patch.object(MODULE.time, "monotonic", side_effect=lambda: clock[0]), \
             patch.object(MODULE.time, "sleep", side_effect=sleep), \
             patch.object(MODULE, "post_response", return_value={"id": "resp_long", "status": "queued"}), \
             patch.object(MODULE, "get_response", side_effect=get):
            code, output = self.run_main({"query": "q"})
        self.assertEqual(code, 0)
        self.assertEqual(clock[0], 1200)
        self.assertGreater(output["poll_attempts"], 60)

    def test_default_polling_returns_resume_id_before_35_minute_runtime_cap(self):
        from unittest.mock import patch
        clock = [0]
        def sleep(seconds): clock[0] += seconds
        with patch.object(MODULE.time, "monotonic", side_effect=lambda: clock[0]), \
             patch.object(MODULE.time, "sleep", side_effect=sleep), \
             patch.object(MODULE, "post_response", return_value={"id": "resp_long", "status": "queued"}), \
             patch.object(MODULE, "get_response", return_value={"id": "resp_long", "status": "in_progress"}):
            code, output = self.run_main({"query": "q"})
        self.assertEqual(code, 1)
        self.assertEqual(output["response_id"], "resp_long")
        self.assertEqual(clock[0], 35 * 60 - 30)
        self.assertLess(clock[0], 35 * 60)

    def test_an_unfinished_run_hands_back_its_response_id(self):
        replies = iter([{"id": "resp_abc", "status": "in_progress"}])
        original = (MODULE.post_response, MODULE.get_response, MODULE.time.sleep)
        MODULE.post_response = lambda api_key, body: next(replies)
        MODULE.get_response = lambda api_key, response_id: {"id": response_id, "status": "in_progress"}
        MODULE.time.sleep = lambda seconds: None
        try:
            code, output = self.run_main({"query": "q", "max_poll_attempts": 2, "poll_interval_secs": 1})
        finally:
            MODULE.post_response, MODULE.get_response, MODULE.time.sleep = original
        self.assertEqual(code, 1)
        self.assertEqual(output["response_id"], "resp_abc")
        self.assertEqual(output["status"], "in_progress")
        self.assertIn("call run again with response_id `resp_abc`", output["error"])

    def test_response_id_collects_an_earlier_run_without_starting_new_research(self):
        def no_post(api_key, body):
            raise AssertionError("a resumed run must not start new research")

        done = {"id": "resp_abc", "status": "completed", "model": "gpt-6.1-sol", "output": [{
            "type": "message",
            "content": [{"type": "output_text", "text": "found it", "annotations": []}],
        }]}
        original = (MODULE.post_response, MODULE.get_response)
        MODULE.post_response = no_post
        MODULE.get_response = lambda api_key, response_id: done
        try:
            code, output = self.run_main({"response_id": "resp_abc"})
        finally:
            MODULE.post_response, MODULE.get_response = original
        self.assertEqual(code, 0)
        self.assertEqual(output["answer"], "found it")
        self.assertEqual(output["response_id"], "resp_abc")

    def test_a_run_without_query_or_response_id_is_refused(self):
        code, output = self.run_main({})
        self.assertEqual(code, 1)
        self.assertIn("query is required", output["error"])
        code, output = self.run_main({"response_id": "../etc"})
        self.assertIn("response_id must be", output["error"])

    def test_a_finished_or_unknown_run_reports_only_the_reason(self):
        captured = io.StringIO()
        with contextlib.redirect_stdout(captured):
            MODULE.report_unfinished("OpenAI request was unavailable")
        self.assertEqual(json.loads(captured.getvalue()), {"error": "OpenAI request was unavailable"})
        MODULE.RUN.update(response_id="resp_abc", status="failed")
        captured = io.StringIO()
        with contextlib.redirect_stdout(captured):
            MODULE.report_unfinished("research did not complete")
        self.assertNotIn("response_id", json.loads(captured.getvalue()))

    def test_sigterm_hands_back_the_response_id_before_the_kill(self):
        child = textwrap.dedent(f"""
            import os, signal, sys, time
            from importlib.machinery import SourceFileLoader
            module = SourceFileLoader("openai_deep_research", {str(SCRIPT)!r}).load_module()
            module.RUN.update(response_id="resp_abc", status="queued")
            signal.signal(signal.SIGTERM, module.on_terminate)
            os.kill(os.getpid(), signal.SIGTERM)
            time.sleep(5)
            sys.exit(3)
        """)
        result = subprocess.run([sys.executable, "-c", child], capture_output=True, text=True, timeout=30)
        output = json.loads(result.stdout)
        self.assertEqual(output["response_id"], "resp_abc")
        self.assertIn("stopped by its time limit", output["error"])


if __name__ == "__main__":
    unittest.main()
