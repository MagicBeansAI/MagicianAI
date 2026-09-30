import argparse
import unittest

from scripts.eval_realtime_local_transcript_live import (
    REQUIRED_MANUAL_QA,
    build_report,
    session_metrics,
)


def arguments() -> argparse.Namespace:
    return argparse.Namespace(
        max_word_error_rate=0.25,
        max_character_error_rate=0.15,
        max_first_partial_ms=1_500,
        max_final_ms=3_000,
        min_critical_entity_recall=0.90,
        max_fallback_recovery_ms=5_000,
    )


class RealtimeLocalTranscriptEvalTests(unittest.TestCase):
    def test_session_metrics_correlate_restore_transition_without_content(self) -> None:
        metrics = session_metrics(
            {
                "voice_session_id": "voice-a",
                "events": [
                    {
                        "event_type": "media.voice.local_transcript.fallback",
                        "timestamp_ms": 100,
                        "payload": {"details": {
                            "state": "vendor_restore_requested",
                            "voice_session_id": "voice-a",
                        }},
                    },
                    {
                        "event_type": "media.voice.local_transcript.fallback",
                        "timestamp_ms": 350,
                        "payload": {"details": {
                            "state": "vendor_restore_active",
                            "voice_session_id": "voice-a",
                        }},
                    },
                    {
                        "event_type": "media.voice.local_transcript.state",
                        "timestamp_ms": 1_350,
                        "payload": {
                            "details": {
                                "state": "ended",
                                "voice_session_id": "voice-a",
                                "local_covered_ms": 1_000,
                                "vendor_fallback_ms": 1_000,
                                "queued_audio_bytes": 0,
                                "queued_audio_frames": 0,
                                "high_water_audio_bytes": 4_800,
                                "dropped_audio_frames": 0,
                            }
                        },
                    },
                ]
            }
        )
        self.assertEqual(metrics["fallback_recovery_ms"], [250.0])
        self.assertTrue(metrics["vendor_restore_observed"])
        self.assertTrue(metrics["terminal_coverage_observed"])
        self.assertEqual(metrics["local_covered_seconds"], 1.0)
        self.assertEqual(metrics["vendor_fallback_seconds"], 1.0)
        self.assertEqual(metrics["queue_high_water_audio_bytes"], 4_800)
        self.assertEqual(metrics["queued_audio_bytes"], 0)
        self.assertEqual(metrics["queued_audio_frames"], 0)
        self.assertEqual(metrics["dropped_audio_frames"], 0)

    def test_canonical_transport_envelope_is_unwrapped_and_scope_filtered(self) -> None:
        metrics = session_metrics({
            "voice_session_id": "voice-a",
            "events": [
                {
                    "event_type": "AgentEvent",
                    "data": {"event": {
                        "event_type": "media.audio.provider.fallback",
                        "timestamp": "2026-07-22T10:00:00Z",
                        "payload": {"details": {
                            "voice_session_id": "voice-a",
                            "stage": "streaming_stt",
                            "state": "provider_failed",
                            "stream_session_id": "stream-1",
                        }},
                    }},
                },
                {
                    "event_type": "AgentEvent",
                    "data": {"event": {
                        "event_type": "media.audio.provider.fallback",
                        "timestamp": "2026-07-22T10:00:00.250Z",
                        "payload": {"details": {
                            "voice_session_id": "voice-a",
                            "stage": "streaming_stt",
                            "state": "provider_activated",
                            "stream_session_id": "stream-1",
                        }},
                    }},
                },
                {
                    "event_type": "media.voice.local_transcript.state",
                    "timestamp_ms": 999_999,
                    "payload": {"details": {
                        "voice_session_id": "voice-b",
                        "state": "ended",
                        "local_covered_ms": 10_000,
                        "vendor_fallback_ms": 0,
                        "high_water_audio_bytes": 1,
                        "dropped_audio_frames": 0,
                    }},
                },
            ],
        })
        self.assertEqual(metrics["fallback_recovery_ms"], [250.0])
        self.assertFalse(metrics["terminal_coverage_observed"])

    def test_zero_timestamp_is_valid_fallback_evidence(self) -> None:
        metrics = session_metrics({
            "voice_session_id": "voice-a",
            "events": [
                {
                    "event_type": "media.audio.provider.fallback",
                    "timestamp_ms": 0,
                    "payload": {"details": {
                        "voice_session_id": "voice-a",
                        "stage": "streaming_stt",
                        "state": "provider_failed",
                        "stream_session_id": "stream-a",
                    }},
                },
                {
                    "event_type": "media.audio.provider.fallback",
                    "timestamp_ms": 10,
                    "payload": {"details": {
                        "voice_session_id": "voice-a",
                        "stage": "streaming_stt",
                        "state": "provider_activated",
                        "stream_session_id": "stream-a",
                    }},
                },
            ],
        })

        self.assertEqual(metrics["fallback_recovery_ms"], [10.0])

    def test_missing_cost_and_manual_fields_fail_closed(self) -> None:
        report = build_report(
            {
                "measurements": [{
                    "stage": "stt",
                    "mode": "streaming",
                    "metrics": {
                        "transcript": "hello",
                        "word_error_rate": 0.0,
                        "character_error_rate": 0.0,
                        "critical_entity_recall": 1.0,
                    },
                    "timings": {"first_partial_ms": 100, "first_final_ms": 500},
                }]
            },
            {
                "voice_session_id": "voice-a",
                "events": [],
                "manual_qa": {"iphone_ptt": True, "tray_ptt": True},
            },
            arguments(),
        )
        self.assertFalse(report["checks"]["local_cost_coverage"])
        self.assertFalse(report["checks"]["manual_surface_qa"])
        self.assertIn("sidecar_termination", report["recovery_and_cost"]["manual_qa_missing"])

    def test_report_fails_when_live_recovery_evidence_is_missing(self) -> None:
        report = build_report(
            {
                "measurements": [
                    {
                        "stage": "stt",
                        "mode": "streaming",
                        "metrics": {
                            "transcript": "hello",
                            "word_error_rate": 0.0,
                            "character_error_rate": 0.0,
                            "critical_entity_recall": 1.0,
                        },
                        "timings": {"first_partial_ms": 100, "first_final_ms": 500},
                    }
                ]
            },
            {
                "local_covered_seconds": 30,
                "prior_provider_cost_usd": 1,
                "observed_provider_cost_usd": 0,
                "manual_qa": {"iphone_ptt": True},
                "events": [],
            },
            arguments(),
        )
        self.assertFalse(report["checks"]["fallback_recovery"])
        self.assertFalse(report["checks"]["terminal_coverage"])
        self.assertFalse(report["passed"])

    def test_incomplete_measurement_rows_fail_closed(self) -> None:
        complete = {
            "stage": "stt",
            "mode": "streaming",
            "metrics": {
                "transcript": "hello",
                "word_error_rate": 0.0,
                "character_error_rate": 0.0,
                "critical_entity_recall": 1.0,
            },
            "timings": {"first_partial_ms": 100, "first_final_ms": 500},
        }
        incomplete = {
            "stage": "stt",
            "mode": "streaming",
            "metrics": {"transcript": "missing metrics"},
            "timings": {"first_final_ms": 500},
        }

        report = build_report(
            {"measurements": [complete, incomplete]},
            {"voice_session_id": "voice-a", "events": []},
            arguments(),
        )

        self.assertFalse(report["checks"]["complete_measurements"])
        self.assertFalse(report["passed"])

    def test_terminal_queue_must_be_empty_for_lossless_acceptance(self) -> None:
        evidence = {
            "voice_session_id": "voice-a",
            "provider_transcription_usage_seconds": 0,
            "provider_transcription_healthy_local_seconds": 0,
            "prior_provider_cost_usd": 1,
            "observed_provider_cost_usd": 0,
            "manual_qa": {key: True for key in REQUIRED_MANUAL_QA},
            "events": [
                {
                    "event_type": "media.audio.provider.fallback",
                    "timestamp_ms": 1,
                    "payload": {"details": {
                        "voice_session_id": "voice-a",
                        "stage": "streaming_stt",
                        "state": "provider_failed",
                        "stream_session_id": "stream-a",
                    }},
                },
                {
                    "event_type": "media.audio.provider.fallback",
                    "timestamp_ms": 11,
                    "payload": {"details": {
                        "voice_session_id": "voice-a",
                        "stage": "streaming_stt",
                        "state": "provider_activated",
                        "stream_session_id": "stream-a",
                    }},
                },
                {
                    "event_type": "media.voice.local_transcript.state",
                    "timestamp_ms": 100,
                    "payload": {
                        "details": {
                            "state": "ended",
                            "voice_session_id": "voice-a",
                            "local_covered_ms": 1_000,
                            "vendor_fallback_ms": 0,
                            "queued_audio_bytes": 960,
                            "queued_audio_frames": 1,
                            "high_water_audio_bytes": 1_920,
                            "dropped_audio_frames": 0,
                        }
                    },
                },
            ],
        }
        report = build_report(
            {
                "measurements": [
                    {
                        "stage": "stt",
                        "mode": "streaming",
                        "metrics": {
                            "transcript": "hello",
                            "word_error_rate": 0,
                            "character_error_rate": 0,
                            "critical_entity_recall": 1,
                        },
                        "timings": {"first_partial_ms": 100, "first_final_ms": 500},
                    }
                ]
            },
            evidence,
            arguments(),
        )

        self.assertTrue(report["checks"]["terminal_coverage"])
        self.assertFalse(report["checks"]["lossless_local_queue"])
        self.assertFalse(report["passed"])


if __name__ == "__main__":
    unittest.main()
