#!/usr/bin/env python3
"""Provider-free Phase 8 browser ladder contract evaluator."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
from typing import Any


SCHEMA_VERSION = 1
DEFAULT_CORPUS = Path(__file__).with_name("fixtures") / "content_browser_phase8/corpus.json"
BROWSER_ACTIONS = {
    "browser.headless.read",
    "browser.headless.discover_handoff",
    "browser.headed.read_handoff",
    "browser.cdp.read",
    "browser.cdp.interact_handoff",
}
IDENTITY_MODES = {"authenticated_read", "authenticated_interact"}


def choose_route(case: dict[str, Any]) -> dict[str, str]:
    static_outcome = str(case.get("static_outcome") or "failed")
    replay_outcome = str(case.get("api_replay_outcome") or "unavailable")
    needs_interaction = bool(case.get("needs_interaction"))
    requires_identity = bool(case.get("requires_identity"))
    owner_assist = bool(case.get("owner_assist"))

    if static_outcome == "sufficient":
        return {
            "action": "static_http.read",
            "authority": "public_read",
            "mode": "none",
            "session_outcome": "not_started",
        }
    if replay_outcome == "sufficient":
        return {
            "action": "api_replay.read",
            "authority": "public_read",
            "mode": "none",
            "session_outcome": "not_started",
        }
    if requires_identity and needs_interaction:
        return {
            "action": "browser.cdp.interact_handoff",
            "authority": "authenticated_interact",
            "mode": "authenticated_interact",
            "session_outcome": "transferred",
        }
    if requires_identity:
        return {
            "action": "browser.cdp.read",
            "authority": "authenticated_read",
            "mode": "authenticated_read",
            "session_outcome": "closed",
        }
    if needs_interaction and owner_assist:
        return {
            "action": "browser.headed.read_handoff",
            "authority": "public_browser_interact",
            "mode": "public_headed_interact",
            "session_outcome": "transferred",
        }
    if needs_interaction:
        return {
            "action": "browser.headless.discover_handoff",
            "authority": "public_browser_interact",
            "mode": "public_headless_interact",
            "session_outcome": "transferred",
        }
    return {
        "action": "browser.headless.read",
        "authority": "public_browser_read",
        "mode": "public_headless_read",
        "session_outcome": "closed",
    }


def evaluate_corpus(corpus: dict[str, Any]) -> dict[str, Any]:
    if corpus.get("schema_version") != SCHEMA_VERSION:
        raise ValueError("unsupported browser fixture corpus schema")
    cases = corpus.get("cases")
    if not isinstance(cases, list) or not cases:
        raise ValueError("browser fixture corpus must contain cases")

    results: list[dict[str, Any]] = []
    false_browser_escalations = 0
    authority_violations = 0
    session_leaks = 0
    for case in cases:
        if not isinstance(case, dict) or not str(case.get("id") or "").strip():
            raise ValueError("browser fixture cases require IDs")
        actual = choose_route(case)
        expected = {
            "action": case.get("expected_action"),
            "authority": case.get("expected_authority"),
            "mode": case.get("expected_mode"),
            "session_outcome": case.get("expected_session_outcome"),
        }
        matched = actual == expected
        browser_escalated_after_success = (
            case.get("static_outcome") == "sufficient"
            or case.get("api_replay_outcome") == "sufficient"
        ) and actual["action"] in BROWSER_ACTIONS
        authority_violation = (
            actual["mode"] in IDENTITY_MODES
            and not bool(case.get("requires_identity"))
        ) or (
            actual["authority"] == "authenticated_read"
            and bool(case.get("needs_interaction"))
        )
        session_leak = actual["session_outcome"] not in {
            "not_started",
            "closed",
            "transferred",
        }
        false_browser_escalations += int(browser_escalated_after_success)
        authority_violations += int(authority_violation)
        session_leaks += int(session_leak)
        results.append(
            {
                "case_id": case["id"],
                "passed": matched
                and not browser_escalated_after_success
                and not authority_violation
                and not session_leak,
                "route": actual,
                "transport_outcome": case.get("transport_outcome", "success"),
            }
        )

    passed_count = sum(result["passed"] for result in results)
    return {
        "schema_version": SCHEMA_VERSION,
        "generated_by": "magician::content_browser_fixture_eval",
        "corpus_digest": hashlib.sha256(
            json.dumps(corpus, sort_keys=True).encode("utf-8")
        ).hexdigest()[:16],
        "summary": {
            "case_count": len(results),
            "passed_count": passed_count,
            "failed_count": len(results) - passed_count,
            "false_browser_escalations": false_browser_escalations,
            "authority_violations": authority_violations,
            "session_leaks": session_leaks,
        },
        "gate": "PASS"
        if passed_count == len(results)
        and false_browser_escalations == 0
        and authority_violations == 0
        and session_leaks == 0
        else "FAIL",
        "cases": results,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--corpus", type=Path, default=DEFAULT_CORPUS)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    corpus = json.loads(args.corpus.read_text(encoding="utf-8"))
    report = evaluate_corpus(corpus)
    rendered = json.dumps(report, indent=2, sort_keys=True) + "\n"
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(rendered, encoding="utf-8")
    else:
        print(rendered, end="")
    return 0 if report["gate"] == "PASS" else 1


if __name__ == "__main__":
    raise SystemExit(main())
