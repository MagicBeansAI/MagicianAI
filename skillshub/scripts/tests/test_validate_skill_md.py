from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).resolve().parents[1] / "validate_skill_md.py"
SPEC = importlib.util.spec_from_file_location("validate_skill_md", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
VALIDATOR = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(VALIDATOR)


class GovernedContentManifestTests(unittest.TestCase):
    def write_skill(
        self,
        root: Path,
        *,
        extension: str,
        runtime_contract: bool = True,
        runtime_actions: bool = True,
    ) -> Path:
        skill = root / "search-demo"
        skill.mkdir()
        contract = """
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires: {bins: [search-demo]}
      runtime: {protocol: cli, command_prefix: []}
""" if runtime_contract else ""
        actions = """
    runtime_actions:
      schema_version: tool-runtime.typed-action-overrides.v2
      actions:
        run:
          description: Search.
          parameters: {}
""" if runtime_actions else ""
        if extension == "content_source":
            extension_body = """
    content_source:
      schema_version: 1
      adapter: {}
      capability: {name: search-demo, action: run}
      output: {}
"""
        elif extension == "content_reader":
            extension_body = """
    content_reader:
      schema_version: 1
      reader: {}
      capability: {name: search-demo, action: run}
      output: {}
"""
        else:
            extension_body = """
    observe_source:
      schema_version: 1
      source: {}
      profiles: []
"""
        (skill / "SKILL.md").write_text(
            f"""---
name: search-demo
description: Search demo.
metadata:
  magician:
{extension_body}{contract}{actions}---
Body.
""",
            encoding="utf-8",
        )
        return skill

    def test_governed_catalog_satisfies_embedded_content_contracts(self) -> None:
        for extension in ("content_source", "content_reader"):
            with self.subTest(extension=extension), tempfile.TemporaryDirectory() as temporary:
                skill = self.write_skill(Path(temporary), extension=extension)
                self.assertEqual(VALIDATOR.validate(skill), [])

    def test_partial_governed_catalog_does_not_satisfy_content_contract(self) -> None:
        for runtime_contract, runtime_actions in [(False, False), (True, False), (False, True)]:
            with self.subTest(
                runtime_contract=runtime_contract, runtime_actions=runtime_actions
            ), tempfile.TemporaryDirectory() as temporary:
                skill = self.write_skill(
                    Path(temporary),
                    extension="content_source",
                    runtime_contract=runtime_contract,
                    runtime_actions=runtime_actions,
                )
                errors = VALIDATOR.validate(skill)
                self.assertTrue(
                    any("same SKILL.md" in error for error in errors), errors
                )

    def test_observe_source_is_embedded_but_does_not_require_an_executable(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            skill = self.write_skill(
                Path(temporary),
                extension="observe_source",
                runtime_contract=False,
                runtime_actions=False,
            )
            self.assertEqual(VALIDATOR.validate(skill), [])

    def test_legacy_skill_sidecars_are_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            skill = self.write_skill(Path(temporary), extension="content_source")
            (skill / "content_source.yaml").write_text("schema_version: 1\n")
            (skill / "tool_schema.yaml").write_text("name: search-demo\n")
            errors = VALIDATOR.validate(skill)
            self.assertEqual(
                sum("legacy skill sidecars" in error for error in errors),
                2,
                errors,
            )


if __name__ == "__main__":
    unittest.main()
