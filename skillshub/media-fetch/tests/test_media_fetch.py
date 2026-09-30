from __future__ import annotations

import importlib.util
from importlib.machinery import SourceFileLoader
import json
import os
import pathlib
import tempfile
import time
import unittest

SCRIPT = pathlib.Path(__file__).parents[1] / "bin" / "media-fetch"
LOADER = SourceFileLoader("media_fetch", str(SCRIPT))
SPEC = importlib.util.spec_from_loader("media_fetch", LOADER)
assert SPEC is not None
MODULE = importlib.util.module_from_spec(SPEC)
LOADER.exec_module(MODULE)


def load_fixture(name: str) -> dict:
    p = pathlib.Path(__file__).parent / "fixtures" / name
    return json.loads(p.read_text())


class ResolverTestCase(unittest.TestCase):
    def test_yt_dlp_command_honours_env_override(self):
        os.environ["YTDLP_BIN"] = "/custom/yt-dlp"
        try:
            self.assertEqual(MODULE._yt_dlp_command(), ["/custom/yt-dlp"])
        finally:
            del os.environ["YTDLP_BIN"]

    def test_ffmpeg_location_prefers_pack_local_bin(self):
        """The pack ships its own ffmpeg symlink; that must win over PATH.

        Driven against a temporary pack dir rather than the real one. The
        symlink is written by `make -C skillshub setup-media-fetch` and is
        gitignored, so asserting on the installed one tests whether this
        machine has run setup, not whether the resolver prefers the pack.
        """
        with tempfile.TemporaryDirectory() as tmp:
            pack_bin = pathlib.Path(tmp)
            (pack_bin / "ffmpeg").write_text("#!/bin/sh\n")
            original_pack_bin = MODULE.PACK_BIN
            original_which = MODULE.shutil.which
            MODULE.PACK_BIN = pack_bin
            MODULE.shutil.which = lambda name: "/somewhere/on/path/ffmpeg"
            try:
                self.assertEqual(MODULE._ffmpeg_location(), pack_bin)
            finally:
                MODULE.PACK_BIN = original_pack_bin
                MODULE.shutil.which = original_which

    def test_ffmpeg_location_falls_back_to_path_without_a_pack_binary(self):
        """No pack-local symlink is not an error; PATH is the documented fallback."""
        with tempfile.TemporaryDirectory() as tmp:
            original_pack_bin = MODULE.PACK_BIN
            original_which = MODULE.shutil.which
            MODULE.PACK_BIN = pathlib.Path(tmp)
            MODULE.shutil.which = lambda name: "/somewhere/on/path/ffmpeg"
            try:
                self.assertEqual(
                    MODULE._ffmpeg_location(), pathlib.Path("/somewhere/on/path")
                )
            finally:
                MODULE.PACK_BIN = original_pack_bin
                MODULE.shutil.which = original_which

    def test_ffmpeg_location_is_none_when_nothing_provides_it(self):
        """Callers branch on None; never invent a location that resolves nowhere."""
        with tempfile.TemporaryDirectory() as tmp:
            original_pack_bin = MODULE.PACK_BIN
            original_which = MODULE.shutil.which
            MODULE.PACK_BIN = pathlib.Path(tmp)
            MODULE.shutil.which = lambda name: None
            try:
                self.assertIsNone(MODULE._ffmpeg_location())
            finally:
                MODULE.PACK_BIN = original_pack_bin
                MODULE.shutil.which = original_which

    @unittest.skipUnless(
        (pathlib.Path(__file__).parents[1] / "bin" / "ffmpeg").exists(),
        "no pack-local ffmpeg installed; run `make -C skillshub setup-media-fetch`",
    )
    def test_installed_pack_ffmpeg_is_the_one_that_gets_used(self):
        """Where setup has run, confirm the real symlink is what resolves."""
        self.assertTrue(str(MODULE._ffmpeg_location()).endswith("media-fetch/bin"))


class ProbeTestCase(unittest.TestCase):
    def test_project_metadata_keeps_the_useful_fields(self):
        projected = MODULE.project_metadata(load_fixture("metadata_video.json"))
        self.assertEqual(projected["source_native_id"], "jNQXAC9IVRw")
        self.assertEqual(projected["extractor"], "youtube")
        self.assertEqual(projected["duration_s"], 19)
        self.assertIn("upload_date", projected)

    def test_project_metadata_resolves_the_real_upload_date(self):
        """The whole point of full extraction: flat search carries no date."""
        projected = MODULE.project_metadata(load_fixture("metadata_video.json"))
        self.assertIsNotNone(projected["upload_date"])
        self.assertRegex(projected["upload_date"], r"^\d{4}-\d{2}-\d{2}$")

    def test_project_metadata_omits_the_formats_firehose(self):
        """A raw extraction carries hundreds of format dicts; never return them."""
        projected = MODULE.project_metadata(load_fixture("metadata_video.json"))
        self.assertNotIn("formats", projected)
        self.assertNotIn("automatic_captions", projected)

    def test_container_is_the_media_container_format_not_a_second_author(self):
        """`container` must report yt-dlp's `ext` (the file format: webm,
        mp4, ...), not channel-or-uploader — which just duplicated `author`
        under a different key. The fixture's uploader and channel are both
        "jawed", so this is the only way to prove the fields are distinct."""
        info = load_fixture("metadata_video.json")
        self.assertEqual(info["uploader"], info["channel"])  # the duplicate-prone shape
        projected = MODULE.project_metadata(info)
        self.assertEqual(projected["container"], "webm")
        self.assertEqual(projected["author"], "jawed")
        self.assertNotEqual(projected["container"], projected["author"])

    def test_container_is_none_when_ext_is_absent(self):
        self.assertIsNone(MODULE.project_metadata({"id": "x"})["container"])


class FakeCompletedProcess:
    def __init__(self, returncode=0, stdout="", stderr=""):
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr


class ProbeContractTestCase(unittest.TestCase):
    """probe() is the positive-signal gate; these are its failure modes.

    Measured on the live provider: yt-dlp exits 0 with empty stdout AND empty
    stderr both when an item genuinely has nothing and when its extractor is
    broken. An absent object must therefore be an error, never a success.
    """

    def probe_with(self, process):
        """Stub both the backend lookup and the call, per the youtube-search
        precedent — this test environment has no yt-dlp on PATH, so probe()
        would otherwise fail at backend detection before ever running."""
        original_command = MODULE._yt_dlp_command
        original_run = MODULE.subprocess.run
        MODULE._yt_dlp_command = lambda: ["yt-dlp"]
        MODULE.subprocess.run = lambda *a, **k: process
        try:
            return MODULE.probe("https://example.com/x")
        finally:
            MODULE._yt_dlp_command = original_command
            MODULE.subprocess.run = original_run

    def test_exit_zero_with_empty_stdout_is_an_error_not_a_success(self):
        """The exact shape of the bug youtube-search shipped."""
        with self.assertRaises(MODULE.ProviderUnavailable):
            self.probe_with(FakeCompletedProcess(returncode=0, stdout="", stderr=""))

    def test_nonzero_exit_carries_the_stderr_diagnostic(self):
        with self.assertRaises(MODULE.ProviderUnavailable) as ctx:
            self.probe_with(FakeCompletedProcess(returncode=1, stderr="ERROR: unsupported URL"))
        self.assertIn("unsupported URL", str(ctx.exception))

    def test_empty_stderr_still_produces_a_usable_message(self):
        """Both streams empty is the worst case; the error must still say something."""
        with self.assertRaises(MODULE.ProviderUnavailable) as ctx:
            self.probe_with(FakeCompletedProcess(returncode=1, stderr=""))
        self.assertTrue(str(ctx.exception).strip())

    def test_non_json_stdout_is_an_error(self):
        with self.assertRaises(MODULE.ProviderUnavailable):
            self.probe_with(FakeCompletedProcess(stdout="<html>sign in to confirm</html>"))

    def test_json_that_is_not_an_object_is_an_error(self):
        with self.assertRaises(MODULE.ProviderUnavailable):
            self.probe_with(FakeCompletedProcess(stdout="[1, 2, 3]"))

    def test_a_real_info_object_is_returned(self):
        info = self.probe_with(FakeCompletedProcess(stdout=json.dumps({"id": "x", "duration": 5})))
        self.assertEqual(info["id"], "x")


