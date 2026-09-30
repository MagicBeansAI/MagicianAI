"""Self-tests for the Task 21 typed-storage boundary ratchet."""

from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("check_typed_storage_boundaries.py")
SPEC = importlib.util.spec_from_file_location("check_typed_storage_boundaries", SCRIPT)
assert SPEC and SPEC.loader
guard = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(guard)


class TypedStorageBoundariesTest(unittest.TestCase):
    def test_self_test_rejects_new_bypasses(self) -> None:
        guard.self_test()

    def test_repo_allowlist_passes(self) -> None:
        repo = Path(__file__).resolve().parents[1]
        hits = guard.scan(repo, guard.load_allowlist())
        self.assertEqual(hits, [])


if __name__ == "__main__":
    unittest.main()
