#!/usr/bin/env python3
"""Audit the Phase 0 LLM call-path and operation-family coverage contracts."""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path
from typing import Any
import pathlib

# The router's profiles and operation_mapping live in a sibling
# `llm-router.yaml`; reading the config file alone yields neither.
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from magician_config_text import read_config_text  # noqa: E402



REQUIRED_ENTRY_FIELDS = {
    "id",
    "classification",
    "source_path",
    "source_marker",
    "logical_boundary",
    "queue_behavior",
    "current_capture",
    "phase1_adapter",
    "training_eligibility",
}
ALLOWED_CLASSIFICATIONS = {
    "queued",
    "direct",
    "streaming",
    "realtime_media",
    "external_ai_run",
    "excluded",
}


def load_json(path: Path) -> dict[str, Any]:
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise ValueError(f"{path} must contain a JSON object")
    return value


def operation_mapping_keys(path: Path) -> set[str]:
    """Read the first llm.router operation_mapping without a YAML dependency."""
    lines = read_config_text(path).splitlines()
    start = next(
        (index for index, line in enumerate(lines) if line == "    operation_mapping:"),
        None,
    )
    if start is None:
        raise ValueError(f"{path} has no four-space llm.router operation_mapping")
    keys: set[str] = set()
    key_re = re.compile(r"^      ([a-zA-Z0-9_-]+):(?:\s|$)")
    for line in lines[start + 1 :]:
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        indent = len(line) - len(line.lstrip(" "))
        if indent <= 4:
            break
        match = key_re.match(line)
        if match:
            keys.add(match.group(1))
    return keys


def discovered_paths(repo_root: Path, roots: list[str], file_glob: str, literal: str) -> set[str]:
    matches: set[str] = set()
    for relative_root in roots:
        root = repo_root / relative_root
        if not root.is_dir():
            continue
        for source in root.rglob(file_glob):
            if literal in source.read_text(encoding="utf-8", errors="replace"):
                matches.add(source.relative_to(repo_root).as_posix())
    return matches


# --- Operation-key code scan -------------------------------------------------
#
# The config/ledger diff above cannot see an operation key that exists only in
# Rust: it silently falls back to the router default profile and no audit fires.
# This scan closes that mechanism (not just the four known instances): every
# operation key referenced in PRODUCTION code must be present in the
# operation-family contract.
#
# Key forms in production use:
#   * `LLMOperation::Other("op_key".to_string())` literals;
#   * `const …OPERATION…: &str = "op_key"` constants (named *_OPERATION* so
#     prompt-template constants like `CHANNEL_CLASSIFY_SYSTEM` don't match).
#
# Test code is excluded: `mod tests` modules (also as sibling `tests.rs` /
# `tests/` files), and any item gated by a test-only `#[cfg(...)]`
# (`#[cfg(test)]`, `#[cfg(any(test, feature = "test-fixtures"))]`, …).

CODE_SCAN_ROOTS = [
    "magician/src",
    "magician-comms/src",
    "magician-api/src",
    "magician-core/src",
]
OPERATION_LITERAL_RE = re.compile(r'LLMOperation::Other\(\s*"([a-z0-9_]+)"')
OPERATION_CONST_RE = re.compile(r'const\s+[A-Z0-9_]*OPERATION[A-Z0-9_]*\s*:\s*&str\s*=\s*"([a-z0-9_]+)"')
MOD_TESTS_RE = re.compile(r"^\s*(pub\s+)?mod\s+tests\b")
# `mod tests;` (semicolon — body in a sibling tests.rs) is a complete item;
# disarm a pending test-region flag so the next production item is scanned.
MOD_TESTS_SEMI_RE = re.compile(r"^\s*(pub\s+)?mod\s+tests\s*;")
CFG_ATTR_START_RE = re.compile(r"#\[\s*cfg\s*\(")
RUST_STRING_RE = re.compile(r'"(?:[^"\\]|\\.)*"')
NOT_TEST_RE = re.compile(r"\bnot\s*\(\s*test\s*\)")
BARE_TEST_RE = re.compile(r"\btest\b")


def _line_comment_start(line: str) -> int | None:
    in_str = False
    escape = False
    for i, c in enumerate(line):
        if in_str:
            if escape:
                escape = False
            elif c == "\\":
                escape = True
            elif c == '"':
                in_str = False
        else:
            if c == '"':
                in_str = True
            elif c == "/" and i + 1 < len(line) and line[i + 1] == "/":
                return i
    return None


def _code_portion(line: str) -> str:
    cut = _line_comment_start(line)
    return line if cut is None else line[:cut]


def _strip_strings(code: str) -> str:
    return RUST_STRING_RE.sub('""', code)


