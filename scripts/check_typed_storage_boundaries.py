#!/usr/bin/env python3
"""Task 21 ratchet: reject new implicit-path storage bypasses.

Production code may construct typed backends and resolve MAGICIAN_ROOT_DIR only
inside reviewed adapters, owner kits, backup/export, the magician-bin composition
root, and the remaining local-compatibility call sites listed in
``scripts/typed_storage_boundary_allowlist.yaml``.

Rules
-----
ambient_backend
    ``StorageRuntime`` / ``LocalStorage`` / ``S3ObjectStore`` / ``SqlitePool``
    construction outside the composition root and reviewed adapters.
parquet_glob
    DuckDB ``read_parquet(`` outside cataloged dataset/analytics owners.
object_sdk
    Direct AWS/object-store SDK symbols outside ``magician-storage-s3``.
runtime_root
    ``default_storage_base_path`` / runtime-root env resolution outside the
    reviewed local-compatibility set.
bin_isolation
    ``magician-bin`` must depend on ``magician-storage`` and must not depend on
    ``magician-storage-s3``, ``magician-storage-state``, or
    ``magician-storage-migration``. Direct S3/state/migration crate deps are
    confined to those crates (migration may also be used by ``magician``).

Raw on-disk ``Connection::open`` remains ``scripts/storage_catalog_guard.py``.
Durable-write adoption remains ``scripts/check_store_durability_adoption.py``.
Skillshub runtime-root resolution remains
``scripts/skillshub_runtime_root_linter.py``.

Exact-file allowlist entries are two-way. Prefix trees are policy.

Runs from ``make check-typed-storage-boundaries`` (part of ``make check-all``)
and from ``scripts/docs_guard.py``.
"""

from __future__ import annotations

import argparse
import re
import sys
import tempfile
from pathlib import Path
from typing import Any, Iterable

import yaml

ROOT = Path(__file__).resolve().parents[1]
ALLOWLIST_PATH = Path(__file__).resolve().parent / "typed_storage_boundary_allowlist.yaml"

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
    "magician-storage/src",
    "magician-storage-s3/src",
    "magician-storage-state/src",
    "magician-storage-migration/src",
    "magicutor/src",
    "magician-decision/src",
    "decision-engine/src",
    "decision-engine-contract/src",
    "magician-media/src",
    "magicllm/src",
    "magician-surfaces/src",
    "magician-mcp-client/src",
    "magician-chunking/src",
    "magician-pty/src",
    "magician-event-taxonomy/src",
    "magician-app-contract/src",
    "magic-supervisor/src",
    "magician-storage-gate1/src",
    "document-to-markdown-cli/src",
    "kindle/src",
)

SKIP_DIR_NAMES = {
    "tests",
    "target",
    "node_modules",
    "examples",
    ".git",
    ".extracted",  # Pinned external sources are governed in their own repositories.
}

RULES = {
    "ambient_backend": re.compile(
        r"StorageRuntime::open(?:_local)?\s*\("
        r"|StorageRuntime::install\s*\("
        r"|LocalStorage::open(?:_with_scratch_quota)?\s*\("
        r"|S3ObjectStore::"
        r"|SqlitePool::open\s*\("
        r"|open_from_profile\s*\("
    ),
    "parquet_glob": re.compile(r"read_parquet\s*\("),
    "object_sdk": re.compile(
        r'(?<!["\'])(?:aws_sdk_s3|aws-sdk-s3|rusoto_s3|rusoto::s3)\b'
        r'|(?<!["\'])object_store::'
    ),
    "runtime_root": re.compile(
        r"default_storage_base_path\s*\("
        r"|runtime_root_env\s*\("
        r"|MAGICIAN_ROOT_DIR_ENV"
        r"|MAGICIAN_STORAGE_PATH_ENV"
        r"""|(?:std::)?env::var(?:_os)?\(\s*["']MAGICIAN_(?:ROOT_DIR|STORAGE_PATH)["']"""
        r"""|"MAGICIAN_ROOT_DIR"\s*\.to_string\(\)"""
        r"|ROOT_ENV_KEYS"
        r"""|"MAGICIAN_(?:ROOT_DIR|STORAGE_PATH)"\s*(?:\||=>|,)"""
    ),
}

