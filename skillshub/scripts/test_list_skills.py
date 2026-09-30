from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).with_name("list_skills.py")
SPEC = importlib.util.spec_from_file_location("list_skills", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
LIST_SKILLS = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(LIST_SKILLS)


class ListSkillsTests(unittest.TestCase):
    def test_workspace_root_includes_scopes_segment(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            data_root = Path(directory)
            self.assertEqual(
                LIST_SKILLS.workspace_skills_root(data_root, "anonymous/default"),
                data_root / "scopes" / "anonymous" / "default" / "skills",
            )


if __name__ == "__main__":
    unittest.main()
