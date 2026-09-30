#!/usr/bin/env python3
"""Build the Live Call local-transcript quality, recovery, and cost report."""

from __future__ import annotations

import argparse
import datetime as dt
import html
import json
import math
import statistics
import subprocess
import sys
from pathlib import Path
from typing import Any


REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_OUTPUT_DIR = REPO_ROOT / "coverage/evals/realtime-local-transcript/latest"
REQUIRED_MANUAL_QA = {
    "iphone_ptt",
    "tray_ptt",
    "rapid_turns",
    "prefix_rejection",
    "sidecar_termination",
    "rotation",
    "reconnect",
    "termination",
}
REQUIRED_MEASUREMENT_METRICS = (
    "word_error_rate",
    "character_error_rate",
    "critical_entity_recall",
)
REQUIRED_MEASUREMENT_TIMINGS = ("first_partial_ms", "first_final_ms")


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--offline-input", type=Path)
    parser.add_argument("--session-evidence", type=Path)
    parser.add_argument("--output-dir", type=Path, default=DEFAULT_OUTPUT_DIR)
    parser.add_argument("--config", type=Path)
    parser.add_argument("--fixtures")
    parser.add_argument("--allow-model-downloads", action="store_true")
    parser.add_argument("--max-word-error-rate", type=float, default=0.25)
    parser.add_argument("--max-character-error-rate", type=float, default=0.15)
    parser.add_argument("--max-first-partial-ms", type=float, default=1_500)
    parser.add_argument("--max-final-ms", type=float, default=3_000)
    parser.add_argument("--min-critical-entity-recall", type=float, default=0.90)
    parser.add_argument("--max-fallback-recovery-ms", type=float, default=5_000)
    parser.add_argument("--self-test", action="store_true")
    return parser.parse_args(argv)


