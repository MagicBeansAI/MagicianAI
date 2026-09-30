from __future__ import annotations

import importlib.util
from importlib.machinery import SourceFileLoader
import json
import pathlib
import unittest
from datetime import datetime, timedelta, timezone


SCRIPT = pathlib.Path(__file__).parents[1] / "bin" / "youtube-search"
LOADER = SourceFileLoader("youtube_search", str(SCRIPT))
SPEC = importlib.util.spec_from_loader("youtube_search", LOADER)
assert SPEC is not None
MODULE = importlib.util.module_from_spec(SPEC)
LOADER.exec_module(MODULE)


def playlist(*entries: dict) -> str:
    return json.dumps({
        "_type": "playlist",
        "id": "rust programming",
        "title": "rust programming",
        "playlist_count": len(entries),
        "entries": list(entries),
    })


def entry(**overrides) -> dict:
    base = {
        "id": "BpPEoZW5IiY",
        "title": "Learn Rust Programming",
        "url": "https://www.youtube.com/watch?v=BpPEoZW5IiY",
        "description": "A comprehensive Rust course.",
        "duration": 50350.0,
        "view_count": 1_205_650,
        "channel": "freeCodeCamp.org",
    }
    base.update(overrides)
    return base


class FakeCompletedProcess:
    def __init__(self, returncode: int = 0, stdout: str = "", stderr: str = "") -> None:
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr


class YouTubeBackendTestCase(unittest.TestCase):
    """Base that stubs the backend so no test reaches YouTube."""

    def run_with(self, process: FakeCompletedProcess):
        original_command = MODULE._yt_dlp_command
        original_run = MODULE.subprocess.run
        MODULE._yt_dlp_command = lambda: ["yt-dlp"]
        MODULE.subprocess.run = lambda *_args, **_kwargs: process
        try:
            return MODULE.search_playlist("rust programming", 6)
        finally:
            MODULE._yt_dlp_command = original_command
            MODULE.subprocess.run = original_run


class SearchPlaylistFailureSurfacingTests(YouTubeBackendTestCase):
    """The whole reason this asks for `-J` instead of `--dump-json`.

    yt-dlp exits 0 with empty stdout AND empty stderr both when a search
    legitimately matches nothing and when the search page is blocked. The line
    stream could not tell those apart and reported `status=ok, count=0` for
    both, which is the silent zero that took this skill dark.
    """

    def test_empty_stdout_with_no_diagnostic_is_an_error_not_an_empty_result(self) -> None:
        with self.assertRaises(MODULE.ProviderUnavailable) as caught:
            self.run_with(FakeCompletedProcess(returncode=0, stdout="", stderr=""))

        self.assertIn("no search result", str(caught.exception))

    def test_a_nonzero_exit_carries_the_backend_diagnostic(self) -> None:
        with self.assertRaises(MODULE.ProviderUnavailable) as caught:
            self.run_with(
                FakeCompletedProcess(returncode=1, stderr="ERROR: Unable to handle request")
            )

        self.assertIn("exited 1", str(caught.exception))
        self.assertIn("Unable to handle request", str(caught.exception))

    def test_non_json_stdout_raises(self) -> None:
        with self.assertRaises(MODULE.ProviderUnavailable) as caught:
            self.run_with(FakeCompletedProcess(stdout="<html>sign in to confirm</html>"))

        self.assertIn("not JSON", str(caught.exception))

    def test_json_that_is_not_a_search_playlist_raises(self) -> None:
        with self.assertRaises(MODULE.ProviderUnavailable):
            self.run_with(FakeCompletedProcess(stdout=json.dumps({"id": "x"})))

    def test_a_playlist_with_no_entries_is_a_legitimate_zero(self) -> None:
        """Receiving the playlist object proves the search ran; an empty
        `entries` inside it means YouTube genuinely had nothing."""
        result = self.run_with(FakeCompletedProcess(stdout=playlist()))

        self.assertEqual(result["entries"], [])


