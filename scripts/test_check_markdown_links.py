"""Regression battery for scripts/check_markdown_links.py.

Every case here earned its place: the original battery validated the first
version, and three review rounds then found bugs the battery missed — an
inline triple-backtick span that blanked 600 lines of a live doc, an
OS-absolute target that resolved on exactly one machine, and a `/..` escape
that resolved against whatever happened to sit above the checkout. Each fix
added its cases below. Before this file existed the battery lived in a
scratchpad, which meant the review that asked "19/19 — says who?" had nothing
in the tree to point at.

Runs from `make check-links` alongside the guard itself.
"""

import importlib.util
import subprocess
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("check_markdown_links.py")
SPEC = importlib.util.spec_from_file_location("check_markdown_links", SCRIPT)
assert SPEC and SPEC.loader
guard = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(guard)


class MarkdownLinkGuardTests(unittest.TestCase):
    def setUp(self):
        self._dir = tempfile.TemporaryDirectory()
        root = Path(self._dir.name).resolve()
        (root / "sub").mkdir()
        (root / "sub" / "real.md").write_text("real\n")
        (root / "sub" / "CaseFile.md").write_text("x\n")
        (root / "sub" / "adir").mkdir()
        # A directory git can actually carry. Git tracks files, never
        # directories, so a directory is only in the repository when
        # something inside it is — see the empty-directory case below.
        (root / "sub" / "adir" / "kept.md").write_text("kept\n")
        (root / "sub" / "emptydir").mkdir()
        self.doc = root / "doc.md"
        # The guard only ever scans sources inside its repository (they come
        # from `git ls-files`), and the containment clamp holds every target
        # to that root. The fixture lives in a tempdir, so the scanned root
        # must be the tempdir for the same preconditions to hold here.
        self._real_root = guard.ROOT
        guard.ROOT = root
        # The fixture has to be a real repository, not just a directory that
        # looks like one. `resolves()` asks the git index what the repository
        # contains rather than asking the filesystem what happens to be lying
        # around, so a fixture with no index would answer "nothing exists" and
        # every case here would pass for the wrong reason.
        self._init_repo(root)

    @staticmethod
    def _init_repo(root: Path) -> None:
        run = lambda *a: subprocess.run(
            a, cwd=root, check=True, capture_output=True
        )
        run("git", "init", "-q")
        run("git", "add", "-A")

    def track(self, *relative: str) -> None:
        """Stage paths created after setUp, and drop the memoised index.

        `_index_paths` is cached per root for speed, so a case that creates a
        file mid-test has to invalidate it or the guard keeps answering from
        the index as it stood at setUp.
        """
        subprocess.run(
            ["git", "add", "-A", *relative], cwd=guard.ROOT, check=True,
            capture_output=True,
        )
        guard._index_paths.cache_clear()

    def tearDown(self):
        guard.ROOT = self._real_root
        guard._index_paths.cache_clear()
        self._dir.cleanup()

    def dangling(self, body: str) -> bool:
        self.doc.write_text(body + "\n")
        return bool(guard.dangling_in(self.doc))

    # -- resolution ---------------------------------------------------------

    def test_resolution_verdicts(self):
        for body, want, label in (
            ("[a](sub/real.md)", False, "existing file"),
            ("[a](sub/missing.md)", True, "missing file"),
            ("[a](sub/adir)", False, "existing directory"),
            ("[a](sub/real.md#h)", False, "fragment on a real path"),
            ("[a](sub/missing.md#h)", True, "fragment on a missing path"),
            ("[a](#local)", False, "bare anchor"),
            ("[a](https://example.com/x)", False, "external scheme"),
            ("[a](src/foo.rs:137)", False, "line citation"),
            ("[a](<sub/real.md>)", False, "angle-bracket form"),
            ("[a](routes/(app)/nope.md)", True, "balanced parens in target"),
        ):
            with self.subTest(label):
                self.assertEqual(self.dangling(body), want)

    # -- the repository, not the working tree ------------------------------

    def test_untracked_target_dangles_until_it_is_staged(self):
        """A file nobody has added is not in the repository.

        This is the hole that motivated resolving against the index: a target
        lying untracked in one working tree made its link resolve there and
        404 on every clone — the same checkout-dependent verdict the
        repo-root rule and the containment clamp already exist to prevent.
        Ten links in the tree were in exactly this state when it was found.
        """
        (guard.ROOT / "sub" / "untracked.md").write_text("new\n")
        guard._index_paths.cache_clear()
        self.assertTrue(self.dangling("[a](sub/untracked.md)"))
        # Staging is enough — `git ls-files` reads the index, so the ordinary
        # flow of writing a doc and its target then staging both keeps working
        # without waiting for a commit.
        self.track()
        self.assertFalse(self.dangling("[a](sub/untracked.md)"))

    def test_empty_directory_dangles_because_git_cannot_carry_one(self):
        """An empty directory exists locally and never survives a clone.

        Git tracks files, so there is no such thing as an empty directory in
        a repository. A link to one resolves for whoever created it and 404s
        for everyone else, which is precisely what this guard is for.
        """
        self.assertTrue(self.dangling("[a](sub/emptydir)"))
        self.assertFalse(self.dangling("[a](sub/adir)"))

    def test_case_mismatch_on_final_component_is_dangling(self):
        # macOS resolves it, Linux 404s it; the guard must catch it here.
        self.assertTrue(self.dangling("[a](sub/casefile.md)"))
        self.assertFalse(self.dangling("[a](sub/CaseFile.md)"))

    # -- code stripping (review round 1: the guard blanked 600 live lines) --

    def test_code_regions_are_not_links(self):
        for body, want, label in (
            ("```\n[a](sub/missing.md)\n```", False, "closed fence"),
            ("```rust\n[a](sub/missing.md)\n```", False, "fence with info string"),
            ("see `[a](sub/missing.md)` here", False, "inline span"),
            ("`x` then [a](sub/missing.md)", True, "link after a span"),
        ):
            with self.subTest(label):
                self.assertEqual(self.dangling(body), want)

    def test_inline_triple_backtick_span_does_not_blank_the_tail(self):
        # CommonMark: a backtick fence's info string cannot contain a
        # backtick, so this line is an inline span. The first tracker
        # treated it as a fence toggle and went blind to everything below —
        # 600 lines of pi-coding-engine-contract.md in production.
        self.assertTrue(self.dangling("``` `x` ``` prose\n[a](sub/missing.md)"))

    def test_fence_close_must_match_open_run(self):
        # A ```` fence contains ``` lines as content; only >= the opening
        # run closes. The link after the outer close must be visible.
        self.assertTrue(
            self.dangling(
                "````\n```\n[a](sub/missing.md)\n```\n````\n[b](sub/missing.md)"
            )
        )

    def test_unclosed_fence_blanks_to_eof_like_github_renders_it(self):
        self.assertFalse(self.dangling("```\n[a](sub/missing.md)\n``` trailing"))

    # -- containment (review rounds 2 and 3: machine-dependent verdicts) ----

    def test_os_absolute_target_dangles_everywhere(self):
        # This resolved on exactly one machine — the one whose checkout path
        # it hardcoded — and failed every other clone. 21 of these existed.
        self.assertTrue(
            self.dangling("[a](/Users/nobody/somewhere/checkout/README.md)")
        )

    def test_escape_above_the_root_dangles_regardless_of_what_is_there(self):
        # ROOT here is the tempdir; its parent certainly exists. Without the
        # containment clamp this verdict depends on the machine's layout.
        self.assertTrue(self.dangling("[a](/../)"))

    def test_leading_slash_resolves_from_the_scanned_root(self):
        # Repo-root resolution is how GitHub renders a leading slash: the
        # target is found from the root, wherever the source doc sits.
        self.assertFalse(self.dangling("[a](/sub/real.md)"))
        self.assertTrue(self.dangling("[a](/sub/missing.md)"))


if __name__ == "__main__":
    unittest.main()
