import importlib.util
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("check_service_name_boundary.py")
SPEC = importlib.util.spec_from_file_location("service_name_boundary", SCRIPT)
assert SPEC and SPEC.loader
boundary = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(boundary)


class ServiceNameBoundaryTests(unittest.TestCase):
    def test_rejects_personification_and_product_attribution(self):
        for text in (
            "Magician will handle that for you.",
            "You are Magician's writing assistant.",
            "Chat with Magician.",
        ):
            with self.subTest(text=text):
                self.assertFalse(boundary.allowed(text))

    def test_allows_compatibility_and_explicit_operational_identity(self):
        for text in (
            "/api/magician/v2/chat",
            "MAGICIAN_PORT",
            "Restart Magician backend",
            "data-magician-source",
            "magician-config.yaml",
            "magician.execution-pipeline-roster.v1",
            "magician.delegated-child-recovery.v1",
            "magician.execution-stateless-children-handoff.v2",
            "Use magician storage status",
        ):
            with self.subTest(text=text):
                self.assertTrue(boundary.allowed(text))

    def test_allows_service_word_only_as_an_exact_wake_spelling(self):
        self.assertTrue(
            boundary.allowed_wake_spelling(
                "- magician", yaml_list_key="wake_spellings"
            )
        )
        self.assertFalse(
            boundary.allowed_wake_spelling("- magician", yaml_list_key="aliases")
        )
        self.assertFalse(
            boundary.allowed_wake_spelling(
                "- Magician assistant", yaml_list_key="wake_spellings"
            )
        )

    def test_repository_boundary_passes(self):
        self.assertEqual(boundary.run(), [])


if __name__ == "__main__":
    unittest.main()