def _is_test_cfg(code: str) -> bool:
    for match in CFG_ATTR_START_RE.finditer(code):
        depth = 0
        i = match.end() - 1
        start = i + 1
        while i < len(code):
            c = code[i]
            if c == "(":
                depth += 1
            elif c == ")":
                depth -= 1
                if depth == 0:
                    predicates = RUST_STRING_RE.sub('""', code[start:i])
                    predicates = NOT_TEST_RE.sub("", predicates)
                    if BARE_TEST_RE.search(predicates):
                        return True
                    break
            i += 1
    return False


def _is_test_file(relative_path: str) -> bool:
    parts = relative_path.split("/")
    return "tests" in parts or parts[-1] == "tests.rs"


def production_operation_keys(repo_root: Path) -> dict[str, list[tuple[str, int]]]:
    """Operation keys referenced in production code, keyed to their sites."""
    keys: dict[str, list[tuple[str, int]]] = {}
    for relative_root in CODE_SCAN_ROOTS:
        root = repo_root / relative_root
        if not root.is_dir():
            continue
        for source in sorted(root.rglob("*.rs")):
            rel = source.relative_to(repo_root).as_posix()
            if _is_test_file(rel):
                continue
            try:
                lines = source.read_text(encoding="utf-8", errors="replace").splitlines()
            except OSError:
                continue
            cfg_test_pending = False
            depth = 0
            test_region_starts: list[int] = []
            for lineno, raw in enumerate(lines, start=1):
                code = _code_portion(raw)
                code_no_str = _strip_strings(code)
                if _is_test_cfg(code) or MOD_TESTS_RE.match(code):
                    cfg_test_pending = True
                if cfg_test_pending and MOD_TESTS_SEMI_RE.match(code):
                    cfg_test_pending = False
                for ch in code_no_str:
                    if ch == "{":
                        if cfg_test_pending:
                            test_region_starts.append(depth)
                            cfg_test_pending = False
                        depth += 1
                    elif ch == "}":
                        depth -= 1
                        while test_region_starts and depth <= test_region_starts[-1]:
                            test_region_starts.pop()
                if test_region_starts:
                    continue
                for pattern in (OPERATION_LITERAL_RE, OPERATION_CONST_RE):
                    for match in pattern.finditer(code):
                        keys.setdefault(match.group(1), []).append((rel, lineno))
    return keys


