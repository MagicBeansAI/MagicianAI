#!/usr/bin/env python3
"""Canonical storage catalog: schema, governance-inventory drift, and I/O allowlist.

The YAML catalog at docs/components/magician/storage-catalog.yaml is the
source of truth for storage-owner identity, class, tier, and readiness.
The `/storage` snapshot in magician-comms (live) and the test-fixtures copy
in magician/src/magician_v2/storage_governance/mod.rs are a projection:
every StorageEntry.id must appear as exactly one catalog governance_id, and
every catalog governance_id must appear in both Rust copies.

Unknown on-disk `Connection::open` sites fail until they are listed under
measurements.direct_io_files and named as a catalog owner. That is the
Task 0 allowlist; later owner packets tighten it.

Runs from `make check-storage-catalog` and as part of `make check-all`.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path
from typing import Any, Iterable

import yaml


ROOT = Path(__file__).resolve().parents[1]
CATALOG_PATH = ROOT / "docs/components/magician/storage-catalog.yaml"
LIVE_GOVERNANCE = ROOT / "magician-comms/src/channel_assist/governance.rs"
FIXTURE_GOVERNANCE = ROOT / "magician/src/magician_v2/storage_governance/mod.rs"

SCAN_ROOTS = (
    "magician/src",
    "magician-api/src",
    "magician-apps/src",
    "magician-bin/src",
    "magician-comms/src",
    "magician-core/src",
    "magician-vector-index/src",
    "magician-learning/src",
    "runtime-core/src",
    "desktop/src-tauri/src",
)

REQUIRED_OWNER_FIELDS = (
    "id",
    "class",
    "tier",
    "scope",
    "capability",
    "owner_module",
    "lifecycle_owner",
    "current_layout",
    "writers",
    "readers",
    "governance_id",
    "readiness",
    "authority",
    "legacy_source",
)

ALLOWED_CLASSES = {
    "authoritative",
    "lifecycle_managed",
    "regenerable",
    "observability",
    "restricted",
    "secret",
    "ephemeral",
    "bootstrap",
    "external",
    "device_local",
}

ALLOWED_TIERS = {1, 2, "device_local"}
ALLOWED_SCOPES = {"tenant", "agent", "task", "device", "system", "host"}
ALLOWED_CAPABILITIES = {
    "domain_repository",
    "object_store",
    "dataset_store",
    "index_store",
    "lease_store",
    "scratch_store",
    "secret_store",
    "notes_provider",
    "device_local",
    "bootstrap",
}

READINESS_STATES = (
    "discovered",
    "characterized",
    "wrapped_local",
    "callers_routed",
    "bypass_guarded",
    "remote_implemented",
    "remote_conformant",
    "migration_qualified",
    "restore_qualified",
    "remote_ready",
)


def required_readiness_evidence_keys(state: str) -> tuple[str, ...]:
    """Keys required to occupy `state`. `discovered` needs none; later states
    require every predecessor key plus their own (plan §5.12)."""
    if state not in READINESS_STATES:
        return ()
    idx = READINESS_STATES.index(state)
    return READINESS_STATES[1 : idx + 1]


def evidence_link_present(evidence: Any, key: str) -> bool:
    if not isinstance(evidence, dict):
        return False
    value = evidence.get(key)
    if value is None:
        return False
    if isinstance(value, str):
        return bool(value.strip())
    if isinstance(value, dict):
        if not value:
            return False
        for field in ("path", "tests", "commit", "report", "locator"):
            field_val = value.get(field)
            if isinstance(field_val, str) and field_val.strip():
                return True
        return any(isinstance(item, str) and item.strip() for item in value.values())
    return False

AUTHORITY_STATES = {
    "local_active",
    "migration_in_progress",
    "remote_active",
    "rollback_in_progress",
    "local_rolled_back",
}

LEGACY_STATES = {"retained", "retirement_eligible", "retired"}

ENTRY_CALL_RE = re.compile(
    r"(?:database_entry|directory_entry|app_directory_entry)\(\s*[^,]+,\s*"
    r'"([a-z][a-z0-9_]*)"',
    re.S,
)
STORAGE_ENTRY_ID_RE = re.compile(
    r"entries\.push\(\s*StorageEntry\s*\{.*?id:\s*\"([a-z][a-z0-9_]*)\"",
    re.S,
)
FOR_ID_LOOP_RE = re.compile(r"for \(id,[^\)]*\) in \[", re.S)
TUPLE_FIRST_STRING_RE = re.compile(r"\(\s*\"([a-z][a-z0-9_]*)\"\s*,")
ON_DISK_OPEN_RE = re.compile(r"Connection::open(?:_with_flags)?\s*\(")


def production_text(text: str) -> str:
    """Keep source above the first test/test-fixtures cfg, matching other guards."""
    kept: list[str] = []
    test_cfg = "#[cfg(test)]"
    any_test_cfg = "#[cfg(any(test"
    for line in text.splitlines(True):
        stripped = line.lstrip()
        if stripped.startswith(test_cfg) or stripped.startswith(any_test_cfg):
            break
        kept.append(line)
    return "".join(kept)


def strip_line_comments(text: str) -> str:
    return re.sub(r"//[^\n]*", "", text)


def extract_governance_ids_from_source(text: str) -> set[str]:
    text = strip_line_comments(text)
    ids: set[str] = set()
    ids.update(ENTRY_CALL_RE.findall(text))
    ids.update(STORAGE_ENTRY_ID_RE.findall(text))
    for match in FOR_ID_LOOP_RE.finditer(text):
        start = match.end()
        depth = 1
        idx = start
        while idx < len(text) and depth:
            if text[idx] == "[":
                depth += 1
            elif text[idx] == "]":
                depth -= 1
            idx += 1
        block = text[start : idx - 1]
        ids.update(TUPLE_FIRST_STRING_RE.findall(block))
    return ids


def _read_required(path: Path) -> str:
    if not path.is_file():
        raise SystemExit(f"storage catalog guard: missing required file: {path}")
    return path.read_text(encoding="utf-8")


def extract_live_governance_ids() -> set[str]:
    return extract_governance_ids_from_source(_read_required(LIVE_GOVERNANCE))


def extract_fixture_governance_ids() -> set[str]:
    return extract_governance_ids_from_source(_read_required(FIXTURE_GOVERNANCE))


def load_catalog(path: Path = CATALOG_PATH) -> dict[str, Any]:
    if not path.is_file():
        raise SystemExit(f"storage catalog guard: missing catalog: {path}")
    loaded = yaml.safe_load(path.read_text(encoding="utf-8"))
    if not isinstance(loaded, dict):
        raise SystemExit(f"storage catalog guard: catalog is not a mapping: {path}")
    return loaded


def _as_list(value: Any) -> list[Any]:
    if value is None:
        return []
    if isinstance(value, list):
        return value
    return [value]


def validate_catalog(
    catalog: dict[str, Any],
    rust_governance_ids: set[str],
) -> list[str]:
    failures: list[str] = []
    if catalog.get("schema_version") != 1:
        failures.append("catalog schema_version must be 1")
    authority = catalog.get("authority") or {}
    if authority.get("source_of_truth") != "catalog_yaml":
        failures.append("authority.source_of_truth must be catalog_yaml")
    if authority.get("governance_inventory") != "projection":
        failures.append("authority.governance_inventory must be projection")

    owners = catalog.get("owners")
    if not isinstance(owners, list) or not owners:
        failures.append("catalog must list at least one owner")
        return failures

    seen: set[str] = set()
    catalog_governance: set[str] = set()
    for index, owner in enumerate(owners):
        prefix = f"owners[{index}]"
        if not isinstance(owner, dict):
            failures.append(f"{prefix} must be a mapping")
            continue
        owner_id = owner.get("id")
        if not owner_id:
            failures.append(f"{prefix} is missing id")
            continue
        if owner_id in seen:
            failures.append(f"duplicate owner id: {owner_id}")
        seen.add(owner_id)
        for field in REQUIRED_OWNER_FIELDS:
            if field not in owner:
                failures.append(f"{owner_id} is missing required field {field}")
        class_name = owner.get("class")
        if class_name not in ALLOWED_CLASSES:
            failures.append(f"{owner_id} has unknown class {class_name!r}")
        tier = owner.get("tier")
        if tier not in ALLOWED_TIERS:
            failures.append(f"{owner_id} has unknown tier {tier!r}")
        if owner.get("scope") not in ALLOWED_SCOPES:
            failures.append(f"{owner_id} has unknown scope {owner.get('scope')!r}")
        if owner.get("capability") not in ALLOWED_CAPABILITIES:
            failures.append(
                f"{owner_id} has unknown capability {owner.get('capability')!r}"
            )
        if not owner.get("lifecycle_owner"):
            failures.append(f"{owner_id} is missing lifecycle_owner")
        readiness = owner.get("readiness") or {}
        state = readiness.get("state")
        if state not in READINESS_STATES:
            failures.append(f"{owner_id} has unknown readiness state {state!r}")
        evidence = readiness.get("evidence") or {}
        if state != "discovered" and not evidence:
            failures.append(
                f"{owner_id} readiness {state} requires evidence"
            )
        if state in READINESS_STATES:
            for key in required_readiness_evidence_keys(state):
                if not evidence_link_present(evidence, key):
                    failures.append(
                        f"{owner_id} readiness {state} missing evidence {key}"
                    )
        authority_state = (owner.get("authority") or {}).get("state")
        if authority_state not in AUTHORITY_STATES:
            failures.append(
                f"{owner_id} has unknown authority state {authority_state!r}"
            )
        legacy = (owner.get("legacy_source") or {}).get("state")
        if legacy not in LEGACY_STATES:
            failures.append(f"{owner_id} has unknown legacy_source state {legacy!r}")
        governance_id = owner.get("governance_id")
        if governance_id:
            if governance_id in catalog_governance:
                failures.append(f"duplicate governance_id: {governance_id}")
            catalog_governance.add(governance_id)

    extra_rust = sorted(rust_governance_ids - catalog_governance)
    extra_catalog = sorted(catalog_governance - rust_governance_ids)
    for item in extra_rust:
        failures.append(
            f"Rust storage-governance inventory id {item} is missing from the catalog"
        )
    for item in extra_catalog:
        failures.append(
            f"catalog governance_id {item} is missing from the Rust inventory"
        )

    measurements = catalog.get("measurements") or {}
    owner_count = measurements.get("owner_count")
    if owner_count is not None and owner_count != len(owners):
        failures.append(
            f"measurements.owner_count is {owner_count}, catalog has {len(owners)} owners"
        )
    gov_count = measurements.get("governance_entry_count")
    if gov_count is not None and gov_count != len(rust_governance_ids):
        failures.append(
            f"measurements.governance_entry_count is {gov_count}, "
            f"Rust inventory has {len(rust_governance_ids)}"
        )
    return failures


def scan_on_disk_connection_opens(
    root: Path, scan_roots: Iterable[str] = SCAN_ROOTS
) -> set[str]:
    found: set[str] = set()
    for rel_root in scan_roots:
        base = root / rel_root
        if not base.is_dir():
            continue
        for path in sorted(base.rglob("*.rs")):
            # Whole-file scan: a `#[cfg(test)]` helper in the middle of a
            # store must not hide later production opens (see
            # docs/components/scripts/source-scanning-guards.md). Test-only
            # opens still identify the owning file for the Task 0 allowlist.
            text = path.read_text(encoding="utf-8", errors="replace")
            if ON_DISK_OPEN_RE.search(text):
                found.add(path.relative_to(root).as_posix())
    return found


def validate_direct_io(catalog: dict[str, Any], scanned: set[str]) -> list[str]:
    failures: list[str] = []
    owners = {
        owner.get("id")
        for owner in _as_list(catalog.get("owners"))
        if isinstance(owner, dict)
    }
    listed: dict[str, str] = {}
    for entry in _as_list((catalog.get("measurements") or {}).get("direct_io_files")):
        if not isinstance(entry, dict):
            failures.append("direct_io_files entries must be mappings")
            continue
        path = entry.get("path")
        owner_id = entry.get("owner_id")
        if not path or not owner_id:
            failures.append(f"direct_io_files entry missing path/owner_id: {entry!r}")
            continue
        if owner_id not in owners:
            failures.append(
                f"direct_io_files path {path} names unknown owner_id {owner_id}"
            )
        listed[path] = owner_id
    for path in sorted(scanned - set(listed)):
        failures.append(
            f"unlisted on-disk Connection::open in {path}; add it to "
            "measurements.direct_io_files with a catalog owner_id"
        )
    for path in sorted(set(listed) - scanned):
        failures.append(
            f"direct_io_files lists {path} but no production Connection::open remains; "
            "remove it from the allowlist"
        )
    return failures


def missing_owner_paths(catalog: dict[str, Any]) -> list[str]:
    missing: list[str] = []
    for owner in _as_list(catalog.get("owners")):
        if not isinstance(owner, dict):
            continue
        owner_id = owner.get("id", "<unknown>")
        for field in ("owner_module",):
            rel = owner.get(field)
            if not rel:
                continue
            path = ROOT / rel
            if not path.exists():
                missing.append(f"{owner_id}.{field} path does not exist: {rel}")
        for field in ("writers", "readers"):
            for rel in _as_list(owner.get(field)):
                if not isinstance(rel, str):
                    continue
                if rel.startswith("skillshub/"):
                    continue
                path = ROOT / rel
                if not path.exists():
                    missing.append(f"{owner_id}.{field} path does not exist: {rel}")
    return missing


def run() -> list[str]:
    live = extract_live_governance_ids()
    fixture = extract_fixture_governance_ids()
    failures: list[str] = []
    if live != fixture:
        only_live = sorted(live - fixture)
        only_fixture = sorted(fixture - live)
        if only_live:
            failures.append(
                "live governance inventory has ids missing from the test-fixtures copy: "
                + ", ".join(only_live)
            )
        if only_fixture:
            failures.append(
                "test-fixtures governance inventory has ids missing from live: "
                + ", ".join(only_fixture)
            )
    catalog = load_catalog()
    failures.extend(validate_catalog(catalog, live))
    scanned = scan_on_disk_connection_opens(ROOT)
    failures.extend(validate_direct_io(catalog, scanned))
    failures.extend(missing_owner_paths(catalog))
    return failures


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--print-scan",
        action="store_true",
        help="print production on-disk Connection::open files and exit",
    )
    parser.add_argument(
        "--print-governance-ids",
        action="store_true",
        help="print live inventory ids and exit",
    )
    args = parser.parse_args()
    if args.print_scan:
        for path in sorted(scan_on_disk_connection_opens(ROOT)):
            print(path)
        return 0
    if args.print_governance_ids:
        for item in sorted(extract_live_governance_ids()):
            print(item)
        return 0
    failures = run()
    if failures:
        print("storage catalog guard failed:", file=sys.stderr)
        for item in failures:
            print(f"  - {item}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