class ProbeFlatPlaylistTestCase(unittest.TestCase):
    """The documented playlist guard (SKILL.md: "Playlists refused ...")
    is dead without `--flat-playlist`: `probe()` used to fully resolve
    every entry of a playlist before `reject_playlists()` ever saw the
    result. Measured live against a real 100-item playlist: without
    `--flat-playlist` the probe did not finish in 95s; with it, ~1.4s.
    Measured the other direction too — for a single-video URL (this pack's
    own canary), `--flat-playlist` is a no-op: every field
    `project_metadata` reads came back identical with and without it, so
    this flag cannot silently degrade the `metadata` action.
    """

    def test_probe_command_requests_flat_playlist(self):
        captured = {}
        original_command = MODULE._yt_dlp_command
        original_run = MODULE.subprocess.run
        MODULE._yt_dlp_command = lambda: ["yt-dlp"]

        def fake_run(cmd, **kwargs):
            captured["cmd"] = cmd
            return FakeCompletedProcess(stdout=json.dumps({"id": "x", "duration": 5}))

        MODULE.subprocess.run = fake_run
        try:
            MODULE.probe("https://example.com/x")
        finally:
            MODULE._yt_dlp_command = original_command
            MODULE.subprocess.run = original_run
        self.assertIn("--flat-playlist", captured["cmd"])
        self.assertIn("--simulate", captured["cmd"])
        self.assertIn("-J", captured["cmd"])


@unittest.skipUnless(
    os.environ.get("MEDIA_FETCH_LIVE_TESTS"),
    "hits the real yt-dlp provider over the network; opt in with MEDIA_FETCH_LIVE_TESTS=1",
)
class LivePlaylistGuardTestCase(unittest.TestCase):
    """Live verification of FIX E against the real provider, not a fixture.

    Not run by default: this needs a real `yt-dlp` binary and network
    access, which the rest of this suite deliberately never depends on.
    Measured manually during the fix: this exact playlist (100 uploads)
    timed out past 95s without `--flat-playlist` and resolved in ~1.4s
    with it — this test pins that behaviour for anyone re-verifying live.
    """

    PLAYLIST_URL = "https://www.youtube.com/playlist?list=UUX6OQ3DkcsbYNE6H8uQQuVA"

    def test_a_real_large_playlist_is_refused_quickly(self):
        started = time.monotonic()
        info = MODULE.probe(self.PLAYLIST_URL, timeout=30)
        elapsed = time.monotonic() - started
        self.assertLess(
            elapsed, 30, "probe() must resolve a real playlist quickly via --flat-playlist"
        )
        with self.assertRaises(MODULE.CapExceeded) as ctx:
            MODULE.reject_playlists(info)
        self.assertRegex(str(ctx.exception), r"\d+ item")


class PlaylistRefusalTestCase(unittest.TestCase):
    def test_playlist_is_refused_with_a_count(self):
        """A playlist would blow every cap; fail fast and say how many items."""
        info = load_fixture("metadata_playlist.json")
        with self.assertRaises(MODULE.CapExceeded) as ctx:
            MODULE.reject_playlists(info)
        self.assertIn("playlist", str(ctx.exception).lower())
        self.assertRegex(str(ctx.exception), r"\d+ item")

    def test_single_video_is_not_refused(self):
        MODULE.reject_playlists(load_fixture("metadata_video.json"))  # must not raise


class CapTestCase(unittest.TestCase):
    def test_duration_over_cap_names_cap_and_actual(self):
        """The error must carry both numbers so the agent can adapt, not guess."""
        with self.assertRaises(MODULE.CapExceeded) as ctx:
            MODULE.enforce_caps({"duration": 50350, "id": "x"}, 7200, 500)
        msg = str(ctx.exception)
        self.assertIn("50350", msg)
        self.assertIn("7200", msg)

    def test_filesize_cap_reads_both_field_spellings(self):
        """Measured: YouTube sets filesize_approx, archive.org sets filesize."""
        with self.assertRaises(MODULE.CapExceeded):
            MODULE.enforce_caps({"filesize": 900 * 1024 * 1024}, 7200, 500)
        with self.assertRaises(MODULE.CapExceeded):
            MODULE.enforce_caps({"filesize_approx": 900 * 1024 * 1024}, 7200, 500)

    def test_within_caps_passes(self):
        MODULE.enforce_caps({"duration": 19, "id": "x"}, 7200, 500)

    def test_unknown_duration_does_not_block(self):
        """A live stream or generic URL reports none; do not invent one."""
        MODULE.enforce_caps({"id": "x"}, 7200, 500)

    def test_probe_blind_url_still_gets_a_runtime_guard(self):
        """Measured: a bare .mp4 via the generic extractor reports nothing.

        Both declared caps are inert for that shape, so the download itself
        must carry a limit.
        """
        MODULE.enforce_caps({"id": "x"}, 7200, 500)  # probe cannot refuse it
        self.assertEqual(MODULE.runtime_size_guard(500), ["--max-filesize", "500M"])


class ResolveNumericParamTestCase(unittest.TestCase):
    """A bad numeric cap must be a bad request (ValueError), never an outage
    (ProviderUnavailable) or an opaque TypeError from a bare int(None)."""

    def test_absent_key_uses_the_default(self):
        self.assertEqual(MODULE.resolve_numeric_param({}, "max_duration_s", 7200), 7200)

    def test_explicit_null_uses_the_default(self):
        """A thorough caller that emits every optional field sends JSON null
        for anything left at its default; that must not become int(None)."""
        self.assertEqual(
            MODULE.resolve_numeric_param({"max_duration_s": None}, "max_duration_s", 7200), 7200
        )

    def test_a_valid_positive_value_passes_through(self):
        self.assertEqual(
            MODULE.resolve_numeric_param({"max_filesize_mb": 250}, "max_filesize_mb", 500), 250
        )

    def test_negative_value_is_rejected_as_bad_input(self):
        """Measured: max_filesize_mb: -1 reaches yt-dlp as --max-filesize -1M,
        which it rejects outright — a bad request, not something a retry
        could ever fix."""
        with self.assertRaises(ValueError):
            MODULE.resolve_numeric_param({"max_filesize_mb": -1}, "max_filesize_mb", 500)

    def test_zero_is_rejected_as_bad_input(self):
        with self.assertRaises(ValueError):
            MODULE.resolve_numeric_param({"max_duration_s": 0}, "max_duration_s", 7200)

    def test_non_numeric_string_is_rejected_as_bad_input(self):
        with self.assertRaises(ValueError):
            MODULE.resolve_numeric_param({"max_duration_s": "soon"}, "max_duration_s", 7200)

    def test_a_bool_is_rejected_as_bad_input(self):
        """isinstance(True, int) is True in Python; must not sneak through."""
        with self.assertRaises(ValueError):
            MODULE.resolve_numeric_param({"max_duration_s": True}, "max_duration_s", 7200)