def percentile(values: list[float], quantile: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    index = min(len(ordered) - 1, max(0, round((len(ordered) - 1) * quantile)))
    return ordered[index]


def numeric(rows: list[dict[str, Any]], section: str, key: str) -> list[float]:
    values: list[float] = []
    for row in rows:
        value = row.get(section, {}).get(key)
        number = optional_number(value)
        if number is not None:
            values.append(number)
    return values


def measurement_is_complete(row: dict[str, Any]) -> bool:
    metrics = row.get("metrics") if isinstance(row.get("metrics"), dict) else {}
    timings = row.get("timings") if isinstance(row.get("timings"), dict) else {}
    if not str(metrics.get("transcript") or "").strip():
        return False
    return all(optional_number(metrics.get(key)) is not None for key in REQUIRED_MEASUREMENT_METRICS) and all(
        optional_number(timings.get(key)) is not None for key in REQUIRED_MEASUREMENT_TIMINGS
    )


def first_present(mapping: dict[str, Any], *keys: str) -> Any:
    for key in keys:
        if key in mapping and mapping[key] is not None:
            return mapping[key]
    return None


def event_parts(event: dict[str, Any]) -> tuple[str, int | None, dict[str, Any]]:
    if event.get("event_type") == "AgentEvent" and isinstance(event.get("data"), dict):
        data = event["data"]
        event = data.get("event") if isinstance(data.get("event"), dict) else data
    elif isinstance(event.get("event"), dict):
        event = event["event"]
    event_type = str(event.get("event_type") or event.get("type") or "")
    timestamp = first_present(event, "timestamp_ms", "created_at_ms", "timestamp")
    payload = event.get("payload") if isinstance(event.get("payload"), dict) else event
    payload_timestamp = first_present(payload, "timestamp_ms")
    if payload_timestamp is not None:
        timestamp = payload_timestamp
    details = (
        dict(payload["details"])
        if isinstance(payload.get("details"), dict)
        else dict(payload)
    )
    if "voice_session_id" not in details and isinstance(payload.get("voice_session_id"), str):
        details["voice_session_id"] = payload["voice_session_id"]
    return event_type, timestamp_millis(timestamp), details


def timestamp_millis(value: Any) -> int | None:
    if isinstance(value, (int, float)):
        return int(value)
    if not isinstance(value, str) or not value.strip():
        return None
    try:
        parsed = dt.datetime.fromisoformat(value.strip().replace("Z", "+00:00"))
    except ValueError:
        return None
    return int(parsed.timestamp() * 1_000)


def optional_number(value: Any) -> float | None:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    number = float(value)
    return number if math.isfinite(number) and number >= 0 else None


def session_metrics(evidence: dict[str, Any]) -> dict[str, Any]:
    events = evidence.get("events") if isinstance(evidence.get("events"), list) else []
    requested_at: int | None = None
    active_at: int | None = None
    fallback_recovery: list[float] = []
    local_provider_failures: dict[str, int] = {}
    states: list[str] = []
    terminal: dict[str, Any] | None = None
    voice_session_id = evidence.get("voice_session_id")
    scoped_session = voice_session_id if isinstance(voice_session_id, str) and voice_session_id else None
    for raw in events:
        if not isinstance(raw, dict):
            continue
        event_type, timestamp, details = event_parts(raw)
        if scoped_session is None or details.get("voice_session_id") != scoped_session:
            continue
        if event_type not in {
            "media.voice.local_transcript.state",
            "media.voice.local_transcript.fallback",
            "media.audio.provider.fallback",
        }:
            continue
        state = str(details.get("state") or "")
        if state:
            states.append(state)
        if state == "vendor_restore_requested":
            requested_at = timestamp
        elif state == "vendor_restore_active":
            active_at = timestamp
            if requested_at is not None and timestamp is not None:
                fallback_recovery.append(float(max(0, timestamp - requested_at)))
        elif state == "ended":
            terminal = details
        if event_type == "media.audio.provider.fallback" and details.get("stage") == "streaming_stt":
            stream_session_id = str(details.get("stream_session_id") or "")
            if state == "provider_failed" and timestamp is not None and stream_session_id:
                local_provider_failures[stream_session_id] = timestamp
            elif state == "provider_activated" and timestamp is not None and stream_session_id:
                failed_at = local_provider_failures.pop(stream_session_id, None)
                if failed_at is not None:
                    fallback_recovery.append(float(max(0, timestamp - failed_at)))
    manual = evidence.get("manual_qa") if isinstance(evidence.get("manual_qa"), dict) else {}
    terminal_numeric = terminal is not None and all(
        optional_number(terminal.get(key)) is not None
        for key in (
            "local_covered_ms",
            "vendor_fallback_ms",
            "queued_audio_bytes",
            "queued_audio_frames",
            "high_water_audio_bytes",
            "dropped_audio_frames",
        )
    )
    local_covered_seconds = (
        optional_number(terminal.get("local_covered_ms")) / 1_000
        if terminal_numeric
        else optional_number(evidence.get("local_covered_seconds"))
    )
    vendor_fallback_seconds = (
        optional_number(terminal.get("vendor_fallback_ms")) / 1_000
        if terminal_numeric
        else optional_number(evidence.get("vendor_fallback_seconds"))
    )
    return {
        "states": states,
        "fallback_recovery_ms": fallback_recovery,
        "vendor_restore_observed": active_at is not None,
        "terminal_coverage_observed": terminal_numeric,
        "provider_transcription_usage_seconds": optional_number(
            evidence.get("provider_transcription_usage_seconds")
        ),
        "provider_transcription_healthy_local_seconds": optional_number(
            evidence.get("provider_transcription_healthy_local_seconds")
        ),
        "local_covered_seconds": local_covered_seconds,
        "vendor_fallback_seconds": vendor_fallback_seconds,
        "queue_high_water_audio_bytes": int(
            optional_number(terminal.get("high_water_audio_bytes")) or 0
            if terminal_numeric
            else 0
        ),
        "queued_audio_bytes": int(
            optional_number(terminal.get("queued_audio_bytes")) or 0
            if terminal_numeric
            else -1
        ),
        "queued_audio_frames": int(
            optional_number(terminal.get("queued_audio_frames")) or 0
            if terminal_numeric
            else -1
        ),
        "dropped_audio_frames": int(
            optional_number(terminal.get("dropped_audio_frames")) or 0
            if terminal_numeric
            else -1
        ),
        "prior_provider_cost_usd": optional_number(evidence.get("prior_provider_cost_usd")),
        "observed_provider_cost_usd": optional_number(
            evidence.get("observed_provider_cost_usd")
        ),
        "manual_qa": manual,
        "manual_qa_missing": sorted(REQUIRED_MANUAL_QA - manual.keys()),
    }


def build_report(
    offline: dict[str, Any], evidence: dict[str, Any], args: argparse.Namespace
) -> dict[str, Any]:
    rows = [
        row
        for row in offline.get("measurements", [])
        if row.get("stage") == "stt" and row.get("mode") == "streaming"
    ]
    transcripts = [str(row.get("metrics", {}).get("transcript", "")).strip() for row in rows]
    complete_measurements = bool(rows) and all(measurement_is_complete(row) for row in rows)
    wer = numeric(rows, "metrics", "word_error_rate")
    cer = numeric(rows, "metrics", "character_error_rate")
    entity = numeric(rows, "metrics", "critical_entity_recall")
    first_partial = numeric(rows, "timings", "first_partial_ms")
    first_final = numeric(rows, "timings", "first_final_ms")
    session = session_metrics(evidence)
    manual_values = [session["manual_qa"].get(key) for key in sorted(REQUIRED_MANUAL_QA)]
    cost_values = (
        session["provider_transcription_usage_seconds"],
        session["provider_transcription_healthy_local_seconds"],
        session["prior_provider_cost_usd"],
        session["observed_provider_cost_usd"],
    )
    checks = {
        "complete_measurements": complete_measurements,
        "caption_presence": bool(rows) and all(transcripts),
        "word_error_rate": bool(wer) and statistics.fmean(wer) <= args.max_word_error_rate,
        "character_error_rate": bool(cer)
        and statistics.fmean(cer) <= args.max_character_error_rate,
        "critical_entity_recall": bool(entity)
        and statistics.fmean(entity) >= args.min_critical_entity_recall,
        "first_partial_latency": bool(first_partial)
        and (percentile(first_partial, 0.95) or float("inf")) <= args.max_first_partial_ms,
        "final_latency": bool(first_final)
        and (percentile(first_final, 0.95) or float("inf")) <= args.max_final_ms,
        "fallback_recovery": bool(session["fallback_recovery_ms"])
        and max(session["fallback_recovery_ms"]) <= args.max_fallback_recovery_ms,
        "terminal_coverage": session["terminal_coverage_observed"],
        "lossless_local_queue": session["terminal_coverage_observed"]
        and session["queued_audio_bytes"] == 0
        and session["queued_audio_frames"] == 0
        and session["dropped_audio_frames"] == 0,
        "local_cost_coverage": all(value is not None for value in cost_values)
        and session["local_covered_seconds"] is not None
        and session["local_covered_seconds"] > 0
        and session["provider_transcription_healthy_local_seconds"] == 0
        and session["observed_provider_cost_usd"] <= session["prior_provider_cost_usd"],
        "manual_surface_qa": not session["manual_qa_missing"]
        and all(value is True for value in manual_values),
    }
    return {
        "schema_version": 1,
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "passed": all(checks.values()),
        "checks": checks,
        "quality": {
            "measurement_count": len(rows),
            "caption_presence_rate": (
                sum(bool(value) for value in transcripts) / len(rows) if rows else 0.0
            ),
            "mean_word_error_rate": statistics.fmean(wer) if wer else None,
            "mean_character_error_rate": statistics.fmean(cer) if cer else None,
            "mean_critical_entity_recall": statistics.fmean(entity) if entity else None,
            "first_partial_p95_ms": percentile(first_partial, 0.95),
            "final_p95_ms": percentile(first_final, 0.95),
        },
        "recovery_and_cost": session,
        "sources": {
            "offline_run_id": offline.get("run_id"),
            "voice_session_id": evidence.get("voice_session_id"),
        },
    }


def render_html(report: dict[str, Any]) -> str:
    rows = "".join(
        f"<tr><td>{html.escape(name.replace('_', ' ').title())}</td>"
        f"<td class={'pass' if passed else 'fail'}>{'PASS' if passed else 'FAIL'}</td></tr>"
        for name, passed in report["checks"].items()
    )
    quality = "".join(
        f"<tr><td>{html.escape(name.replace('_', ' ').title())}</td>"
        f"<td>{html.escape(str(value))}</td></tr>"
        for name, value in report["quality"].items()
    )
    return f"""<!doctype html><html><head><meta charset="utf-8"><title>Realtime local transcript eval</title>
<style>body{{font:14px system-ui;margin:32px;color:#202124}}table{{border-collapse:collapse;min-width:620px}}
td,th{{border:1px solid #d8dce2;padding:8px;text-align:left}}.pass{{color:#137333;font-weight:700}}
.fail{{color:#b3261e;font-weight:700}}code{{background:#f4f5f7;padding:2px 4px}}</style></head><body>
<h1>Realtime local transcript eval</h1><p>Overall: <strong>{'PASS' if report['passed'] else 'FAIL'}</strong></p>
<h2>Acceptance checks</h2><table><tbody>{rows}</tbody></table>
<h2>Quality and latency</h2><table><tbody>{quality}</tbody></table>
<p>Generated <code>{html.escape(report['generated_at'])}</code>. Transcript text and audio are not included.</p>
</body></html>"""


def run_offline(args: argparse.Namespace, output: Path) -> None:
    command = [
        sys.executable,
        str(REPO_ROOT / "scripts/media_offline_audio_eval.py"),
        "--stages",
        "stt",
        "--stt-modes",
        "streaming",
        "--realtime-streaming",
        "--include-events",
        "--fail-on-quality",
        "--output",
        str(output),
    ]
    if args.config:
        command.extend(["--config", str(args.config)])
    if args.fixtures:
        command.extend(["--fixtures", args.fixtures])
    if args.allow_model_downloads:
        command.append("--allow-model-downloads")
    subprocess.run(command, cwd=REPO_ROOT, check=True)


def self_test(args: argparse.Namespace) -> int:
    offline = {
        "run_id": "self-test",
        "measurements": [
            {
                "stage": "stt",
                "mode": "streaming",
                "metrics": {
                    "transcript": "Pay Riya 4200 on 21 July",
                    "word_error_rate": 0.05,
                    "character_error_rate": 0.03,
                    "critical_entity_recall": 1.0,
                },
                "timings": {"first_partial_ms": 300, "first_final_ms": 900},
            }
        ],
    }
    evidence = {
        "voice_session_id": "self-test",
        "local_covered_seconds": 60,
        "provider_transcription_usage_seconds": 1,
        "provider_transcription_healthy_local_seconds": 0,
        "prior_provider_cost_usd": 1.0,
        "observed_provider_cost_usd": 0.1,
        "events": [
            {
                "event_type": "media.voice.local_transcript.fallback",
                "timestamp_ms": 1_000,
                "payload": {"details": {"state": "vendor_restore_requested"}},
            },
            {
                "event_type": "media.voice.local_transcript.fallback",
                "timestamp_ms": 1_500,
                "payload": {"details": {"state": "vendor_restore_active"}},
            },
            {
                "event_type": "media.voice.local_transcript.state",
                "timestamp_ms": 61_500,
                "payload": {
                    "details": {
                        "state": "ended",
                        "local_covered_ms": 60_000,
                        "vendor_fallback_ms": 1_000,
                        "queued_audio_bytes": 0,
                        "queued_audio_frames": 0,
                        "high_water_audio_bytes": 9_600,
                        "dropped_audio_frames": 0,
                    }
                },
            },
        ],
        "manual_qa": {key: True for key in REQUIRED_MANUAL_QA},
    }
    for event in evidence["events"]:
        event["payload"]["details"]["voice_session_id"] = "self-test"
    report = build_report(offline, evidence, args)
    assert report["passed"]
    assert "Realtime local transcript eval" in render_html(report)
    print("Realtime local transcript eval self-test passed")
    return 0


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    if args.self_test:
        return self_test(args)
    if args.session_evidence is None:
        raise SystemExit("--session-evidence is required for the live recovery/cost gate")
    output_dir = args.output_dir.expanduser().resolve()
    output_dir.mkdir(parents=True, exist_ok=True)
    offline_path = args.offline_input or output_dir / "offline-streaming-stt.json"
    if args.offline_input is None:
        run_offline(args, offline_path)
    offline = json.loads(offline_path.read_text(encoding="utf-8"))
    evidence = json.loads(args.session_evidence.expanduser().read_text(encoding="utf-8"))
    report = build_report(offline, evidence, args)
    (output_dir / "report.json").write_text(
        json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    (output_dir / "report.html").write_text(render_html(report), encoding="utf-8")
    print(f"Wrote {output_dir / 'report.json'}")
    print(f"Wrote {output_dir / 'report.html'}")
    return 0 if report["passed"] else 2


if __name__ == "__main__":
    raise SystemExit(main())
