import fcntl
import importlib.util
import os
from pathlib import Path
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

if __name__ == '__main__':
    unittest.main()