class FractionalCapTestCase(unittest.TestCase):
    """A cap that truncates to 0 is not a usable cap.

    Measured: `max_filesize_mb: 0.5` passed the old `<= 0` check (0.5 is
    positive) and was then handed to a bare `int(0.5)`, truncating to `0`.
    `runtime_size_guard(0)` then emits `--max-filesize 0M`, aborting every
    download, and `enforce_caps` trips on every measured size too — reported
    as "exceeds max_filesize_mb 0", never mentioning the caller's actual
    0.5. Every bad value here must be rejected before truncation, and the
    error must echo what the caller actually sent, not the truncated 0.
    """

    KEYS_AND_DEFAULTS = (("max_filesize_mb", 500), ("max_duration_s", 7200))
    BAD_VALUES = (0.5, 0.99, 0, -1)

    def test_bad_values_are_rejected_for_every_cap_and_echo_the_original(self):
        for key, default in self.KEYS_AND_DEFAULTS:
            for bad_value in self.BAD_VALUES:
                with self.subTest(key=key, bad_value=bad_value):
                    with self.assertRaises(ValueError) as ctx:
                        MODULE.resolve_numeric_param({key: bad_value}, key, default)
                    message = str(ctx.exception)
                    # The caller's real input must be echoed verbatim (via
                    # `repr`, matching the implementation) — never the
                    # truncated `int()` value. `repr(0.5)` is "0.5", which is
                    # distinct from `repr(0)` == "0", so this also proves 0.5
                    # is not silently reported as if the caller had sent 0.
                    self.assertIn(repr(bad_value), message)

    def test_a_whole_number_float_is_still_accepted(self):
        """500.0 is a whole number spelled as a float; only the fractional
        part is the problem, not the type."""
        self.assertEqual(
            MODULE.resolve_numeric_param({"max_filesize_mb": 250.0}, "max_filesize_mb", 500), 250
        )


class DispatchRejectsBadCapsTestCase(unittest.TestCase):
    """End-to-end: dispatch must report a bad cap as ValueError before ever
    probing the URL, not as CapExceeded, ProviderUnavailable, or TypeError."""

    def _dispatch_without_probing(self, request):
        def fail_if_called(*a, **k):
            self.fail("probe() must not run for a request with a bad cap")

        original_probe = MODULE.probe
        MODULE.probe = fail_if_called
        try:
            return MODULE.dispatch("metadata", {**request, "url": "https://example.com/x"})
        finally:
            MODULE.probe = original_probe

    def test_negative_max_filesize_mb_is_a_bad_request_not_an_outage(self):
        env = self._dispatch_without_probing({"max_filesize_mb": -1})
        self.assertEqual(env["status"], "failed")
        self.assertEqual(env["error"]["kind"], "ValueError")

    def test_explicit_null_max_duration_s_does_not_raise_typeerror(self):
        original_probe = MODULE.probe
        MODULE.probe = lambda url, timeout=90: {"id": "x", "duration": 5}
        try:
            env = MODULE.dispatch(
                "metadata", {"url": "https://example.com/x", "max_duration_s": None}
            )
        finally:
            MODULE.probe = original_probe
        self.assertEqual(env["status"], "ok")

    def test_non_numeric_max_duration_s_is_a_bad_request(self):
        env = self._dispatch_without_probing({"max_duration_s": "soon"})
        self.assertEqual(env["status"], "failed")
        self.assertEqual(env["error"]["kind"], "ValueError")


class LiveStreamTestCase(unittest.TestCase):
    def test_live_stream_is_refused(self):
        with self.assertRaises(MODULE.CapExceeded) as ctx:
            MODULE.reject_live({"id": "x", "is_live": True})
        self.assertIn("live", str(ctx.exception).lower())

    def test_finished_broadcast_is_allowed(self):
        """was_live is a recording with a real duration; only is_live is endless."""
        MODULE.reject_live({"id": "x", "is_live": False, "was_live": True})

    def test_ordinary_item_is_allowed(self):
        MODULE.reject_live({"id": "x", "duration": 19})

    def test_live_has_no_duration_so_caps_cannot_catch_it(self):
        """Documents why this guard exists separately from enforce_caps."""
        MODULE.enforce_caps({"id": "x", "is_live": True}, 7200, 500)  # passes!


class FlattenTestCase(unittest.TestCase):
    def _doc(self):
        p = pathlib.Path(__file__).parent / "fixtures" / "captions.json3"
        return json.loads(p.read_text())

    def test_flatten_joins_segments_into_text(self):
        self.assertIn("hello there world", MODULE.flatten_captions(self._doc()))

    def test_flatten_groups_into_timestamped_paragraphs(self):
        text = MODULE.flatten_captions(self._doc())
        self.assertIn("[00:00:00]", text)
        self.assertIn("[00:00:31]", text)

    def test_flatten_drops_whitespace_only_events(self):
        self.assertNotIn("\n\n\n", MODULE.flatten_captions(self._doc()))

    def test_flatten_handles_an_empty_document(self):
        self.assertEqual(MODULE.flatten_captions({"events": []}), "")

    def test_flatten_handles_a_missing_events_key(self):
        self.assertEqual(MODULE.flatten_captions({}), "")


class NoCaptionsTestCase(unittest.TestCase):
    def test_absent_caption_file_raises_no_captions(self):
        import tempfile
        with tempfile.TemporaryDirectory() as tmp:
            with self.assertRaises(MODULE.NoCaptionsAvailable):
                MODULE.require_caption_file(pathlib.Path(tmp), "en")

    def test_no_captions_is_not_a_provider_outage(self):
        """Different next move: transcribe audio, versus retry later."""
        self.assertFalse(issubclass(MODULE.NoCaptionsAvailable, MODULE.ProviderUnavailable))

    def test_no_captions_message_suggests_the_audio_fallback(self):
        import tempfile
        with tempfile.TemporaryDirectory() as tmp:
            with self.assertRaises(MODULE.NoCaptionsAvailable) as ctx:
                MODULE.require_caption_file(pathlib.Path(tmp), "en")
        self.assertIn("audio", str(ctx.exception).lower())

    def test_present_caption_file_is_returned(self):
        import tempfile
        with tempfile.TemporaryDirectory() as tmp:
            f = pathlib.Path(tmp) / "vid.en.json3"
            f.write_text('{"events":[]}')
            self.assertEqual(MODULE.require_caption_file(pathlib.Path(tmp), "en"), f)


class EmptyTranscriptTestCase(unittest.TestCase):
    """`require_caption_file` only proves a file appeared; it does not prove
    the file has content. A track with `events: []` (or events that flatten
    to no real text) must not be reported as a successful, empty transcript."""

    def test_a_track_with_no_events_is_no_captions_available(self):
        with self.assertRaises(MODULE.NoCaptionsAvailable):
            MODULE.require_transcript_has_words({"events": []}, "en")

    def test_a_track_whose_events_are_all_whitespace_is_no_captions_available(self):
        doc = {"events": [{"tStartMs": 0, "segs": [{"utf8": "   \n"}]}]}
        with self.assertRaises(MODULE.NoCaptionsAvailable):
            MODULE.require_transcript_has_words(doc, "en")

    def test_a_track_with_real_words_passes(self):
        doc = {"events": [{"tStartMs": 0, "segs": [{"utf8": "hello"}]}]}
        MODULE.require_transcript_has_words(doc, "en")  # must not raise

    def test_empty_track_is_not_a_provider_outage(self):
        """Same next move as any other captionless item: fetch audio."""
        with self.assertRaises(MODULE.NoCaptionsAvailable):
            MODULE.require_transcript_has_words({"events": []}, "en")


