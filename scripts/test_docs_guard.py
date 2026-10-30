#!/usr/bin/env python3
"""Range checks skip files that only format-only (blame-ignored) commits touched."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

import docs_guard

ENV = {"GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@t", "GIT_COMMITTER_NAME": "t",
       "GIT_COMMITTER_EMAIL": "t@t"}


def git(repo, *args):
    return subprocess.run(["git", *args], cwd=repo, text=True, capture_output=True, check=True,
                          env={**os.environ, **ENV}).stdout.strip()


def commit(repo, files, message):
    for name, text in files.items():
        path = Path(repo, name)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)
    git(repo, "add", "-A")
    git(repo, "commit", "-q", "-m", message)
    return git(repo, "rev-parse", "HEAD")


class FormatOnlyCommitsTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.repo = self.tmp.name
        git(self.repo, "init", "-q", "-b", "main")
        self.base = commit(self.repo, {"a.rs": "fn a(){}\n", "b.rs": "fn b(){}\n"}, "base")
        self.cwd = os.getcwd()
        os.chdir(self.repo)

    def tearDown(self):
        os.chdir(self.cwd)
        self.tmp.cleanup()

    def test_files_only_a_listed_commit_touched_are_dropped(self):
        fmt = commit(self.repo, {"a.rs": "fn a() {}\n", "b.rs": "fn b() {}\n"}, "fmt")
        commit(self.repo, {"b.rs": "fn b() { 1; }\n", "docs/b.md": "b\n"}, "real")
        Path(self.repo, ".git-blame-ignore-revs").write_text(f"# format\n{fmt}\n")
        self.assertEqual(sorted(docs_guard.changed_files(False, self.base)),
                         ["b.rs", "docs/b.md"])

    def test_unlisted_commits_all_count(self):
        commit(self.repo, {"a.rs": "fn a() {}\n"}, "fmt")
        self.assertEqual(docs_guard.changed_files(False, self.base), ["a.rs"])

    def test_a_listed_commit_alone_leaves_nothing(self):
        fmt = commit(self.repo, {"a.rs": "fn a() {}\n"}, "fmt")
        Path(self.repo, ".git-blame-ignore-revs").write_text(fmt + "\n")
        self.assertEqual(docs_guard.changed_files(False, self.base), [])


if __name__ == "__main__":
    unittest.main()
