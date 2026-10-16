import fcntl
import importlib.util
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import unittest

spec = importlib.util.spec_from_file_location('maintenance', Path(__file__).with_name('maintain-build-cache.py'))
maintenance = importlib.util.module_from_spec(spec)
spec.loader.exec_module(maintenance)

class MaintenanceTests(unittest.TestCase):
    def test_retention_respects_build_lock_fresh_files_and_symlinks(self):
        with tempfile.TemporaryDirectory() as tmp:
            target = Path(tmp).resolve()
            cache = target / 'debug' / 'incremental'
            cache.mkdir(parents=True)
            old = cache / 'old-hash'; old.mkdir(); (old / 'data').write_bytes(b'x' * 32)
            age = time.time() - 10 * 86400
            os.utime(old / 'data', (age, age)); os.utime(old, (age, age))
            fresh = cache / 'fresh-hash'; fresh.mkdir(); (fresh / 'data').write_bytes(b'x' * 32)
            os.utime(fresh, (age, age))  # child is new, directory alone is misleading
            outside = target / 'not-cache'; outside.mkdir(); (outside / 'keep').write_text('safe')
            (cache / 'linked').symlink_to(outside, target_is_directory=True)
            with (target / 'debug' / '.cargo-lock').open('a') as lock:
                fcntl.flock(lock, fcntl.LOCK_EX)
                self.assertEqual(maintenance.maintain(target, 0, 86400), [])
            self.assertEqual(maintenance.maintain(target, 0, 86400, True), [str(old)])
            self.assertTrue(old.exists())
            self.assertEqual(maintenance.maintain(target, 0, 86400), [str(old)])
            self.assertTrue(fresh.exists()); self.assertTrue((outside / 'keep').exists())

class IncrementalPrunerTests(unittest.TestCase):
    def run_pruner(self, target, status, output='', dry_run=False):
        bin_dir = target / 'test-bin'
        bin_dir.mkdir(exist_ok=True)
        lsof = bin_dir / 'lsof'
        lsof.write_text('#!/bin/sh\nif [ -n "$PRUNE_TEST_LSOF_OUTPUT" ]; then printf "%s\\n" "$PRUNE_TEST_LSOF_OUTPUT"; fi\nexit "$PRUNE_TEST_LSOF_STATUS"\n')
        lsof.chmod(0o755)
        env = dict(os.environ, PATH=str(bin_dir) + os.pathsep + os.environ['PATH'],
                   PRUNE_TEST_LSOF_STATUS=str(status), PRUNE_TEST_LSOF_OUTPUT=output)
        command = [shutil.which('bash'), str(Path(__file__).with_name('prune-incremental.sh')),
                   '--target-dir', str(target), '--crate', 'fixture']
        if dry_run:
            command.append('--dry-run')
        return subprocess.run(command, env=env, capture_output=True, text=True, check=True)

    def test_open_files_and_inspection_failures_never_remove_cache(self):
        for status, output in [(0, 'COMMAND\nrustc metadata.rmeta'),
                               (1, 'COMMAND\nrustc metadata.rmeta'),
                               (1, 'lsof: access denied'), (2, '')]:
            with self.subTest(status=status, output=output), tempfile.TemporaryDirectory() as tmp:
                target = Path(tmp)
                cache = target / 'debug' / 'incremental' / 'fixture-hash'
                cache.mkdir(parents=True)
                (cache / 'metadata.rmeta').write_text('active compiler metadata')
                self.run_pruner(target, status, output)
                self.assertTrue((cache / 'metadata.rmeta').exists())

    def test_empty_no_match_prunes_only_selected_cache(self):
        with tempfile.TemporaryDirectory() as tmp:
            target = Path(tmp)
            cache = target / 'debug' / 'incremental' / 'fixture-hash'
            keep = cache.parent / 'other-hash'
            cache.mkdir(parents=True); keep.mkdir()
            (cache / 'data').write_text('closed cache')
            (keep / 'data').write_text('other crate')
            self.run_pruner(target, 1)
            self.assertFalse(cache.exists())
            self.assertTrue((keep / 'data').exists())

    def test_dry_run_preserves_closed_cache(self):
        with tempfile.TemporaryDirectory() as tmp:
            target = Path(tmp)
            cache = target / 'debug' / 'incremental' / 'fixture-hash'
            cache.mkdir(parents=True)
            (cache / 'data').write_text('closed cache')
            self.run_pruner(target, 1, dry_run=True)
            self.assertTrue((cache / 'data').exists())

if __name__ == '__main__':
    unittest.main()
