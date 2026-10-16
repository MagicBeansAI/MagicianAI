from __future__ import annotations

from pathlib import Path
import sys
import tempfile
import unittest


SCRIPTS_ROOT = Path(__file__).resolve().parent
if str(SCRIPTS_ROOT) not in sys.path:
    sys.path.insert(0, str(SCRIPTS_ROOT))

from codegraph_ext.magician_skills import MagicianSkillsExtension  # noqa: E402


class MagicianSkillsCodegraphTests(unittest.TestCase):
    def write_skill(self, root: Path, name: str, magician: str = "") -> Path:
        directory = root / "skillshub" / name
        directory.mkdir(parents=True)
        skill = directory / "SKILL.md"
        metadata = (
            f"metadata:\n  magician:\n{magician}"
            if magician
            else "metadata: {}\n"
        )
        skill.write_text(
            f"---\nname: {name}\ndescription: fixture\n{metadata}---\nBody.\n",
            encoding="utf-8",
        )
        return directory

    def test_runtime_contract_in_skill_md_is_the_tool_boundary(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.write_skill(
                root,
                "compiled-tool",
                "    runtime_contract:\n      schema_version: tool-runtime.skill-runtime.v1\n",
            )
            stale = self.write_skill(root, "plain-procedure")
            (stale / "tool_schema.yaml").write_text("name: stale\n", encoding="utf-8")

            nodes = list(MagicianSkillsExtension().discover_nodes(root, {}))
            kinds = {node["label"]: node["skill_type"] for node in nodes}

            self.assertEqual(kinds["compiled-tool"], "tool")
            self.assertEqual(kinds["plain-procedure"], "procedure")

    def test_personality_without_runtime_contract_stays_personality(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.write_skill(
                root,
                "warm-guide",
                "    personality:\n      active_mode: warm\n",
            )

            [node] = list(MagicianSkillsExtension().discover_nodes(root, {}))

            self.assertEqual(node["skill_type"], "personality")


if __name__ == "__main__":
    unittest.main()
