import importlib.util
from importlib.machinery import SourceFileLoader
import pathlib
import unittest


SCRIPT = pathlib.Path(__file__).parents[1] / "bin" / "claude-deep-research"
LOADER = SourceFileLoader("claude_deep_research", str(SCRIPT))
SPEC = importlib.util.spec_from_loader("claude_deep_research", LOADER)
MODULE = importlib.util.module_from_spec(SPEC)
LOADER.exec_module(MODULE)


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


if __name__ == "__main__":
    unittest.main()