class LeadingDiagnosticLineTestCase(unittest.TestCase):
    """FIX 6 reordered only `classify_download_failure`'s CapExceeded branch
    to lead with the decision-relevant line ahead of a long URL preamble.
    Its ProviderUnavailable sibling and `classify_transcript_failure` had
    the identical flaw. `_leading_diagnostic_line` is the one helper now
    shared by all three call sites — these tests exercise it directly and
    through both of the previously-unfixed siblings."""

    # Comfortably over `build_envelope`'s 200-char `reason` truncation, so a
    # classifier that still embeds raw, unordered output would lose the
    # ERROR text entirely — reproducing the reviewer's exact repro shape.
    LONG_URL_PREAMBLE = "[generic] Extracting URL: https://example.com/" + "a" * 150 + "/video\n"

    def test_helper_prefers_the_marker_line_over_a_long_preamble(self):
        output = self.LONG_URL_PREAMBLE + "ERROR: HTTP Error 503: Service Unavailable\n"
        self.assertEqual(
            MODULE._leading_diagnostic_line(output, MODULE.YTDLP_ERROR_MARKER),
            "ERROR: HTTP Error 503: Service Unavailable",
        )

    def test_helper_falls_back_to_full_output_when_marker_absent(self):
        self.assertEqual(MODULE._leading_diagnostic_line("plain failure", "ERROR:"), "plain failure")

    def test_helper_falls_back_to_empty_string_for_empty_output(self):
        self.assertEqual(MODULE._leading_diagnostic_line("", "ERROR:"), "")

    def test_transcript_failure_reason_survives_truncation_behind_a_long_url(self):
        """Sibling #2 left unreordered by FIX 6."""
        output = (
            self.LONG_URL_PREAMBLE
            + "ERROR: unable to download video subtitles: HTTP Error 503: Service Unavailable\n"
        )
        with self.assertRaises(MODULE.ProviderUnavailable) as ctx:
            MODULE.classify_transcript_failure(1, output, "en")
        reason = str(ctx.exception)[:200]  # mirrors build_envelope's truncation
        self.assertIn("503", reason)
        self.assertIn("Service Unavailable", reason)

    def test_download_failure_no_artifact_reason_survives_truncation_behind_a_long_url(self):
        """Sibling #1 left unreordered by FIX 6: classify_download_failure's
        ProviderUnavailable branch (no --max-filesize abort present)."""
        output = self.LONG_URL_PREAMBLE + "ERROR: HTTP Error 503: Service Unavailable\n"
        with self.assertRaises(MODULE.ProviderUnavailable) as ctx:
            MODULE.classify_download_failure(output, artifacts=[])
        reason = str(ctx.exception)[:200]  # mirrors build_envelope's truncation
        self.assertIn("503", reason)
        self.assertIn("Service Unavailable", reason)


class TranscriptFailureClassificationTestCase(unittest.TestCase):
    """`classify_transcript_failure` is the transcript-side equivalent of
    `classify_download_failure`: a missing caption file is ambiguous, and
    only the process's own exit code / ERROR marker can tell an outage
    apart from a genuinely captionless item."""

    def test_nonzero_exit_is_an_outage_even_with_no_error_marker(self):
        with self.assertRaises(MODULE.ProviderUnavailable):
            MODULE.classify_transcript_failure(1, "", "en")

    def test_error_marker_on_exit_zero_is_still_an_outage(self):
        """yt-dlp does not always propagate a subtitle-fetch failure through
        its exit code; the ERROR: marker is the second positive signal."""
        with self.assertRaises(MODULE.ProviderUnavailable):
            MODULE.classify_transcript_failure(0, "ERROR: giving up after 3 retries", "en")

    def test_the_verified_503_reproduction_is_an_outage_not_no_captions(self):
        """The reviewer's exact repro: returncode=1 with an HTTP 503 line."""
        with self.assertRaises(MODULE.ProviderUnavailable) as ctx:
            MODULE.classify_transcript_failure(
                1, "ERROR: unable to download video subtitles: HTTP Error 503: Service Unavailable", "en"
            )
        self.assertIn("503", str(ctx.exception))

    def test_clean_exit_with_no_marker_is_genuinely_no_captions(self):
        with self.assertRaises(MODULE.NoCaptionsAvailable):
            MODULE.classify_transcript_failure(0, "[info] there are no subtitles for this video", "en")


class RunTranscriptTestCase(unittest.TestCase):
    """`_run_transcript` had zero direct coverage: mocking subprocess.run to
    fabricate a failure used to still report `NoCaptionsAvailable` because
    the subprocess result was discarded entirely."""

    def _run_with(self, process, request=None):
        original_command = MODULE._yt_dlp_command
        original_run = MODULE.subprocess.run
        MODULE._yt_dlp_command = lambda: ["yt-dlp"]
        MODULE.subprocess.run = lambda *a, **k: process
        try:
            return MODULE._run_transcript(
                {"id": "x", "duration": 19}, request or {}, "https://example.com/x",
                max_filesize_mb=MODULE.MAX_FILESIZE_MB,
            )
        finally:
            MODULE._yt_dlp_command = original_command
            MODULE.subprocess.run = original_run

    def test_a_failed_download_with_captions_known_to_exist_is_an_outage(self):
        """The reviewer's repro: HTTP 503 during the caption fetch must not
        be reported as NoCaptionsAvailable, even though captions genuinely
        exist for this item."""
        with tempfile.TemporaryDirectory() as tmp:
            process = FakeCompletedProcess(
                returncode=1,
                stderr="ERROR: unable to download video subtitles: HTTP Error 503: Service Unavailable",
            )
            with self.assertRaises(MODULE.ProviderUnavailable):
                self._run_with(process, {"output_dir": tmp})

    def test_a_genuinely_captionless_item_is_no_captions_available(self):
        with tempfile.TemporaryDirectory() as tmp:
            process = FakeCompletedProcess(returncode=0, stdout="", stderr="")
            with self.assertRaises(MODULE.NoCaptionsAvailable):
                self._run_with(process, {"output_dir": tmp})

    def test_the_happy_path_writes_a_transcript(self):
        doc = {"events": [{"tStartMs": 0, "segs": [{"utf8": "hello there"}]}]}

        def fake_run(cmd, **kwargs):
            out_dir = pathlib.Path(cmd[cmd.index("-o") + 1]).parent
            (out_dir / "x.en.json3").write_text(json.dumps(doc), encoding="utf-8")
            return FakeCompletedProcess(returncode=0, stdout="", stderr="")

        original_command = MODULE._yt_dlp_command
        original_run = MODULE.subprocess.run
        MODULE._yt_dlp_command = lambda: ["yt-dlp"]
        MODULE.subprocess.run = fake_run
        try:
            with tempfile.TemporaryDirectory() as tmp:
                result = MODULE._run_transcript(
                    {"id": "x", "duration": 19}, {"output_dir": tmp}, "https://example.com/x",
                    max_filesize_mb=MODULE.MAX_FILESIZE_MB,
                )
        finally:
            MODULE._yt_dlp_command = original_command
            MODULE.subprocess.run = original_run
        self.assertIn("hello there", result["preview"])
        self.assertEqual(result["words"], 2)

    def test_the_raw_json3_is_removed_once_flattened_but_the_txt_survives(self):
        """The raw caption file is an intermediate, already fully parsed and
        flattened into the .txt deliverable; nothing should own cleaning it
        up but this call, since nothing else ever will."""
        doc = {"events": [{"tStartMs": 0, "segs": [{"utf8": "hello there"}]}]}
        raw_path_holder = {}

        def fake_run(cmd, **kwargs):
            out_dir = pathlib.Path(cmd[cmd.index("-o") + 1]).parent
            raw = out_dir / "x.en.json3"
            raw.write_text(json.dumps(doc), encoding="utf-8")
            raw_path_holder["path"] = raw
            return FakeCompletedProcess(returncode=0, stdout="", stderr="")

        original_command = MODULE._yt_dlp_command
        original_run = MODULE.subprocess.run
        MODULE._yt_dlp_command = lambda: ["yt-dlp"]
        MODULE.subprocess.run = fake_run
        try:
            with tempfile.TemporaryDirectory() as tmp:
                result = MODULE._run_transcript(
                    {"id": "x", "duration": 19}, {"output_dir": tmp}, "https://example.com/x",
                    max_filesize_mb=MODULE.MAX_FILESIZE_MB,
                )
                self.assertFalse(raw_path_holder["path"].exists(), "raw .json3 must be removed")
                self.assertTrue(pathlib.Path(result["path"]).is_file(), "the .txt deliverable must survive")
        finally:
            MODULE._yt_dlp_command = original_command
            MODULE.subprocess.run = original_run

    def test_an_empty_caption_track_on_disk_is_no_captions_available(self):
        """A file appeared (require_caption_file is satisfied) but it carries
        no real events — must not report status: ok with words: 0."""

        def fake_run(cmd, **kwargs):
            out_dir = pathlib.Path(cmd[cmd.index("-o") + 1]).parent
            (out_dir / "x.en.json3").write_text(json.dumps({"events": []}), encoding="utf-8")
            return FakeCompletedProcess(returncode=0, stdout="", stderr="")

        original_command = MODULE._yt_dlp_command
        original_run = MODULE.subprocess.run
        MODULE._yt_dlp_command = lambda: ["yt-dlp"]
        MODULE.subprocess.run = fake_run
        try:
            with tempfile.TemporaryDirectory() as tmp:
                with self.assertRaises(MODULE.NoCaptionsAvailable):
                    MODULE._run_transcript(
                        {"id": "x", "duration": 19}, {"output_dir": tmp}, "https://example.com/x",
                        max_filesize_mb=MODULE.MAX_FILESIZE_MB,
                    )
        finally:
            MODULE._yt_dlp_command = original_command
            MODULE.subprocess.run = original_run

    def test_an_empty_caption_track_still_removes_the_raw_json3(self):
        """FIX 3's empty-track guard (`require_transcript_has_words`) used to
        run before `caption_path.unlink()`, so raising `NoCaptionsAvailable`
        orphaned the raw `.json3` on disk. Reviewer confirmed by
        monkeypatching an empty-events `.json3`: the file still existed
        after the exception. The cleanup must be unconditional once the
        file has been read, not gated on a successful transcript."""
        raw_path_holder = {}

        def fake_run(cmd, **kwargs):
            out_dir = pathlib.Path(cmd[cmd.index("-o") + 1]).parent
            raw = out_dir / "x.en.json3"
            raw.write_text(json.dumps({"events": []}), encoding="utf-8")
            raw_path_holder["path"] = raw
            return FakeCompletedProcess(returncode=0, stdout="", stderr="")

        original_command = MODULE._yt_dlp_command
        original_run = MODULE.subprocess.run
        MODULE._yt_dlp_command = lambda: ["yt-dlp"]
        MODULE.subprocess.run = fake_run
        try:
            with tempfile.TemporaryDirectory() as tmp:
                with self.assertRaises(MODULE.NoCaptionsAvailable):
                    MODULE._run_transcript(
                        {"id": "x", "duration": 19}, {"output_dir": tmp}, "https://example.com/x",
                        max_filesize_mb=MODULE.MAX_FILESIZE_MB,
                    )
                self.assertFalse(
                    raw_path_holder["path"].exists(),
                    "the raw .json3 must not be orphaned when the empty-track guard fires",
                )
        finally:
            MODULE._yt_dlp_command = original_command
            MODULE.subprocess.run = original_run

    def test_transcript_download_does_not_silence_the_error_marker(self):
        """Same --quiet-hides-the-marker bug already fixed for audio/video."""
        captured = {}

        def fake_run(cmd, **kwargs):
            captured["cmd"] = cmd
            return FakeCompletedProcess(returncode=0, stdout="", stderr="")

        original_command = MODULE._yt_dlp_command
        original_run = MODULE.subprocess.run
        MODULE._yt_dlp_command = lambda: ["yt-dlp"]
        MODULE.subprocess.run = fake_run
        try:
            with tempfile.TemporaryDirectory() as tmp:
                with self.assertRaises(MODULE.NoCaptionsAvailable):
                    MODULE._run_transcript(
                        {"id": "x"}, {"output_dir": tmp}, "https://example.com/x",
                        max_filesize_mb=MODULE.MAX_FILESIZE_MB,
                    )
        finally:
            MODULE._yt_dlp_command = original_command
            MODULE.subprocess.run = original_run
        self.assertNotIn("--quiet", captured["cmd"])
        self.assertIn("--no-progress", captured["cmd"])