OBJECT_SDK_CRATES = ("aws-sdk-s3", "rusoto_s3", "rusoto-s3", "object_store")
REMOTE_STORAGE_CRATES = (
    "magician-storage-s3",
    "magician-storage-state",
    "magician-storage-migration",
)


def load_allowlist(path: Path = ALLOWLIST_PATH) -> dict[str, Any]:
    data = yaml.safe_load(path.read_text(encoding="utf-8"))
    if not isinstance(data, dict):
        raise ValueError(f"allowlist must be a mapping: {path}")
    return data


def path_allowed(rel: str, prefixes: Iterable[str], files: Iterable[str]) -> bool:
    if rel in set(files):
        return True
    for prefix in prefixes:
        if prefix.endswith(".rs"):
            if rel == prefix:
                return True
            continue
        folder = prefix if prefix.endswith("/") else prefix + "/"
        if rel.startswith(folder):
            return True
    return False


def iter_rust_files(repo_root: Path, scan_roots: Iterable[str] = SCAN_ROOTS) -> list[Path]:
    files: list[Path] = []
    for rel_root in scan_roots:
        base = repo_root / rel_root
        if not base.is_dir():
            continue
        for path in base.rglob("*.rs"):
            if any(part in SKIP_DIR_NAMES for part in path.parts):
                continue
            files.append(path)
    return sorted(files)


def hits_in_text(text: str, pattern: re.Pattern[str], rel: str) -> list[str]:
    found: list[str] = []
    for match in pattern.finditer(text):
        line = text.count("\n", 0, match.start()) + 1
        snippet = match.group(0).splitlines()[0][:80]
        found.append(f"{rel}:{line}: {snippet}")
    return found


def scan_rule(
    repo_root: Path,
    rule: str,
    pattern: re.Pattern[str],
    prefixes: Iterable[str],
    files: list[str],
    scan_roots: Iterable[str] = SCAN_ROOTS,
) -> tuple[list[str], list[str]]:
    unmatched: list[str] = []
    matched_files: set[str] = set()
    listed = list(files)
    for path in iter_rust_files(repo_root, scan_roots):
        rel = path.relative_to(repo_root).as_posix()
        try:
            text = path.read_text(encoding="utf-8")
        except UnicodeDecodeError:
            continue
        file_hits = hits_in_text(text, pattern, rel)
        if not file_hits:
            continue
        if path_allowed(rel, prefixes, listed):
            matched_files.add(rel)
            continue
        unmatched.extend(f"{rule}: {hit}" for hit in file_hits)
    stale = [
        f"{rule}: stale allowlist file {rel} (no remaining match)"
        for rel in listed
        if rel not in matched_files and not path_allowed(rel, prefixes, [])
    ]
    return unmatched, stale


def _dependency_names(text: str) -> set[str]:
    names: set[str] = set()
    in_deps = False
    for raw in text.splitlines():
        line = raw.strip()
        if line.startswith("[") and line.endswith("]"):
            in_deps = "dependencies" in line.lower()
            continue
        if not in_deps or not line or line.startswith("#"):
            continue
        name = line.split("=", 1)[0].strip().strip('"').strip("'")
        if name:
            names.add(name)
    return names


