import importlib.util
import json
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("presentation_identity_codegen.py")
SPEC = importlib.util.spec_from_file_location("presentation_identity_codegen", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
CODEGEN = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CODEGEN)


class PresentationIdentityCodegenTests(unittest.TestCase):
    def write_manifest(self, value: object) -> Path:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        path = Path(directory.name) / "identity.json"
        path.write_text(json.dumps(value), encoding="utf-8")
        return path

    def valid_manifest(self) -> dict[str, object]:
        return {
            "schema_version": 1,
            "product_name": "Example",
            "host_app_name": "Example Desktop",
            "assistant_fallback_name": "Assistant",
        }

    def test_all_platforms_are_rendered_from_one_identity(self) -> None:
        outputs = CODEGEN.render_outputs(self.valid_manifest())
        self.assertEqual(set(outputs), set(CODEGEN.OUTPUT_PATHS.values()))
        for content in outputs.values():
            self.assertIn("Example", content)
            self.assertNotIn("Magician", content)
        self.assertIn("Assistant", outputs[CODEGEN.OUTPUT_PATHS["rust"]])
        self.assertIn("Assistant", outputs[CODEGEN.OUTPUT_PATHS["typescript"]])
        self.assertIn("Assistant", outputs[CODEGEN.OUTPUT_PATHS["swift"]])
        self.assertIn("Assistant", outputs[CODEGEN.OUTPUT_PATHS["kotlin"]])
        self.assertNotIn("Assistant", outputs[CODEGEN.OUTPUT_PATHS["desktop_rust"]])
        self.assertNotIn(
            "Assistant", outputs[CODEGEN.OUTPUT_PATHS["desktop_typescript"]]
        )
        self.assertIn(
            'PRODUCT_NAME: string = "Example"',
            outputs[CODEGEN.OUTPUT_PATHS["typescript"]],
        )

    def test_manifest_rejects_unknown_and_missing_fields(self) -> None:
        value = self.valid_manifest()
        value["unexpected"] = True
        with self.assertRaisesRegex(ValueError, "keys differ"):
            CODEGEN.load_identity(self.write_manifest(value))

        value = self.valid_manifest()
        del value["host_app_name"]
        with self.assertRaisesRegex(ValueError, "keys differ"):
            CODEGEN.load_identity(self.write_manifest(value))

    def test_manifest_rejects_unbounded_or_unsafe_names(self) -> None:
        for invalid in ("", " padded ", "line\nbreak", "x" * 81):
            with self.subTest(invalid=invalid):
                value = self.valid_manifest()
                value["product_name"] = invalid
                with self.assertRaises(ValueError):
                    CODEGEN.load_identity(self.write_manifest(value))

    def test_kotlin_output_escapes_interpolation_characters(self) -> None:
        value = self.valid_manifest()
        value["product_name"] = "Example $ Edition"
        outputs = CODEGEN.render_outputs(value)
        self.assertIn(
            r"Example \$ Edition", outputs[CODEGEN.OUTPUT_PATHS["kotlin"]]
        )
        self.assertIn("Example $ Edition", outputs[CODEGEN.OUTPUT_PATHS["swift"]])

    def test_repository_manifest_and_generated_outputs_are_current(self) -> None:
        identity = CODEGEN.load_identity()
        outputs = CODEGEN.render_outputs(identity)
        self.assertEqual(CODEGEN.check(outputs, identity), 0)


if __name__ == "__main__":
    unittest.main()