class WriteTranscriptTestCase(unittest.TestCase):
    def _doc(self):
        p = pathlib.Path(__file__).parent / "fixtures" / "captions.json3"
        return json.loads(p.read_text())

    def test_writes_a_file_and_returns_a_manifest(self):
        import tempfile
        with tempfile.TemporaryDirectory() as tmp:
            r = MODULE.write_transcript(
                info={"id": "abc", "title": "A talk", "duration": 61},
                doc=self._doc(), out_dir=pathlib.Path(tmp), language="en", kind="auto")
            self.assertTrue(pathlib.Path(r["path"]).is_file())
            self.assertGreater(r["words"], 0)
            self.assertEqual(r["kind"], "auto")
            self.assertEqual(r["language"], "en")
            self.assertIn("hello there world", r["preview"])

    def test_the_file_on_disk_is_complete_not_truncated(self):
        """The preview is a pointer; the file must hold everything."""
        import tempfile
        doc = self._doc()
        with tempfile.TemporaryDirectory() as tmp:
            r = MODULE.write_transcript(
                info={"id": "abc", "title": "t", "duration": 61},
                doc=doc, out_dir=pathlib.Path(tmp), language="en", kind="auto")
            self.assertEqual(pathlib.Path(r["path"]).read_text(), MODULE.flatten_captions(doc))

    def test_preview_is_capped_but_file_is_not(self):
        """A long transcript must still write in full."""
        import tempfile
        doc = {"events": [
            {"tStartMs": i * 1000, "segs": [{"utf8": f"word{i} "}]} for i in range(2000)
        ]}
        with tempfile.TemporaryDirectory() as tmp:
            r = MODULE.write_transcript(
                info={"id": "long", "duration": 2000},
                doc=doc, out_dir=pathlib.Path(tmp), language="en", kind="manual")
            self.assertEqual(len(r["preview"].split()), MODULE.PREVIEW_WORDS)
            self.assertEqual(r["words"], 2000)
            self.assertGreater(len(pathlib.Path(r["path"]).read_text()), len(r["preview"]))


class AudioArgsTestCase(unittest.TestCase):
    def test_selector_is_native_and_never_transcodes(self):
        args = MODULE.audio_args("/tmp/x.%(ext)s")
        joined = " ".join(args)
        self.assertIn("bestaudio", joined)
        self.assertNotIn("--extract-audio", args)
        self.assertNotIn("--audio-format", args)
        self.assertNotIn("--recode-video", args)

    def test_audio_requires_no_ffmpeg(self):
        """Measured: bestaudio downloads natively with ffmpeg off PATH."""
        self.assertFalse(MODULE.action_requires_ffmpeg("audio"))

    def test_audio_args_carry_the_runtime_size_guard(self):
        self.assertIn("--max-filesize", MODULE.audio_args("/tmp/x.%(ext)s"))


class SizeAbortTestCase(unittest.TestCase):
    ABORT = ("[download] File is larger than max-filesize "
             "(332243668 bytes > 1048576 bytes). Aborting.")

    def test_size_abort_is_detected(self):
        self.assertTrue(MODULE.was_size_aborted(self.ABORT))

    def test_normal_output_is_not_a_size_abort(self):
        self.assertFalse(MODULE.was_size_aborted("[download] 100% of 246.27KiB"))

    def test_size_abort_raises_cap_exceeded(self):
        with self.assertRaises(MODULE.CapExceeded):
            MODULE.classify_download_failure(self.ABORT, artifacts=[])

    def test_size_abort_message_carries_both_numbers(self):
        with self.assertRaises(MODULE.CapExceeded) as ctx:
            MODULE.classify_download_failure(self.ABORT, artifacts=[])
        self.assertIn("332243668", str(ctx.exception))
        self.assertIn("1048576", str(ctx.exception))

    def test_other_failure_is_an_outage(self):
        with self.assertRaises(MODULE.ProviderUnavailable):
            MODULE.classify_download_failure("some other failure", artifacts=[])

    def test_cap_exceeded_is_not_a_provider_outage(self):
        self.assertFalse(issubclass(MODULE.CapExceeded, MODULE.ProviderUnavailable))

    def test_byte_counts_survive_200_char_truncation_behind_a_long_url(self):
        """build_envelope truncates `reason` to 200 chars. yt-dlp's own
        output leads with the URL before the byte-count line; a ~145-char
        URL used to push the counts out of `reason` entirely. The counts
        must now lead the message so they survive the truncation."""
        long_url = "https://example.com/" + "a" * 130 + "/video.mp4"
        output = (
            f"[generic] Extracting URL: {long_url}\n"
            "[download] File is larger than max-filesize "
            "(61878609 bytes > 1048576 bytes). Aborting.\n"
        )
        with self.assertRaises(MODULE.CapExceeded) as ctx:
            MODULE.classify_download_failure(output, artifacts=[])
        message = str(ctx.exception)
        reason = message[:200]  # mirrors build_envelope's truncation
        self.assertIn("61878609", reason)
        self.assertIn("1048576", reason)
        # The full detail — including the URL — must still be recoverable
        # from the untruncated message (error.message never truncates).
        self.assertIn(long_url, message)