class GovernedPathContractTests(unittest.TestCase):
    """Pin that the backend is reachable from the governed environment.

    The defect this guards: the governed child receives a cleared environment
    whose PATH is assembled only from the directories that resolve the
    manifest's declared `bins`. This package declared just its own entry
    point, so the adapter ran under the OS interpreter with a PATH that never
    contained `skillshub/.venv/bin` — and BOTH routes to the backend were
    closed at once. `shutil.which("yt-dlp")` found nothing because the console
    script was not on that PATH, and `find_spec("yt_dlp")` found nothing
    either because the module lives in the environment the interpreter was
    not.

    What the lane reported — "youtube-search backend unavailable" — was the
    error contract working exactly as designed, over a backend the runtime had
    made unreachable. That distinction is the point: the message was true, and
    the fix belongs in the manifest rather than in the adapter's reporting.
    """

    SKILL = pathlib.Path(__file__).parents[1] / "SKILL.md"

    def declared_bin_blocks(self) -> list[list[str]]:
        """Every `bins:` list in the frontmatter, read by indentation so the
        tests carry no dependency the governed adapter does not need."""
        frontmatter = self.SKILL.read_text(encoding="utf-8").split("\n---\n", 1)[0]
        blocks: list[list[str]] = []
        lines = frontmatter.splitlines()
        for index, line in enumerate(lines):
            if line.strip() != "bins:":
                continue
            indent = len(line) - len(line.lstrip())
            entries: list[str] = []
            for candidate in lines[index + 1 :]:
                stripped = candidate.strip()
                if not stripped or stripped.startswith("#"):
                    continue
                if (
                    len(candidate) - len(candidate.lstrip()) < indent
                    or not stripped.startswith("- ")
                ):
                    break
                entries.append(stripped[2:].strip())
            blocks.append(entries)
        return blocks

    def test_the_interpreter_and_the_backend_are_both_declared(self) -> None:
        blocks = self.declared_bin_blocks()
        self.assertTrue(blocks, "the manifest declares no bins at all")
        for entries in blocks:
            self.assertIn("youtube-search", entries)
            for companion in ("python3", "yt-dlp"):
                self.assertIn(
                    companion,
                    entries,
                    f"a bins list omits `{companion}`; without it the governed "
                    "PATH cannot reach the search backend by either route",
                )

    def test_the_entry_point_is_named_because_more_than_one_binary_is(self) -> None:
        """`bins` is a SET, and the validator will not guess.

        `validate_requirements` in tool-runtime-core refuses a CLI contract
        with several `bins` and no exact `entrypoint`; the contract then fails
        validation, the loader drops the pack, and the tool answers `unknown
        inner-loop pack` in 0 ms. Declaring `python3` and `yt-dlp` without this
        line replaced a working search that reported a backend error with a
        skill that no longer existed.

        The set ordering is why the validator refuses rather than defaulting:
        the fallback it declines to apply is `bins.first()`, and over
        {python3, yt-dlp, youtube-search} that is `python3` — the interpreter,
        not the adapter.
        """
        frontmatter = self.SKILL.read_text(encoding="utf-8").split("\n---\n", 1)[0]
        entrypoints = [
            line.split(":", 1)[1].strip()
            for line in frontmatter.splitlines()
            if line.strip().startswith("entrypoint:")
        ]
        self.assertTrue(
            entrypoints,
            "a multi-binary contract declares no entrypoint, so the pack cannot load",
        )
        for entrypoint in entrypoints:
            self.assertEqual(entrypoint, "youtube-search")
        for entries in self.declared_bin_blocks():
            self.assertLess(
                sorted(entries)[0],
                "youtube-search",
                "the lexical fallback the validator refuses would not be the adapter, "
                "which is exactly why the entrypoint must be explicit",
            )

    def test_both_backend_routes_are_still_attempted(self) -> None:
        # The console script and the importable module are two independent
        # ways to the same backend. Declaring the bins fixes the PATH, but the
        # adapter must keep trying both, because a host may have installed
        # only one of them.
        source = (pathlib.Path(__file__).parents[1] / "bin" / "youtube-search").read_text(
            encoding="utf-8"
        )
        self.assertIn('shutil.which("yt-dlp")', source)
        self.assertIn('find_spec("yt_dlp")', source)

    def test_an_unreachable_backend_is_still_an_error_and_not_a_zero(self) -> None:
        original = MODULE._yt_dlp_command
        MODULE._yt_dlp_command = lambda: None
        try:
            with self.assertRaises(RuntimeError) as caught:
                MODULE.search_playlist("rust programming", 3)
        finally:
            MODULE._yt_dlp_command = original

        self.assertIn("backend unavailable", str(caught.exception))


class ProjectionTests(unittest.TestCase):
    def test_flat_entry_normalizes_to_the_governed_item_shape(self) -> None:
        items, _ = MODULE.project([entry()], 10, 365)

        self.assertEqual(len(items), 1)
        self.assertEqual(items[0]["source_native_id"], "BpPEoZW5IiY")
        self.assertEqual(items[0]["title"], "Learn Rust Programming")
        self.assertEqual(items[0]["url"], "https://www.youtube.com/watch?v=BpPEoZW5IiY")
        self.assertEqual(items[0]["author"], "freeCodeCamp.org")
        self.assertEqual(items[0]["container"], "freeCodeCamp.org")

    def test_absent_like_counts_are_omitted_rather_than_zeroed(self) -> None:
        """Flat search entries never carry a like count, and reporting zero
        likes for every video is a measurement nobody made."""
        items, _ = MODULE.project([entry()], 10, 365)

        self.assertEqual(items[0]["engagement"], {"duration_s": 50350, "views": 1_205_650})
        self.assertNotIn("likes", items[0]["engagement"])

    def test_undated_entries_report_the_window_as_unapplied(self) -> None:
        """A flat search almost never carries a publication time, so `days`
        cannot be enforced and must not pretend it was."""
        items, dated = MODULE.project([entry()], 10, 1)

        self.assertEqual(len(items), 1)
        self.assertFalse(dated)
        self.assertIsNone(items[0]["published_at"])

    def test_dated_entries_are_filtered_and_report_the_window_as_applied(self) -> None:
        now = datetime.now(timezone.utc)
        recent = int((now - timedelta(days=2)).timestamp())
        ancient = int((now - timedelta(days=900)).timestamp())

        items, dated = MODULE.project(
            [
                entry(id="recent", title="Recent", timestamp=recent),
                entry(id="ancient", title="Ancient", timestamp=ancient),
            ],
            10,
            30,
        )

        self.assertTrue(dated)
        self.assertEqual([item["title"] for item in items], ["Recent"])

    def test_release_timestamp_is_used_when_timestamp_is_absent(self) -> None:
        moment = int(datetime(2026, 8, 13, 12, tzinfo=timezone.utc).timestamp())

        items, dated = MODULE.project([entry(release_timestamp=moment)], 10, 100_000)

        self.assertTrue(dated)
        self.assertEqual(items[0]["published_at"], "2026-08-13T12:00:00Z")

    def test_limit_bounds_the_returned_list(self) -> None:
        items, _ = MODULE.project([entry(id=str(index)) for index in range(5)], 2, 365)

        self.assertEqual(len(items), 2)


if __name__ == "__main__":
    unittest.main()