def scan_manifests(repo_root: Path, allowlist: dict[str, Any]) -> list[str]:
    failures: list[str] = []
    bin_forbidden = list(allowlist.get("bin_forbidden_crates") or REMOTE_STORAGE_CRATES)
    bin_manifests = list(allowlist.get("bin_forbidden_manifests") or ["magician-bin/Cargo.toml"])
    migration_allowed = set(
        allowlist.get("migration_allowed_manifests")
        or ["magician/Cargo.toml", "magician-storage-migration/Cargo.toml"]
    )
    object_sdk_allowed = set(
        allowlist.get("object_sdk_allowed_manifests") or ["magician-storage-s3/Cargo.toml"]
    )

    for rel in bin_manifests:
        path = repo_root / rel
        if not path.is_file():
            failures.append(f"bin_isolation: missing manifest {rel}")
            continue
        text = path.read_text(encoding="utf-8")
        deps = _dependency_names(text)
        if "magician-storage" not in deps and "magician-storage" not in text:
            failures.append(f"bin_isolation: {rel} must depend on magician-storage")
        for crate in bin_forbidden:
            if crate in deps or re.search(rf"^[^#]*{re.escape(crate)}\s*=", text, re.M):
                failures.append(f"bin_isolation: {rel} must not depend on {crate}")
        main = repo_root / "magician-bin/src/main.rs"
        if main.is_file():
            main_text = main.read_text(encoding="utf-8")
            if "StorageRuntime::open_local" not in main_text:
                failures.append(
                    "bin_isolation: magician-bin/src/main.rs must construct StorageRuntime::open_local"
                )
            for crate in ("magician_storage_s3", "magician_storage_state", "magician_storage_migration"):
                if crate in main_text:
                    failures.append(
                        f"bin_isolation: magician-bin/src/main.rs must not mention {crate}"
                    )

    for path in sorted(repo_root.rglob("Cargo.toml")):
        rel = path.relative_to(repo_root).as_posix()
        if any(part in SKIP_DIR_NAMES for part in path.relative_to(repo_root).parts):
            continue
        text = path.read_text(encoding="utf-8")
        deps = _dependency_names(text)
        for crate in OBJECT_SDK_CRATES:
            crate_key = crate.replace("-", "_") if crate.startswith("rusoto") else crate
            if crate in deps or crate_key in deps:
                if rel not in object_sdk_allowed:
                    failures.append(f"object_sdk: {rel} depends on {crate}")
        if "magician-storage-s3" in deps and rel not in object_sdk_allowed:
            failures.append(f"object_sdk: {rel} depends on magician-storage-s3")
        if "magician-storage-state" in deps and rel != "magician-storage-state/Cargo.toml":
            failures.append(f"ambient_backend: {rel} depends on magician-storage-state")
        if "magician-storage-migration" in deps and rel not in migration_allowed:
            failures.append(f"ambient_backend: {rel} depends on magician-storage-migration")
    return failures


def catalog_database_ratchet_present(repo_root: Path) -> list[str]:
    catalog_path = repo_root / "docs/components/magician/storage-catalog.yaml"
    if not catalog_path.is_file():
        return ["raw_database_open: missing docs/components/magician/storage-catalog.yaml"]
    catalog = yaml.safe_load(catalog_path.read_text(encoding="utf-8"))
    files = (catalog or {}).get("measurements", {}).get("direct_io_files") or []
    if not files:
        return [
            "raw_database_open: catalog measurements.direct_io_files is empty; "
            "scripts/storage_catalog_guard.py owns the Connection::open allowlist"
        ]
    return []


def scan(repo_root: Path, allowlist: dict[str, Any] | None = None) -> list[str]:
    allowlist = allowlist or load_allowlist(repo_root / "scripts" / "typed_storage_boundary_allowlist.yaml")
    failures: list[str] = []
    failures.extend(catalog_database_ratchet_present(repo_root))
    failures.extend(scan_manifests(repo_root, allowlist))
    for rule, pattern in RULES.items():
        spec = allowlist.get(rule) or {}
        prefixes = list(spec.get("prefixes") or [])
        files = list(spec.get("files") or [])
        unmatched, stale = scan_rule(repo_root, rule, pattern, prefixes, files)
        failures.extend(unmatched)
        failures.extend(stale)
    return failures