class RunnerValidatedCapsTestCase(unittest.TestCase):
    """`_run_audio`/`_run_video` used to re-derive `max_filesize_mb` from the
    raw `request` dict via a bare `int(...)`, bypassing `resolve_numeric_param`
    entirely — harmless only because `dispatch()` happened to always pass a
    pre-validated copy of `request`. Now `max_filesize_mb` is a required
    keyword-only argument the runner has no other way to obtain, so the
    validation is structural rather than a call-site convention that a
    future refactor could silently break.
    """

    def _stub_yt_dlp(self, cmd_holder):
        original_command = MODULE._yt_dlp_command
        original_run = MODULE.subprocess.run
        MODULE._yt_dlp_command = lambda: ["yt-dlp"]

        def fake_run(cmd, **kwargs):
            cmd_holder["cmd"] = cmd
            # Drop a non-empty artifact so the runner reaches its normal
            # return path instead of `classify_download_failure` — these
            # tests only care about the argv `--max-filesize` was built
            # from, not the download outcome.
            out_dir = pathlib.Path(cmd[cmd.index("-o") + 1]).parent
            (out_dir / "x.artifact").write_bytes(b"data")
            return FakeCompletedProcess(returncode=0, stdout="", stderr="")

        MODULE.subprocess.run = fake_run
        return original_command, original_run

    def test_run_audio_called_with_a_raw_request_and_no_explicit_cap_fails_fast(self):
        """The exact bug shape: calling a runner as if `max_filesize_mb`
        could still be read out of `request`. This must raise a `TypeError`
        immediately (a missing required keyword argument), not silently
        build `--max-filesize -1M` from `request["max_filesize_mb"]`."""
        with tempfile.TemporaryDirectory() as tmp:
            with self.assertRaises(TypeError):
                MODULE._run_audio(  # no max_filesize_mb kwarg — the old call shape
                    {"id": "x"}, {"output_dir": tmp, "max_filesize_mb": -1}, "https://example.com/x"
                )

    def test_run_video_called_with_a_raw_request_and_no_explicit_cap_fails_fast(self):
        with tempfile.TemporaryDirectory() as tmp:
            with self.assertRaises(TypeError):
                MODULE._run_video(
                    {"id": "x"}, {"output_dir": tmp, "max_filesize_mb": -1}, "https://example.com/x"
                )

    def test_run_audio_ignores_a_bad_raw_value_still_present_in_request(self):
        """Even when `request` carries the reviewer's exact bad value
        (`max_filesize_mb: -1`), the runner must build its `--max-filesize`
        argument from the explicit, already-validated keyword argument only."""
        cmd_holder = {}
        original_command, original_run = self._stub_yt_dlp(cmd_holder)
        try:
            with tempfile.TemporaryDirectory() as tmp:
                MODULE._run_audio(
                    {"id": "x"}, {"output_dir": tmp, "max_filesize_mb": -1},
                    "https://example.com/x", max_filesize_mb=250,
                )
        finally:
            MODULE._yt_dlp_command = original_command
            MODULE.subprocess.run = original_run
        self.assertIn("--max-filesize", cmd_holder["cmd"])
        self.assertIn("250M", cmd_holder["cmd"])
        self.assertNotIn("-1M", cmd_holder["cmd"])

    def test_run_video_ignores_a_bad_raw_value_still_present_in_request(self):
        cmd_holder = {}
        original_command, original_run = self._stub_yt_dlp(cmd_holder)
        try:
            with tempfile.TemporaryDirectory() as tmp:
                MODULE._run_video(
                    {"id": "x"}, {"output_dir": tmp, "max_filesize_mb": -1},
                    "https://example.com/x", max_filesize_mb=250,
                )
        finally:
            MODULE._yt_dlp_command = original_command
            MODULE.subprocess.run = original_run
        self.assertIn("--max-filesize", cmd_holder["cmd"])
        self.assertIn("250M", cmd_holder["cmd"])
        self.assertNotIn("-1M", cmd_holder["cmd"])


class DownloadFlagsTestCase(unittest.TestCase):
    """The size-abort marker must survive the subprocess boundary.

    yt-dlp prints it as an [download] INFO line on stdout, which --quiet
    suppresses entirely. classify_download_failure then sees an empty string
    and misreports a cap failure as an outage. Measured: with --quiet the
    captured output was 0 bytes; without it, the marker was present.

    --quiet is appended in `_run_audio`/`_run_video` themselves, not inside
    `audio_args`/`video_args`, so asserting against the arg-builder output
    alone would pass trivially even with the bug live. These tests
    monkeypatch `subprocess.run` to capture the real argv the runner
    functions build, and drive them through `assertRaises` so the download
    "fails" (no file is ever created) without touching the network.
    """

    def _capture_cmd(self, runner, request, max_filesize_mb=None):
        captured = {}
        original_command = MODULE._yt_dlp_command
        original_run = MODULE.subprocess.run
        MODULE._yt_dlp_command = lambda: ["yt-dlp"]

        def fake_run(cmd, **kwargs):
            captured["cmd"] = cmd
            return FakeCompletedProcess(returncode=0, stdout="", stderr="")

        MODULE.subprocess.run = fake_run
        try:
            with self.assertRaises(MODULE.ProviderUnavailable):
                runner(
                    {"id": "x"}, request, "https://example.com/x",
                    max_filesize_mb=max_filesize_mb or MODULE.MAX_FILESIZE_MB,
                )
        finally:
            MODULE._yt_dlp_command = original_command
            MODULE.subprocess.run = original_run
        return captured["cmd"]

    def test_audio_download_does_not_silence_the_abort_marker(self):
        with tempfile.TemporaryDirectory() as tmp:
            cmd = self._capture_cmd(MODULE._run_audio, {"output_dir": tmp})
        self.assertNotIn("--quiet", cmd)
        self.assertIn("--no-progress", cmd)

    def test_video_download_does_not_silence_the_abort_marker(self):
        with tempfile.TemporaryDirectory() as tmp:
            cmd = self._capture_cmd(MODULE._run_video, {"output_dir": tmp})
        self.assertNotIn("--quiet", cmd)
        self.assertIn("--no-progress", cmd)

    def test_realistic_quiet_output_would_misclassify(self):
        """Documents the failure mode: empty output cannot be classified."""
        with self.assertRaises(MODULE.ProviderUnavailable):
            MODULE.classify_download_failure("", artifacts=[])

    def test_realistic_noprogress_output_classifies_as_cap(self):
        captured = ("[generic] Extracting URL: https://example.com/x.mp4\n"
                    "[download] File is larger than max-filesize "
                    "(61878609 bytes > 1048576 bytes). Aborting.\n")
        with self.assertRaises(MODULE.CapExceeded) as ctx:
            MODULE.classify_download_failure(captured, artifacts=[])
        self.assertIn("61878609", str(ctx.exception))


