"""Self-tests for the Task 21 typed-storage boundary ratchet."""

from __future__ import annotations

import importlib.util
import unittest
import tempfile
from pathlib import Path


SCRIPT = Path(__file__).with_name("check_typed_storage_boundaries.py")
SPEC = importlib.util.spec_from_file_location("check_typed_storage_boundaries", SCRIPT)
assert SPEC and SPEC.loader
guard = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(guard)


class TypedStorageBoundariesTest(unittest.TestCase):
    def test_parallel_worktree_manifests_do_not_change_the_parent_gate(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            composition = root / "magician-bin" / "Cargo.toml"
            composition.parent.mkdir()
            composition.write_text('[dependencies]\nmagician-storage = "0.1"\n')
            for folder in [".worktrees/parallel", ".local/remote-sync/integration"]:
                nested = root / folder / "magician" / "Cargo.toml"
                nested.parent.mkdir(parents=True)
                nested.write_text('[dependencies]\nmagician-storage-state = "0.1"\n')
            manifest = root / "magician" / "Cargo.toml"
            manifest.parent.mkdir()
            manifest.write_text('[dependencies]\nmagician-storage-state = "0.1"\n')
            self.assertEqual(
                guard.scan_manifests(root, {}),
                ["ambient_backend: magician/Cargo.toml depends on magician-storage-state"],
            )

    def test_a_worktree_is_scanned_when_it_is_the_selected_root(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw) / ".worktrees" / "parallel"
            source = root / "magician/src/evil/bypass.rs"
            source.parent.mkdir(parents=True)
            source.write_text("StorageRuntime::open_local(root, profile, owner);\n")
            unmatched, _ = guard.scan_rule(
                root, "ambient_backend", guard.RULES["ambient_backend"], [], []
            )
            self.assertEqual(len(unmatched), 1)
            self.assertIn("magician/src/evil/bypass.rs", unmatched[0])

    def test_checkout_under_build_target_is_still_scanned(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw) / "target" / "fixture"
            source = root / "magician/src/evil/bypass.rs"
            source.parent.mkdir(parents=True)
            source.write_text("StorageRuntime::open_local(root, profile, owner);\n")
            unmatched, stale = guard.scan_rule(
                root, "ambient_backend", guard.RULES["ambient_backend"], [], []
            )
            self.assertEqual(len(unmatched), 1)
            self.assertIn("magician/src/evil/bypass.rs", unmatched[0])
            self.assertEqual(stale, [])

    def test_local_snapshot_is_scanned_when_selected_as_the_root(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw) / ".local/remote-sync/integration"
            manifest = root / "magician" / "Cargo.toml"
            manifest.parent.mkdir(parents=True)
            manifest.write_text('[dependencies]\nmagician-storage-state = "0.1"\n')
            composition = root / "magician-bin" / "Cargo.toml"
            composition.parent.mkdir()
            composition.write_text('[dependencies]\nmagician-storage = "0.1"\n')
            self.assertEqual(
                guard.scan_manifests(root, {}),
                ["ambient_backend: magician/Cargo.toml depends on magician-storage-state"],
            )

    def test_self_test_rejects_new_bypasses(self) -> None:
        guard.self_test()

    def test_repo_allowlist_passes(self) -> None:
        repo = Path(__file__).resolve().parents[1]
        hits = guard.scan(repo, guard.load_allowlist())
        self.assertEqual(hits, [])


if __name__ == "__main__":
    unittest.main()
