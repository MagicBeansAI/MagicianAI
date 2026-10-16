import importlib.util
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


if __name__ == "__main__":
    unittest.main()