class VideoZeroBytePartialTestCase(unittest.TestCase):
    """`_run_audio` filters zero-byte files out of `matches`; `_run_video`
    used not to. A zero-byte partial left behind by a --max-filesize abort
    then made `_run_video`'s `matches` non-empty, which skipped
    `classify_download_failure` entirely and fell into
    `require_single_artifact` — raising ProviderUnavailable instead of
    CapExceeded, the wrong kind again, by a different route."""

    def test_a_zero_byte_partial_does_not_skip_classification(self):
        abort_output = ("[download] File is larger than max-filesize "
                         "(61878609 bytes > 1048576 bytes). Aborting.")
        original_command = MODULE._yt_dlp_command
        original_run = MODULE.subprocess.run
        MODULE._yt_dlp_command = lambda: ["yt-dlp"]

        def fake_run(cmd, **kwargs):
            out_dir = pathlib.Path(cmd[cmd.index("-o") + 1]).parent
            (out_dir / "x.f137.mp4").write_bytes(b"")  # zero-byte partial
            return FakeCompletedProcess(returncode=0, stdout=abort_output, stderr="")

        MODULE.subprocess.run = fake_run
        try:
            with tempfile.TemporaryDirectory() as tmp:
                with self.assertRaises(MODULE.CapExceeded) as ctx:
                    MODULE._run_video(
                        {"id": "x"}, {"output_dir": tmp}, "https://example.com/x",
                        max_filesize_mb=MODULE.MAX_FILESIZE_MB,
                    )
                self.assertIn("61878609", str(ctx.exception))
        finally:
            MODULE._yt_dlp_command = original_command
            MODULE.subprocess.run = original_run


class VideoTestCase(unittest.TestCase):
    def test_video_requires_ffmpeg(self):
        self.assertTrue(MODULE.action_requires_ffmpeg("video"))

    def test_preflight_raises_a_remediable_error_without_ffmpeg(self):
        with self.assertRaises(RuntimeError) as ctx:
            MODULE.preflight_ffmpeg("video", location=None)
        self.assertIn("setup-media-fetch", str(ctx.exception))

    def test_preflight_never_downgrades_to_audio(self):
        """A missing merger must fail, not silently hand back a different artifact."""
        with self.assertRaises(RuntimeError) as ctx:
            MODULE.preflight_ffmpeg("video", location=None)
        self.assertNotIn("falling back", str(ctx.exception).lower())

    def test_preflight_is_a_noop_for_cheap_actions(self):
        MODULE.preflight_ffmpeg("transcript", location=None)
        MODULE.preflight_ffmpeg("audio", location=None)
        MODULE.preflight_ffmpeg("metadata", location=None)

    def test_video_args_request_a_merged_mp4(self):
        joined = " ".join(MODULE.video_args("/tmp/x.%(ext)s", pathlib.Path("/opt/ff")))
        self.assertIn("bv*+ba", joined)
        self.assertIn("--merge-output-format", joined)
        self.assertIn("--ffmpeg-location", joined)

    def test_video_args_omit_ffmpeg_location_when_absent(self):
        self.assertNotIn("--ffmpeg-location", MODULE.video_args("/tmp/x.%(ext)s", None))

    def test_two_unmerged_streams_is_a_failure_not_a_success(self):
        """Measured: without ffmpeg yt-dlp leaves both streams and exits 0."""
        import tempfile
        with tempfile.TemporaryDirectory() as tmp:
            a = pathlib.Path(tmp) / "b.f395.mp4"; a.write_bytes(b"v")
            b = pathlib.Path(tmp) / "b.f251.webm"; b.write_bytes(b"a")
            with self.assertRaises(MODULE.ProviderUnavailable) as ctx:
                MODULE.require_single_artifact([a, b])
            self.assertIn("2", str(ctx.exception))

    def test_one_artifact_is_returned(self):
        import tempfile
        with tempfile.TemporaryDirectory() as tmp:
            f = pathlib.Path(tmp) / "x.mp4"; f.write_bytes(b"data")
            self.assertEqual(MODULE.require_single_artifact([f]), f)

    def test_zero_artifacts_is_a_failure(self):
        with self.assertRaises(MODULE.ProviderUnavailable):
            MODULE.require_single_artifact([])

    def test_an_empty_file_does_not_count_as_an_artifact(self):
        """A zero-byte file is not a positive signal."""
        import tempfile
        with tempfile.TemporaryDirectory() as tmp:
            f = pathlib.Path(tmp) / "x.mp4"; f.write_bytes(b"")
            with self.assertRaises(MODULE.ProviderUnavailable):
                MODULE.require_single_artifact([f])


class OutputDirTestCase(unittest.TestCase):
    def test_traversal_outside_allowlist_is_rejected(self):
        with self.assertRaises(ValueError):
            MODULE.validate_output_dir("/etc")

    def test_rejection_does_not_exit_the_process(self):
        """It must raise so the envelope can report it, not sys.exit."""
        try:
            MODULE.validate_output_dir("/etc")
        except ValueError:
            pass
        except SystemExit:
            self.fail("validate_output_dir must raise ValueError, not exit")

    def test_tmp_is_allowed(self):
        import tempfile
        self.assertTrue(MODULE.validate_output_dir(tempfile.gettempdir()))

    def test_literal_tmp_prefixed_path_is_allowed(self):
        """macOS subtlety: /tmp is a symlink and gettempdir() answers
        /var/folders/.../T, so a caller-supplied /tmp/... path must be
        allowed via the explicit /tmp and /private/tmp aliases, not just
        via a match against gettempdir()."""
        self.assertTrue(MODULE.validate_output_dir("/tmp"))

    def test_cwd_subtree_is_allowed(self):
        self.assertTrue(MODULE.validate_output_dir(str(pathlib.Path.cwd())))

    def test_dotdot_escape_is_rejected(self):
        with self.assertRaises(ValueError):
            MODULE.validate_output_dir(str(pathlib.Path.cwd() / ".." / ".." / ".." / "etc"))


class EnvelopeTestCase(unittest.TestCase):
    def test_failure_envelope_names_the_error(self):
        env = MODULE.build_envelope(action="teleport", url="u", result=None,
                                    error={"kind": "ValueError", "message": "unknown action"},
                                    started=0.0)
        self.assertEqual(env["status"], "failed")
        self.assertEqual(env["error"]["kind"], "ValueError")
        self.assertIsNotNone(env["reason"])

    def test_success_envelope_carries_no_error(self):
        env = MODULE.build_envelope(action="metadata", url="u",
                                    result={"title": "t"}, error=None, started=0.0)
        self.assertEqual(env["status"], "ok")
        self.assertIsNone(env["error"])
        self.assertIsNone(env["reason"])

    def test_envelope_reports_duration(self):
        env = MODULE.build_envelope(action="metadata", url="u", result={}, error=None, started=0.0)
        self.assertIsInstance(env["duration_ms"], int)

    def test_envelope_is_json_serialisable(self):
        env = MODULE.build_envelope(action="metadata", url="u", result={"a": 1}, error=None, started=0.0)
        json.dumps(env)  # must not raise


class RemainingBudgetTestCase(unittest.TestCase):
    """`remaining_budget` is what keeps every subprocess inside the action's
    governed `timeout_secs` — the governed runtime force-kills the whole
    process group at that wall-clock deadline and reports a bare TimedOut
    outside this pack's error taxonomy, so nothing here may outlive it."""

    def test_never_returns_more_than_what_is_left(self):
        deadline = time.monotonic() + 50
        result = MODULE.remaining_budget(deadline)
        self.assertLessEqual(result, 50)
        self.assertGreater(result, 45)  # sanity: not clamped down needlessly

    def test_floors_at_a_small_positive_value_once_the_budget_is_spent(self):
        """An already-passed deadline must not produce zero or negative."""
        deadline = time.monotonic() - 1000
        result = MODULE.remaining_budget(deadline)
        self.assertEqual(result, MODULE.MIN_SUBPROCESS_TIMEOUT_S)
        self.assertGreater(result, 0)

    def test_the_floor_is_configurable(self):
        deadline = time.monotonic() - 1000
        self.assertEqual(MODULE.remaining_budget(deadline, floor=1), 1)


