#!/usr/bin/env python3
"""Guard the rank-recompute result-semantics vocabulary across three languages.

The backend emits one of these labels on every rank-recompute result, the frozen
fixture pins them as contract, and the web parser validates against its own copy
and returns `null` for anything it does not recognise. Those three lists must
agree, and nothing in a single language can make them.

The failure is silent, which is why it is worth a guard rather than a comment:
the web parser's sibling check is an exact key allowlist, so a backend that
begins emitting a label the parser has not been taught reads as no result at all
while the page carries on rendering. That is how the attention impressions bug
stayed invisible for the feature's entire life.
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
# Moved by 6873bf7bf, which extracted `magician-comms`; then by plan
# workstream 3.0 (2026-08-26), which relocated the durable job/result
# vocabulary lib-side. The old paths were
# `magician/src/magician_v2/attention_learning/rank_recompute.rs` and
# `magician-comms/src/channel_assist/attention_learning/rank_recompute.rs`
# (the comms file now holds only the comms-coupled worker and re-exports).
RUST = ROOT / "magician/src/magician_v2/attention/learning/rank_recompute.rs"
FIXTURE = ROOT / "data/magician_v2/attention_learning/rank-recompute-frozen-v1.json"
WEB = ROOT / "ui/unified-ui/src/lib/attention/attentionRankRecompute.ts"

RUST_CONST = re.compile(
    r"pub const ATTENTION_RANK_RECOMPUTE_(?:RESULT|SERVED_UNIVERSE)_SEMANTICS"
    r'\s*:\s*&str\s*=\s*"([a-z_]+)"'
)
WEB_SET = re.compile(
    r"const SEMANTICS = new Set<AttentionRankRecomputeSemantics>\(\[(.*?)\]\)",
    re.DOTALL,
)
WEB_STALE = re.compile(
    r"const STALE_REASONS = new Set<AttentionRankRecomputeStaleReason>\(\[(.*?)\]\)",
    re.DOTALL,
)
WEB_MEMBER = re.compile(r"'([a-z_]+)'")
RUST_STALE = re.compile(r'self\.stale\(&job,\s*"([a-z_]+)"\)')
WORKER = ROOT / "magician-comms/src/channel_assist/attention_learning/rank_recompute.rs"


def read(path: Path) -> str:
    if not path.exists():
        raise SystemExit(f"missing required file: {path.relative_to(ROOT)}")
    return path.read_text(encoding="utf-8")


def rust_labels() -> set[str]:
    return set(RUST_CONST.findall(read(RUST)))


def fixture_labels() -> tuple[set[str], dict[str, str]]:
    api = json.loads(read(FIXTURE))["api_contract"]
    return set(api["result_semantics"]), api["result_semantics_by_universe"]


def web_labels() -> set[str]:
    match = WEB_SET.search(read(WEB))
    if not match:
        raise SystemExit(
            "could not find the SEMANTICS set in the web parser; if it was "
            "renamed, update this guard rather than deleting it"
        )
    return set(WEB_MEMBER.findall(match.group(1)))


def run() -> list[str]:
    failures: list[str] = []
    rust = rust_labels()
    fixture, by_universe = fixture_labels()
    web = web_labels()

    if len(rust) != 2:
        failures.append(
            f"expected two backend semantics constants, found {sorted(rust)}"
        )
    for label, found in (("frozen fixture", fixture), ("web parser", web)):
        if found != rust:
            missing = sorted(rust - found)
            extra = sorted(found - rust)
            detail = []
            if missing:
                detail.append(f"missing {missing}")
            if extra:
                detail.append(f"unknown {extra}")
            failures.append(f"{label} disagrees with the backend: {'; '.join(detail)}")

    # Each provenance maps to one label, and between them they cover the set, so
    # no provenance can go unnamed.
    if set(by_universe) != {"served", "recomputed"}:
        failures.append(
            f"result_semantics_by_universe must key on served/recomputed, found {sorted(by_universe)}"
        )
    elif set(by_universe.values()) != rust:
        failures.append(
            "result_semantics_by_universe does not cover the backend labels: "
            f"{sorted(by_universe.values())} vs {sorted(rust)}"
        )

    api = json.loads(read(FIXTURE))["api_contract"]
    fixture_stale = set(api["stale_reason_codes"])
    worker_stale = set(RUST_STALE.findall(read(WORKER)))
    web_stale_match = WEB_STALE.search(read(WEB))
    if not web_stale_match:
        failures.append("could not find the STALE_REASONS set in the web parser")
    else:
        web_stale = set(WEB_MEMBER.findall(web_stale_match.group(1)))
        if web_stale != fixture_stale:
            failures.append(
                "web stale reasons disagree with the frozen fixture: "
                f"missing {sorted(fixture_stale - web_stale)}; "
                f"unknown {sorted(web_stale - fixture_stale)}"
            )
        missing_worker = sorted(worker_stale - fixture_stale)
        if missing_worker:
            failures.append(
                "frozen fixture is missing worker stale reasons: "
                f"{missing_worker}"
            )
    return failures


def main() -> int:
    failures = run()
    if failures:
        print("rank-recompute semantics drift:", file=sys.stderr)
        for failure in failures:
            print(f"  - {failure}", file=sys.stderr)
        return 1
    print(
        "rank-recompute semantics guard passed: backend, frozen fixture, and web parser agree"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
