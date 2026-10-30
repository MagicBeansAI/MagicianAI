import contextlib
import importlib.util
import io
import json
import subprocess
import sys
import textwrap
from importlib.machinery import SourceFileLoader
import pathlib
import unittest


SCRIPT = pathlib.Path(__file__).parents[1] / "bin" / "claude-deep-research"
LOADER = SourceFileLoader("claude_deep_research", str(SCRIPT))
SPEC = importlib.util.spec_from_loader("claude_deep_research", LOADER)
MODULE = importlib.util.module_from_spec(SPEC)
LOADER.exec_module(MODULE)


def sse(events):
    """A readline() source carrying the events as server-sent events."""
    lines = []
    for event in events:
        lines.append(("event: " + event["type"] + "\n").encode())
        lines.append(("data: " + json.dumps(event) + "\n").encode())
        lines.append(b"\n")
    return io.BytesIO(b"".join(lines))


class ClaudeDeepResearchAdapterTest(unittest.TestCase):
    def test_output_budget_always_exceeds_supported_thinking_budget(self):
        self.assertEqual(MODULE.output_token_budget(5_000), 16_384)
        self.assertEqual(MODULE.output_token_budget(30_000), 34_096)

    def test_default_model_uses_adaptive_thinking_with_effort(self):
        config = MODULE.thinking_config(MODULE.DEFAULT_MODEL, {"thinking_budget": 9_000})
        self.assertEqual(MODULE.DEFAULT_MODEL, "claude-sonnet-5-5")
        self.assertEqual(config["thinking"], {"type": "adaptive", "display": "summarized"})
        self.assertEqual(config["output_config"], {"effort": "medium"})
        self.assertNotIn("budget_tokens", config["thinking"])
        self.assertEqual(
            MODULE.thinking_config("claude-opus-5-5", {"effort": "high"})["output_config"],
            {"effort": "high"},
        )
        with self.assertRaises(ValueError):
            MODULE.thinking_config("claude-sonnet-5-5", {"effort": "none"})

    def test_budget_thinking_models_keep_budget_tokens(self):
        config = MODULE.thinking_config("claude-haiku-4-5", {"thinking_budget": 8_000})
        self.assertEqual(config["thinking"], {"type": "enabled", "budget_tokens": 8_000})
        self.assertEqual(config["max_tokens"], 16_384)
        self.assertNotIn("output_config", config)

    def test_domain_allowlist_is_normalized_deduplicated_and_bounded(self):
        self.assertEqual(
            MODULE.normalized_domains("Example.COM, example.com.,docs.example.com"),
            ["example.com", "docs.example.com"],
        )
        with self.assertRaises(ValueError):
            MODULE.normalized_domains("https://example.com/path")

    def test_normalizer_deduplicates_sources_and_limits_thinking_preview(self):
        output = MODULE.normalize({
            "model": "claude-sonnet-5-5",
            "usage": {"input_tokens": 4},
            "content": [
                {"type": "thinking", "thinking": "x" * 900},
                {"type": "text", "text": "answer", "citations": [
                    {"title": "A", "url": "https://example.test/a"},
                ]},
                {"type": "web_search_tool_result", "content": [
                    {"type": "web_search_result", "title": "A2", "url": "https://example.test/a"},
                    {"type": "web_search_result", "title": "bad", "url": "file:///secret"},
                ]},
            ],
        })
        self.assertEqual(output["answer"], "answer")
        self.assertEqual(len(output["thinking_preview"]), 500)
        self.assertEqual(output["sources"], [{"title": "A", "url": "https://example.test/a"}])


    def test_streamed_message_is_rebuilt_from_its_events(self):
        events = [
            {"type": "message_start", "message": {"model": "claude-sonnet-5-5", "usage": {"input_tokens": 10}}},
            {"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": ""}},
            {"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "plan"}},
            {"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "sig"}},
            {"type": "content_block_stop", "index": 0},
            {"type": "content_block_start", "index": 1, "content_block": {"type": "server_tool_use", "id": "srv_1", "name": "web_search", "input": {}}},
            {"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "{\"query\": "}},
            {"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "\"rust\"}"}},
            {"type": "content_block_stop", "index": 1},
            {"type": "content_block_start", "index": 2, "content_block": {"type": "text", "text": ""}},
            {"type": "content_block_delta", "index": 2, "delta": {"type": "text_delta", "text": "Rust is "}},
            {"type": "content_block_delta", "index": 2, "delta": {"type": "citations_delta", "citation": {"title": "R", "url": "https://rust-lang.org"}}},
            {"type": "content_block_delta", "index": 2, "delta": {"type": "text_delta", "text": "a language."}},
            {"type": "content_block_stop", "index": 2},
            {"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 7}},
            {"type": "message_stop"},
        ]
        message = MODULE.read_message(sse(events), MODULE.time.monotonic() + 60)
        self.assertEqual(message["stop_reason"], "end_turn")
        self.assertEqual(message["usage"], {"input_tokens": 10, "output_tokens": 7})
        self.assertEqual(message["content"][0], {"type": "thinking", "thinking": "plan", "signature": "sig"})
        self.assertEqual(message["content"][1]["input"], {"query": "rust"})
        self.assertEqual(message["content"][2]["text"], "Rust is a language.")
        self.assertEqual(MODULE.normalize(message)["sources"], [{"title": "R", "url": "https://rust-lang.org"}])

    def test_stream_error_event_is_reported(self):
        events = [{"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}]
        with self.assertRaisesRegex(MODULE.ResearchError, "overloaded_error: Overloaded"):
            MODULE.read_message(sse(events), MODULE.time.monotonic() + 60)

    def test_paused_turn_is_resumed_with_the_content_so_far(self):
        calls = []
        replies = [
            {"model": "m", "usage": {"output_tokens": 2}, "stop_reason": "pause_turn",
             "content": [{"type": "server_tool_use", "id": "srv_1", "name": "web_search", "input": {}}]},
            {"model": "m", "usage": {"output_tokens": 3}, "stop_reason": "end_turn",
             "content": [{"type": "text", "text": "done"}]},
        ]

        def fake_call(api_key, body, deadline, blocks=None):
            calls.append(body["messages"])
            return replies[len(calls) - 1]

        original = MODULE.call_anthropic
        MODULE.call_anthropic = fake_call
        try:
            result = MODULE.run_research("k", {"model": "m", "messages": [{"role": "user", "content": "q"}]}, 0)
        finally:
            MODULE.call_anthropic = original
        self.assertEqual(len(calls), 2)
        self.assertEqual(calls[1][1], {"role": "assistant", "content": replies[0]["content"]})
        self.assertEqual(result["usage"], {"output_tokens": 5})
        self.assertEqual(MODULE.normalize(result)["answer"], "done")

    def test_research_stream_can_continue_past_ten_minutes(self):
        from unittest.mock import patch
        events = [
            {"type": "message_start", "message": {"model": "claude-sonnet-5-5", "usage": {}}},
            {"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": "findings"}},
            {"type": "message_stop"},
        ]
        with patch.object(MODULE.time, "monotonic", return_value=1200):
            result = MODULE.read_message(sse(events), MODULE.RUN_DEADLINE_SECS)
        self.assertEqual(MODULE.normalize(result)["answer"], "findings")
        self.assertEqual(MODULE.RUN_DEADLINE_SECS, 35 * 60 - 30)

    def test_a_passed_deadline_stops_the_stream(self):
        with self.assertRaisesRegex(MODULE.ResearchError, "2070 s limit"):
            MODULE.read_message(sse([{"type": "message_stop"}]), MODULE.time.monotonic() - 1)


    def test_findings_survive_a_stream_that_fails_midway(self):
        progress = MODULE.Progress("claude-sonnet-5-5")
        events = [
            {"type": "message_start", "message": {"model": "claude-sonnet-5-5", "usage": {}}},
            {"type": "content_block_start", "index": 0, "content_block": {"type": "web_search_tool_result", "tool_use_id": "srv_1", "content": [
                {"type": "web_search_result", "title": "Rust 1.99", "url": "https://blog.rust-lang.org/1.99"},
            ]}},
            {"type": "content_block_stop", "index": 0},
            {"type": "content_block_start", "index": 1, "content_block": {"type": "text", "text": ""}},
            {"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": "Rust 1.99 adds"}},
            {"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}},
        ]
        with self.assertRaises(MODULE.ResearchError):
            MODULE.read_message(sse(events), MODULE.time.monotonic() + 60, progress.blocks)
        captured = io.StringIO()
        with contextlib.redirect_stdout(captured):
            code = MODULE.report_partial(progress, "Anthropic stream failed: overloaded_error")
        output = json.loads(captured.getvalue())
        self.assertEqual(code, 0)
        self.assertEqual(output["answer"], "Rust 1.99 adds")
        self.assertEqual(output["sources"], [{"title": "Rust 1.99", "url": "https://blog.rust-lang.org/1.99"}])
        self.assertEqual(output["incomplete"], "Anthropic stream failed: overloaded_error")

    def test_a_run_with_no_findings_reports_only_the_reason(self):
        captured = io.StringIO()
        with contextlib.redirect_stdout(captured):
            code = MODULE.report_partial(MODULE.Progress("m"), "Anthropic request was unavailable")
        self.assertEqual(code, 1)
        self.assertEqual(json.loads(captured.getvalue()), {"error": "Anthropic request was unavailable"})

    def test_fetched_pages_count_as_sources(self):
        output = MODULE.normalize({"content": [{
            "type": "web_fetch_tool_result",
            "content": {"type": "web_fetch_result", "url": "https://example.test/page",
                        "content": {"type": "document", "title": "Page"}},
        }]})
        self.assertEqual(output["sources"], [{"title": "Page", "url": "https://example.test/page"}])

    def test_sigterm_hands_back_the_findings_before_the_kill(self):
        child = textwrap.dedent(f"""
            import os, signal, sys, time
            from importlib.machinery import SourceFileLoader
            module = SourceFileLoader("claude_deep_research", {str(SCRIPT)!r}).load_module()
            module.PROGRESS = module.Progress("claude-sonnet-5-5")
            module.PROGRESS.blocks[0] = {{"type": "text", "text": "partial finding"}}
            signal.signal(signal.SIGTERM, module.on_terminate)
            os.kill(os.getpid(), signal.SIGTERM)
            time.sleep(5)
            sys.exit(3)
        """)
        result = subprocess.run([sys.executable, "-c", child], capture_output=True, text=True, timeout=30)
        output = json.loads(result.stdout)
        self.assertEqual(result.returncode, 0)
        self.assertEqual(output["answer"], "partial finding")
        self.assertEqual(output["incomplete"], "stopped by its time limit before it finished")


if __name__ == "__main__":
    unittest.main()
