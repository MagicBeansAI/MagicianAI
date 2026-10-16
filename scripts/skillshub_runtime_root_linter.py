#!/usr/bin/env python3
"""Reject new skillshub runtime-root resolution outside the approved shim.

Rust source guards cannot see Python or shell skills. This linter is the Task
16A ratchet: launched skill code must not resolve ``MAGICIAN_ROOT_DIR`` or
``MAGICIAN_STORAGE_PATH`` except through ``skillshub/scripts/runtime_root_shim.py``.
It runs from ``scripts/docs_guard.py`` (the same CI and pre-commit gate).

Markdown is not scanned. Operator ``skillshub/Makefile`` remains the install
bootstrap and is allowlisted. In-tree ``REPO / magician_data_v3`` seed paths
are not live runtime-root resolution.
"""

from __future__ import annotations

import argparse
import re
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SKILLSHUB = ROOT / "skillshub"
APPROVED_SHIM = "skillshub/scripts/runtime_root_shim.py"
ALLOWLIST = {
    APPROVED_SHIM,
    "skillshub/Makefile",
}

PY_PATTERNS = (
    re.compile(
        r"""os\.environ(?:\.get)?\(\s*['"]MAGICIAN_(?:ROOT_DIR|STORAGE_PATH)['"]"""
    ),
    re.compile(r"""os\.getenv\(\s*['"]MAGICIAN_(?:ROOT_DIR|STORAGE_PATH)['"]"""),
)
SH_PATTERNS = (
    re.compile(r"\$\{MAGICIAN_(?:ROOT_DIR|STORAGE_PATH)"),
    re.compile(r"\$MAGICIAN_(?:ROOT_DIR|STORAGE_PATH)"),
    re.compile(r"MAGICIAN_(?:ROOT_DIR|STORAGE_PATH):-"),
)
MAKE_PATTERNS = (
    re.compile(r"\$\(MAGICIAN_(?:ROOT_DIR|STORAGE_PATH)\)"),
)

SCAN_SUFFIXES = {".py", ".sh", ".bash", ".js", ".ts", ".mjs"}


def iter_scan_files(skillshub: Path) -> list[Path]:
    files: list[Path] = []
    makefile = skillshub / "Makefile"
    if makefile.is_file():
        files.append(makefile)
    for path in skillshub.rglob("*"):
        if not path.is_file():
            continue
        if path.suffix.lower() in SCAN_SUFFIXES:
            files.append(path)
    return sorted(files)


def patterns_for(path: Path) -> tuple[re.Pattern[str], ...]:
    if path.name == "Makefile":
        return MAKE_PATTERNS
    if path.suffix.lower() in {".sh", ".bash"}:
        return SH_PATTERNS
    if path.suffix.lower() == ".py":
        return PY_PATTERNS
    return PY_PATTERNS + SH_PATTERNS


def violations_in(path: Path, repo_root: Path) -> list[str]:
    rel = path.relative_to(repo_root).as_posix()
    if rel in ALLOWLIST:
        return []
    try:
        text = path.read_text(encoding="utf-8")
    except UnicodeDecodeError:
        return []
    hits: list[str] = []
    for pattern in patterns_for(path):
        for match in pattern.finditer(text):
            line = text.count("\n", 0, match.start()) + 1
            hits.append(f"{rel}:{line}: {match.group(0)}")
    return hits


def scan(repo_root: Path) -> list[str]:
    skillshub = repo_root / "skillshub"
    if not skillshub.is_dir():
        return [f"skillshub directory missing: {skillshub}"]
    hits: list[str] = []
    for path in iter_scan_files(skillshub):
        hits.extend(violations_in(path, repo_root))
    return hits


def self_test() -> None:
    with tempfile.TemporaryDirectory() as raw:
        root = Path(raw)
        skillshub = root / "skillshub" / "evil-skill"
        skillshub.mkdir(parents=True)
        bypass = skillshub / "run.py"
        bypass.write_text(
            'import os\nroot = os.environ.get("MAGICIAN_ROOT_DIR")\n',
            encoding="utf-8",
        )
        shim = root / "skillshub" / "scripts"
        shim.mkdir(parents=True)
        (shim / "runtime_root_shim.py").write_text(
            'import os\nprint(os.environ.get("MAGICIAN_ROOT_DIR"))\n',
            encoding="utf-8",
        )
        (root / "skillshub" / "Makefile").write_text(
            "DATA_ROOT ?= $(MAGICIAN_ROOT_DIR)\n", encoding="utf-8"
        )
        hits = scan(root)
        if not any("evil-skill/run.py" in hit for hit in hits):
            raise AssertionError(f"linter fixture bypass was not rejected: {hits}")
        if any("runtime_root_shim.py" in hit for hit in hits):
            raise AssertionError(f"approved shim must be allowlisted: {hits}")
        if any("Makefile" in hit for hit in hits):
            raise AssertionError(f"operator Makefile must be allowlisted: {hits}")


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Reject skillshub runtime-root resolution outside the approved shim."
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
            sys.stderr.write(f"skillshub runtime-root linter self-test failed: {exc}\n")
            return 1
    hits = scan(Path(args.repo_root).resolve())
    if hits:
        sys.stderr.write(
            "skillshub runtime-root linter failed. Resolve MAGICIAN_ROOT_DIR / "
            "MAGICIAN_STORAGE_PATH only via skillshub/scripts/runtime_root_shim.py.\n"
        )
        for hit in hits:
            sys.stderr.write(f"  {hit}\n")
        return 1
    print("skillshub runtime-root linter passed.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