def audit(repo_root: Path) -> dict[str, Any]:
    data_root = repo_root / "data/magician_v2/llm_observability"
    ledger = load_json(data_root / "coverage-ledger-v1.json")
    families = load_json(data_root / "operation-families-v1.json")
    sink_contract = load_json(data_root / "sink-contract-v1.json")
    errors: list[str] = []

    entries = ledger.get("entries")
    if not isinstance(entries, list) or not entries:
        errors.append("coverage ledger must contain a non-empty entries array")
        entries = []
    ids: set[str] = set()
    for index, entry in enumerate(entries):
        label = f"coverage entry #{index + 1}"
        if not isinstance(entry, dict):
            errors.append(f"{label} is not an object")
            continue
        missing = sorted(REQUIRED_ENTRY_FIELDS - entry.keys())
        if missing:
            errors.append(f"{label} missing fields: {', '.join(missing)}")
        entry_id = str(entry.get("id", ""))
        if not entry_id:
            errors.append(f"{label} has an empty id")
        elif entry_id in ids:
            errors.append(f"duplicate coverage entry id: {entry_id}")
        ids.add(entry_id)
        classification = entry.get("classification")
        if classification not in ALLOWED_CLASSIFICATIONS:
            errors.append(f"{entry_id}: unknown classification {classification!r}")
        source_path = repo_root / str(entry.get("source_path", ""))
        if not source_path.is_file():
            errors.append(f"{entry_id}: source path does not exist: {source_path}")
            continue
        marker = str(entry.get("source_marker", ""))
        if not marker:
            errors.append(f"{entry_id}: source marker is required")
        elif marker not in source_path.read_text(encoding="utf-8", errors="replace"):
            errors.append(f"{entry_id}: source marker not found in {entry['source_path']}: {marker!r}")
        for field in ("logical_boundary", "queue_behavior", "current_capture", "phase1_adapter", "training_eligibility"):
            if not str(entry.get(field, "")).strip():
                errors.append(f"{entry_id}: {field} must be explicit")

    probes = ledger.get("discovery_probes")
    if not isinstance(probes, list) or not probes:
        errors.append("coverage ledger must contain production-boundary discovery probes")
        probes = []
    probe_ids: set[str] = set()
    for probe in probes:
        if not isinstance(probe, dict):
            errors.append("coverage discovery probe is not an object")
            continue
        probe_id = str(probe.get("id", ""))
        if not probe_id or probe_id in probe_ids:
            errors.append(f"invalid or duplicate discovery probe id: {probe_id!r}")
        probe_ids.add(probe_id)
        roots = probe.get("roots")
        allowed = probe.get("allowed_paths")
        literal = str(probe.get("literal", ""))
        file_glob = str(probe.get("file_glob", ""))
        if not isinstance(roots, list) or not roots or not literal or not file_glob:
            errors.append(f"{probe_id}: roots, file_glob and literal are required")
            continue
        if not isinstance(allowed, dict) or not allowed:
            errors.append(f"{probe_id}: allowed_paths must be a non-empty object")
            continue
        actual_paths = discovered_paths(repo_root, [str(root) for root in roots], file_glob, literal)
        expected_paths = set(allowed)
        if unknown := sorted(actual_paths - expected_paths):
            errors.append(f"{probe_id}: newly discovered production paths need ownership: {unknown}")
        if stale := sorted(expected_paths - actual_paths):
            errors.append(f"{probe_id}: declared paths no longer match the discovery marker: {stale}")
        for path, owner in allowed.items():
            if owner != "infrastructure" and owner not in ids:
                errors.append(f"{probe_id}: {path} references unknown coverage entry {owner!r}")

    validator_ids: set[str] = set()
    validators = families.get("validators")
    if not isinstance(validators, list) or not validators:
        errors.append("operation-family contract must contain validators")
        validators = []
    for validator in validators:
        if not isinstance(validator, dict):
            errors.append("validator row is not an object")
            continue
        validator_id = str(validator.get("id", ""))
        if not validator_id or validator_id in validator_ids:
            errors.append(f"invalid or duplicate validator id: {validator_id!r}")
        validator_ids.add(validator_id)
        for source in validator.get("source_paths", []):
            if not (repo_root / str(source)).is_file():
                errors.append(f"{validator_id}: validator source does not exist: {source}")
        if not validator.get("immediate_signals") or not validator.get("delayed_signals"):
            errors.append(f"{validator_id}: immediate and delayed signals are required")

    assigned: dict[str, str] = {}
    family_rows = families.get("families")
    if not isinstance(family_rows, list) or not family_rows:
        errors.append("operation-family contract must contain families")
        family_rows = []
    for family in family_rows:
        if not isinstance(family, dict):
            errors.append("operation family row is not an object")
            continue
        family_id = str(family.get("id", ""))
        validator_id = str(family.get("validator_id", ""))
        if validator_id not in validator_ids:
            errors.append(f"{family_id}: unknown validator {validator_id!r}")
        for operation in family.get("operations", []):
            operation = str(operation)
            if operation in assigned:
                errors.append(
                    f"operation {operation!r} belongs to both {assigned[operation]!r} and {family_id!r}"
                )
            assigned[operation] = family_id

    configured_operations = operation_mapping_keys(repo_root / "magician-config.yaml")
    assigned_operations = set(assigned)
    if missing := sorted(configured_operations - assigned_operations):
        errors.append(f"operation-family contract is missing configured operations: {missing}")
    if extra := sorted(assigned_operations - configured_operations):
        errors.append(f"operation-family contract has unknown operations: {extra}")

    # Production-code closure: an operation key that exists only in Rust never
    # enters the checks above and silently rides the router default profile.
    code_keys = production_operation_keys(repo_root)
    if unmapped := sorted(set(code_keys) - assigned_operations):
        for key in unmapped:
            sites = ", ".join(f"{path}:{line}" for path, line in code_keys[key][:3])
            errors.append(
                f"operation key {key!r} is used in production code but absent from the "
                f"operation-family contract (first sites: {sites})"
            )

    sinks = sink_contract.get("sinks")
    if not isinstance(sinks, list) or not sinks:
        errors.append("sink contract must contain sinks")
        sinks = []
    for sink in sinks:
        sink_id = str(sink.get("id", ""))
        source = repo_root / str(sink.get("source_path", ""))
        if not source.is_file():
            errors.append(f"{sink_id}: sink source does not exist: {source}")
        if not sink.get("loss_points"):
            errors.append(f"{sink_id}: current loss points must be explicit")
        if not sink.get("phase2_requirement"):
            errors.append(f"{sink_id}: Phase 2 replacement requirement is missing")

    classification_counts = {
        name: sum(1 for entry in entries if entry.get("classification") == name)
        for name in sorted(ALLOWED_CLASSIFICATIONS)
    }
    return {
        "schema_version": 1,
        "status": "passed" if not errors else "failed",
        "entry_count": len(entries),
        "discovery_probe_count": len(probes),
        "classification_counts": classification_counts,
        "operation_count": len(configured_operations),
        "family_count": len(family_rows),
        "validator_count": len(validators),
        "sink_count": len(sinks),
        "errors": errors,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--repo-root",
        type=Path,
        default=Path(__file__).resolve().parents[1],
    )
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args()
    result = audit(args.repo_root.resolve())
    if args.json:
        print(json.dumps(result, indent=2, sort_keys=True))
    else:
        print(
            "LLM trace coverage: "
            f"{result['status']} — {result['entry_count']} boundaries, "
            f"{result['operation_count']} operations, {result['family_count']} families, "
            f"{result['validator_count']} validators"
        )
        for error in result["errors"]:
            print(f"  - {error}", file=sys.stderr)
    return 0 if result["status"] == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
