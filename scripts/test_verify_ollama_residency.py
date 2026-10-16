import json
import unittest

from scripts.verify_ollama_residency import canonical_model_name, verify_residency


def process_state(name: str, context: int = 32768) -> str:
    return json.dumps({"models": [{"model": name, "context_length": context}]})


class OllamaResidencyVerificationTests(unittest.TestCase):
    def test_bare_expected_name_matches_default_latest_tag(self) -> None:
        self.assertEqual(
            verify_residency(
                process_state("qwen3.8-ud2-mtp:latest"),
                "qwen3.8-ud2-mtp\t32768\n",
            ),
            [],
        )

    def test_default_registry_qualified_name_matches_bare_alias(self) -> None:
        self.assertEqual(
            verify_residency(
                process_state("registry.ollama.ai/library/qwen3.8-ud2-mtp:latest"),
                "qwen3.8-ud2-mtp\t32768\n",
            ),
            [],
        )

    def test_untagged_api_name_matches_explicit_latest(self) -> None:
        self.assertEqual(
            verify_residency(process_state("gemma3"), "gemma3:latest\t32768\n"),
            [],
        )

    def test_explicit_tag_substitution_is_rejected(self) -> None:
        self.assertEqual(
            verify_residency(process_state("gemma3:q4"), "gemma3:q8\t32768\n"),
            ["gemma3:q8 is not resident"],
        )

    def test_namespace_substitution_is_rejected(self) -> None:
        self.assertEqual(
            verify_residency(
                process_state("other/qwen3.8-ud2-mtp:latest"),
                "qwen3.8-ud2-mtp\t32768\n",
            ),
            ["qwen3.8-ud2-mtp is not resident"],
        )

    def test_context_substitution_is_rejected(self) -> None:
        self.assertEqual(
            verify_residency(process_state("gemma3:latest", 8192), "gemma3\t32768\n"),
            ["gemma3 context is 8192, expected 32768"],
        )

    def test_invalid_process_shape_fails_closed(self) -> None:
        self.assertEqual(
            verify_residency("{}", "gemma3\t32768\n"),
            ["invalid /api/ps response: models must be an array"],
        )

    def test_canonicalizer_preserves_explicit_nondefault_identity(self) -> None:
        self.assertEqual(
            canonical_model_name("hf.co/mykor/pplx-embed-v1-4b-GGUF:Q6_K"),
            "hf.co/mykor/pplx-embed-v1-4b-gguf:q6_k",
        )


if __name__ == "__main__":
    unittest.main()