def self_test() -> None:
    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        allow = {
            "bin_forbidden_crates": list(REMOTE_STORAGE_CRATES),
            "bin_forbidden_manifests": ["magician-bin/Cargo.toml"],
            "migration_allowed_manifests": [
                "magician/Cargo.toml",
                "magician-storage-migration/Cargo.toml",
            ],
            "object_sdk_allowed_manifests": ["magician-storage-s3/Cargo.toml"],
            "ambient_backend": {
                "prefixes": ["magician-storage/", "magician-storage-s3/"],
                "files": ["magician-bin/src/main.rs"],
            },
            "parquet_glob": {
                "prefixes": ["magician/src/magician_v2/analytics/"],
                "files": [],
            },
            "object_sdk": {"prefixes": ["magician-storage-s3/"], "files": []},
            "runtime_root": {
                "prefixes": ["magician/src/magician_v2/artifact_v2/workspace.rs"],
                "files": ["magician-bin/src/main.rs"],
            },
        }
        (root / "magician-bin/src").mkdir(parents=True)
        (root / "magician/src/evil").mkdir(parents=True)
        (root / "magician-storage/src").mkdir(parents=True)
        (root / "docs/components/magician").mkdir(parents=True)
        (root / "magician-bin/Cargo.toml").write_text(
            '[package]\nname = "magician-bin"\n[dependencies]\nmagician-storage = { path = "../magician-storage" }\n',
            encoding="utf-8",
        )
        (root / "magician-bin/src/main.rs").write_text(
            "fn main() {\n    magician_storage::StorageRuntime::open_local(root, profile, owner);\n    let _ = default_storage_base_path();\n}\n",
            encoding="utf-8",
        )
        (root / "magician-storage/src/lib.rs").write_text(
            "pub fn open() { let _ = LocalStorage::open(root); }\n",
            encoding="utf-8",
        )
        (root / "docs/components/magician/storage-catalog.yaml").write_text(
            "measurements:\n  direct_io_files:\n  - path: magician/src/x.rs\n    owner_id: example\n",
            encoding="utf-8",
        )
        (root / "magician/src/evil/bypass.rs").write_text(
            'fn evil() {\n    StorageRuntime::open_local(root, profile, owner);\n    let _ = read_parquet("x/*.parquet");\n    let _ = default_storage_base_path();\n    let _ = aws_sdk_s3::Client::new();\n}\n',
            encoding="utf-8",
        )
        hits = scan(root, allow)
        joined = "\n".join(hits)
        for needle in (
            "ambient_backend: magician/src/evil/bypass.rs",
            "parquet_glob: magician/src/evil/bypass.rs",
            "runtime_root: magician/src/evil/bypass.rs",
            "object_sdk: magician/src/evil/bypass.rs",
        ):
            if needle not in joined:
                raise AssertionError(f"self-test missed {needle}: {hits}")
        if any("magician-storage/src/lib.rs" in hit for hit in hits):
            raise AssertionError(f"reviewed adapter prefix must pass: {hits}")
        if any("magician-bin/src/main.rs" in hit and "stale" not in hit for hit in hits):
            raise AssertionError(f"composition root must pass: {hits}")

        (root / "magician-bin/Cargo.toml").write_text(
            '[dependencies]\nmagician-storage = { path = "../magician-storage" }\nmagician-storage-s3 = { path = "../magician-storage-s3" }\n',
            encoding="utf-8",
        )
        hits = scan(root, allow)
        if not any("magician-storage-s3" in hit for hit in hits):
            raise AssertionError(f"bin isolation missed s3 dep: {hits}")

        (root / "desktop/src-tauri").mkdir(parents=True)
        (root / "desktop/src-tauri/Cargo.toml").write_text(
            '[package]\nname = "desktop"\n[dependencies]\naws-sdk-s3 = "1"\n',
            encoding="utf-8",
        )
        hits = scan(root, allow)
        if not any("desktop/src-tauri/Cargo.toml" in hit for hit in hits):
            raise AssertionError(f"nested manifest object SDK missed: {hits}")


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Reject new implicit-path storage bypasses (Task 21)."
    )
    parser.add_argument("--repo-root", default=str(ROOT), help="repository root")
    parser.add_argument(
        "--skip-self-test",
        action="store_true",
        help="Skip the fixture that proves a new bypass is rejected.",
    )
    args = parser.parse_args()
    if not args.skip_self_test:
        try:
            self_test()
        except AssertionError as exc:
            sys.stderr.write(f"typed-storage boundary self-test failed: {exc}\n")
            return 1
    repo_root = Path(args.repo_root).resolve()
    try:
        allowlist = load_allowlist(repo_root / "scripts" / "typed_storage_boundary_allowlist.yaml")
    except FileNotFoundError:
        sys.stderr.write("typed-storage boundary allowlist missing.\n")
        return 1
    hits = scan(repo_root, allowlist)
    if hits:
        sys.stderr.write(
            "typed-storage boundary ratchet failed. New runtime-root I/O, Parquet "
            "globs, object SDKs, or ambient backend construction must go through "
            "reviewed adapters (see scripts/typed_storage_boundary_allowlist.yaml).\n"
        )
        for hit in hits:
            sys.stderr.write(f"  {hit}\n")
        return 1
    print("typed-storage boundary ratchet passed.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