class ActionBudgetMatchesSkillMdTestCase(unittest.TestCase):
    """ACTION_BUDGET_S must equal SKILL.md's declared `timeout_secs` per
    action — read straight from the file, not retyped, so this test breaks
    the moment the two drift apart instead of the mismatch surviving as a
    silent kill in production."""

    SKILL = pathlib.Path(__file__).parents[1] / "SKILL.md"

    def declared_action_timeouts(self) -> dict:
        """Parsed by indentation, not a YAML library — same convention as
        youtube-search's `declared_bin_blocks` — so this test carries no
        dependency the adapter itself does not need."""
        frontmatter = self.SKILL.read_text(encoding="utf-8").split("\n---\n", 1)[0]
        lines = frontmatter.splitlines()
        result = {}
        current = None
        current_indent = None
        for line in lines:
            stripped = line.strip()
            indent = len(line) - len(line.lstrip())
            if current is not None and indent <= current_indent:
                current = None
            if current is None:
                candidate = stripped[:-1] if stripped.endswith(":") else None
                if candidate in MODULE.ACTIONS and indent > 0:
                    current = candidate
                    current_indent = indent
                continue
            if stripped.startswith("timeout_secs:"):
                result[current] = int(stripped.split(":", 1)[1].strip())
                current = None
        return result

    def test_every_action_is_declared(self):
        declared = self.declared_action_timeouts()
        self.assertEqual(set(declared), set(MODULE.ACTIONS))

    def test_action_budget_matches_skill_md_exactly(self):
        declared = self.declared_action_timeouts()
        self.assertEqual(declared, MODULE.ACTION_BUDGET_S)


class DeadlineThreadingTestCase(unittest.TestCase):
    """dispatch() must compute one deadline and hand it to both probe() and
    the action's own runner, so together they cannot exceed the action's
    governed budget the way independent, unbudgeted timeouts used to."""

    def test_probe_and_runner_share_one_budget_for_audio(self):
        calls = []

        def fake_run(cmd, **kwargs):
            calls.append(kwargs.get("timeout"))
            if len(calls) == 1:
                return FakeCompletedProcess(returncode=0, stdout=json.dumps({"id": "x", "duration": 5}))
            return FakeCompletedProcess(returncode=0, stdout="", stderr="")

        original_command = MODULE._yt_dlp_command
        original_run = MODULE.subprocess.run
        MODULE._yt_dlp_command = lambda: ["yt-dlp"]
        MODULE.subprocess.run = fake_run
        try:
            with tempfile.TemporaryDirectory() as tmp:
                MODULE.dispatch("audio", {"url": "https://example.com/x", "output_dir": tmp})
        finally:
            MODULE._yt_dlp_command = original_command
            MODULE.subprocess.run = original_run

        self.assertEqual(len(calls), 2, "expected exactly probe() then the audio runner")
        budget = MODULE.ACTION_BUDGET_S["audio"] - MODULE.ENVELOPE_MARGIN_S
        for timeout in calls:
            self.assertIsNotNone(timeout)
            self.assertGreater(timeout, 0)
            self.assertLessEqual(timeout, budget)


class DispatchTestCase(unittest.TestCase):
    def test_unknown_action_is_a_failure_naming_valid_actions(self):
        env = MODULE.dispatch("teleport", {"url": "https://x/y"})
        self.assertEqual(env["status"], "failed")
        for name in ("metadata", "transcript", "audio", "video"):
            self.assertIn(name, env["error"]["message"])

    def test_missing_action_does_not_default_to_one(self):
        """Defaulting would silently run something the caller did not ask for.

        Checked at the ValueError/"unknown action" level, not just
        status=="failed": a defaulted action that goes on to probe the URL
        and fails for an unrelated reason (backend unavailable, provider
        outage) would also read status=="failed" and hide the real bug.
        """
        env = MODULE.dispatch("", {"url": "https://x/y"})
        self.assertEqual(env["status"], "failed")
        self.assertEqual(env["error"]["kind"], "ValueError")
        self.assertIn("action", env["error"]["message"].lower())

    def test_missing_url_is_a_failure_not_a_traceback(self):
        env = MODULE.dispatch("metadata", {})
        self.assertEqual(env["status"], "failed")
        self.assertIn("url", env["error"]["message"].lower())

    def test_all_four_actions_are_known(self):
        self.assertEqual(set(MODULE.ACTIONS), {"metadata", "transcript", "audio", "video"})

    def _dispatch_with_probe(self, action, info, request=None):
        """Stub probe() itself (not the yt-dlp subprocess below it) so these
        tests exercise dispatch's own ordering of reject_playlists /
        reject_live / enforce_caps without touching the network."""
        original_probe = MODULE.probe
        MODULE.probe = lambda url, timeout=90: info
        try:
            return MODULE.dispatch(action, {**(request or {}), "url": "https://example.com/x"})
        finally:
            MODULE.probe = original_probe

    def test_live_stream_is_refused_for_fetching_actions(self):
        env = self._dispatch_with_probe("audio", {"id": "x", "is_live": True})
        self.assertEqual(env["status"], "failed")
        self.assertEqual(env["error"]["kind"], "CapExceeded")

    def test_live_stream_metadata_is_still_answered(self):
        """metadata must still answer for a live stream — describing one is
        cheap and useful, unlike downloading it."""
        env = self._dispatch_with_probe("metadata", {"id": "x", "is_live": True, "title": "Live now"})
        self.assertEqual(env["status"], "ok")
        self.assertEqual(env["result"]["title"], "Live now")

    def test_playlist_is_refused_for_every_action_including_metadata(self):
        env = self._dispatch_with_probe(
            "metadata", {"id": "x", "_type": "playlist", "entries": [{}, {}]}
        )
        self.assertEqual(env["status"], "failed")
        self.assertEqual(env["error"]["kind"], "CapExceeded")

    def test_caps_are_enforced_before_the_action_runs(self):
        env = self._dispatch_with_probe("metadata", {"id": "x", "duration": 999999})
        self.assertEqual(env["status"], "failed")
        self.assertEqual(env["error"]["kind"], "CapExceeded")


class IsolatedDownloadDirTestCase(unittest.TestCase):
    """Regression test for a bug the live smoke test caught: transcript and
    audio both default `output_dir` to tempfile.gettempdir() and name files
    after the same info["id"] slug, so back-to-back actions against one URL
    used to collide in the same directory — audio's glob("{slug}.*") once
    picked up a leftover ".en.json3" caption file and returned it as the
    audio result. `_isolated_download_dir` must hand each call its own
    empty subdirectory so this is structurally impossible."""

    def test_two_calls_get_different_directories(self):
        with tempfile.TemporaryDirectory() as tmp:
            a = MODULE._isolated_download_dir({"output_dir": tmp})
            b = MODULE._isolated_download_dir({"output_dir": tmp})
            self.assertNotEqual(a, b)

    def test_the_directory_is_under_the_requested_output_dir(self):
        with tempfile.TemporaryDirectory() as tmp:
            out_dir = MODULE._isolated_download_dir({"output_dir": tmp})
            self.assertEqual(out_dir.parent, pathlib.Path(tmp).resolve())

    def test_the_directory_starts_out_empty(self):
        with tempfile.TemporaryDirectory() as tmp:
            out_dir = MODULE._isolated_download_dir({"output_dir": tmp})
            self.assertEqual(list(out_dir.iterdir()), [])

    def test_an_unsafe_output_dir_is_still_rejected(self):
        with self.assertRaises(ValueError):
            MODULE._isolated_download_dir({"output_dir": "/etc"})


if __name__ == "__main__":
    unittest.main()
