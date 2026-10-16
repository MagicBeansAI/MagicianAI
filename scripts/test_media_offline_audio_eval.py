#!/usr/bin/env python3
"""Provider-free regression tests for the offline audio evaluator."""

from __future__ import annotations

import io
import math
import struct
import unittest
import wave

from scripts import media_offline_audio_eval as evaluator


def wav_bytes(samples: list[int], sample_rate: int = 16000) -> bytes:
    output = io.BytesIO()
    with wave.open(output, "wb") as target:
        target.setnchannels(1)
        target.setsampwidth(2)
        target.setframerate(sample_rate)
        target.writeframes(b"".join(struct.pack("<h", sample) for sample in samples))
    return output.getvalue()


class TextMetricTests(unittest.TestCase):
    def test_word_and_character_error_rates_are_strict(self) -> None:
        self.assertAlmostEqual(evaluator.word_error_rate("one two three", "one two"), 1 / 3)
        self.assertGreater(evaluator.character_error_rate("7421", "seven four two one"), 1)


class VadMetricTests(unittest.TestCase):
    def test_segment_metrics_score_overlap_and_false_positive_time(self) -> None:
        metrics = evaluator.speech_segment_metrics([(100, 500)], [(200, 600)], 1000)
        self.assertAlmostEqual(metrics["speech_precision"], 0.75)
        self.assertAlmostEqual(metrics["speech_recall"], 0.75)
        self.assertEqual(metrics["false_positive_ms"], 100)
        self.assertEqual(metrics["missed_speech_ms"], 100)

    def test_no_speech_is_perfect_only_when_model_is_quiet(self) -> None:
        quiet = evaluator.speech_segment_metrics([], [], 1000)
        noisy = evaluator.speech_segment_metrics([], [(0, 100)], 1000)
        self.assertEqual(quiet["speech_f1"], 1)
        self.assertEqual(noisy["speech_f1"], 0)
        self.assertAlmostEqual(noisy["false_positive_rate"], 0.1)


class TtsMetricTests(unittest.TestCase):
    def test_waveform_metrics_report_duration_silence_and_clipping(self) -> None:
        silence = [0] * 1600
        tone = [round(12000 * math.sin(index / 8)) for index in range(3200)]
        clipped = [32767]
        metrics = evaluator.wav_signal_metrics(wav_bytes(silence + tone + clipped + silence))
        self.assertAlmostEqual(metrics["audio_duration_ms"], 400.0625)
        self.assertGreaterEqual(metrics["leading_silence_ms"], 99)
        self.assertGreaterEqual(metrics["trailing_silence_ms"], 99)
        self.assertGreater(metrics["clipping_ratio"], 0)


class DiarizationMetricTests(unittest.TestCase):
    def test_speaker_ids_are_permutation_invariant(self) -> None:
        expected = [
            {"speaker_id": "alice", "start_ms": 0, "end_ms": 500},
            {"speaker_id": "bob", "start_ms": 500, "end_ms": 1000},
        ]
        predicted = [
            {"speaker_id": "speaker_2", "start_ms": 0, "end_ms": 500},
            {"speaker_id": "speaker_1", "start_ms": 500, "end_ms": 1000},
        ]
        rate, mapping = evaluator.diarization_error_rate(expected, predicted, 1000)
        self.assertEqual(rate, 0)
        self.assertEqual(mapping, {"speaker_1": "bob", "speaker_2": "alice"})

    def test_missing_second_speaker_counts_as_error(self) -> None:
        expected = [
            {"speaker_id": "a", "start_ms": 0, "end_ms": 500},
            {"speaker_id": "b", "start_ms": 500, "end_ms": 1000},
        ]
        predicted = [{"speaker_id": "one", "start_ms": 0, "end_ms": 500}]
        rate, _ = evaluator.diarization_error_rate(expected, predicted, 1000)
        self.assertAlmostEqual(rate, 0.5)


class SelectionAndSummaryTests(unittest.TestCase):
    def test_provider_filter_rejects_unknown_stage(self) -> None:
        with self.assertRaises(ValueError):
            evaluator.parse_provider_filters(["voice=local"])

    def test_generic_stt_filter_can_select_one_stt_mode(self) -> None:
        providers = {
            "stt_recording": [{"id": "recording"}],
            "stt_streaming": [{"id": "streaming"}],
        }
        selected = evaluator.filter_providers(providers, {"stt": {"recording"}})
        self.assertEqual([row["id"] for row in selected["stt_recording"]], ["recording"])
        self.assertEqual(selected["stt_streaming"], [])

    def test_faster_better_model_dominates_pareto_peer(self) -> None:
        provider_a = {"id": "a", "adapter": "local", "model": "a"}
        provider_b = {"id": "b", "adapter": "local", "model": "b"}
        rows = []
        for provider, wer, rtf in ((provider_a, 0.1, 0.2), (provider_b, 0.2, 0.3)):
            row = evaluator.base_measurement("stt", "recording", provider, "fixture", "warm", 1)
            row.update(
                status="passed",
                quality_passed=True,
                metrics={"word_error_rate": wer, "character_error_rate": wer},
                timings={"wall_time_ms": 100, "real_time_factor": rtf},
                resources={},
            )
            rows.append(row)
        summaries = {row["provider_id"]: row for row in evaluator.summarize_models(rows)}
        self.assertTrue(summaries["a"]["pareto_preferred"])
        self.assertFalse(summaries["b"]["pareto_preferred"])


class LifecycleConfigurationTests(unittest.TestCase):
    def test_evaluator_builds_cached_only_sidecar_without_starting_it(self) -> None:
        config_path = evaluator.REPO_ROOT / "magician-config.yaml"
        suite_path = evaluator.DEFAULT_SUITE
        config = evaluator.baseline.read_yaml(config_path)
        suite = evaluator.read_json(suite_path)
        fixture_manifest_path = suite_path.parent / suite["fixture_manifest"]
        fixtures = evaluator.fixture_index(evaluator.read_json(fixture_manifest_path))
        providers = evaluator.discover_providers(config, suite)
        args = evaluator.parse_args(
            ["--config", str(config_path), "--suite", str(suite_path), "--stages", "vad", "--dry-run"]
        )
        instance = evaluator.OfflineEvaluator(
            args, config, suite, fixture_manifest_path, fixtures, providers
        )
        try:
            payload = instance.sidecar.sidecar_config()
            models = {row["id"]: row for row in payload["models"]}
            self.assertTrue(payload["offline"])
            self.assertEqual(payload["download_policy"], "disabled")
            self.assertEqual(models["fluid-kokoro-en"]["voice"], "af_heart")
            self.assertIn("af_kore", models["fluid-kokoro-en"]["voices"])
            self.assertIsNone(instance.sidecar.process_id())
        finally:
            instance.close()


if __name__ == "__main__":
    unittest.main()
