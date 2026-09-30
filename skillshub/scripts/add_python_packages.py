#!/usr/bin/env python3
"""Add `metadata.magician.requires.python_packages` to skills with python facades.

Walks every `skillshub/<name>/scripts/*.py`, extracts top-level
`import X` and `from X import Y` statements, filters out the Python
stdlib + relative imports, and writes the remaining package names into
the parent skill's SKILL.md frontmatter under
`metadata.magician.requires.python_packages`.

Idempotent — if the field is already present, the script merges
existing values with newly-discovered ones (deduped, sorted) and
rewrites in place. Safe to re-run.

Built for Phase 3 [P3-3] closure: AgentSkills v1 has no standard
field for python deps, so we extend `metadata.magician.requires.*`.
"""
from __future__ import annotations

import ast
import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
SKILLSHUB_DIR = REPO_ROOT / "skillshub"

# Python 3.11+ stdlib modules we actually see imported by our scripts.
# Conservative — better to over-include than miss a real pip dep.
STDLIB_PREFIXES = {
    "abc", "argparse", "ast", "asyncio", "base64", "collections",
    "concurrent", "contextlib", "copy", "csv", "dataclasses", "datetime",
    "email", "enum", "functools", "glob", "hashlib", "html", "http",
    "io", "ipaddress", "itertools", "json", "logging", "math", "mimetypes",
    "operator", "os", "pathlib", "platform", "random", "re",
    "secrets", "shlex", "shutil", "signal", "socket", "sqlite3", "ssl",
    "string", "struct", "subprocess", "sys", "tempfile", "textwrap",
    "threading", "time", "traceback", "types", "typing", "unicodedata",
    "urllib", "uuid", "warnings", "weakref", "xml", "zipfile",
    "__future__",
}

# Map import-name → install name when they differ.
PIP_NAME_OVERRIDES = {
    "bs4": "beautifulsoup4",
    "PIL": "Pillow",
    "exa_py": "exa-py",
    "google": "google-genai",  # google.genai imports
    "tavily": "tavily-python",
}


def extract_imports(source: str) -> set[str]:
    """Return the set of top-level package names imported by `source`."""
    try:
        tree = ast.parse(source)
    except SyntaxError:
        return set()
    out: set[str] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for alias in node.names:
                out.add(alias.name.split(".")[0])
        elif isinstance(node, ast.ImportFrom):
            if node.level and node.level > 0:
                continue  # relative import (no install name)
            if node.module:
                out.add(node.module.split(".")[0])
    return out


def to_install_packages(imports: set[str]) -> list[str]:
    pkgs: set[str] = set()
    for imp in imports:
        if imp in STDLIB_PREFIXES:
            continue
        pkgs.add(PIP_NAME_OVERRIDES.get(imp, imp))
    return sorted(pkgs)


def parse_frontmatter(text: str) -> tuple[dict, str, str]:
    """Split a SKILL.md into (frontmatter dict, raw frontmatter, body).

    Uses a regex split rather than yaml.safe_load to avoid PyYAML's
    aggressive normalisation of multi-line literal scalars (`|` blocks)
    that we want to preserve byte-for-byte.
    """
    if not text.startswith("---"):
        raise ValueError("missing frontmatter")
    parts = text.split("---", 2)
    if len(parts) < 3:
        raise ValueError("unterminated frontmatter")
    raw_fm = parts[1]
    body = parts[2]
    try:
        import yaml  # type: ignore

        fm = yaml.safe_load(raw_fm) or {}
    except ImportError:
        fm = {}
    if not isinstance(fm, dict):
        fm = {}
    return fm, raw_fm, body


def merge_python_packages(
    raw_fm: str, existing_packages: list[str], new_packages: list[str]
) -> str:
    """Insert / replace `python_packages: [...]` under `requires:`.

    Preserves the rest of the frontmatter byte-for-byte. Operates
    string-side rather than re-emitting YAML, because re-emitting via
    PyYAML re-flows multi-line literals and we don't want to touch the
    personality `voice: |` blocks etc.
    """
    merged = sorted(set(existing_packages) | set(new_packages))
    if not merged:
        return raw_fm
    inline_list = "[" + ", ".join(f'"{p}"' for p in merged) + "]"

    # Case 1: `python_packages:` already exists under requires — replace
    # its value. Anchor to the exact key + indent we wrote on previous runs.
    pp_pattern = re.compile(
        r"(^[ \t]+python_packages:[ \t]*).*$",
        re.MULTILINE,
    )
    if pp_pattern.search(raw_fm):
        return pp_pattern.sub(rf"\g<1>{inline_list}", raw_fm)

    # Case 2: a `requires:` block exists — append python_packages to it.
    # Anchor on the indent of `requires:` and only consume children that
    # are indented STRICTLY DEEPER (so we don't run into sibling keys
    # like `install_hint:`). Use the same child indent for the new line.
    requires_pattern = re.compile(
        r"(^(?P<lead>[ \t]+)requires:[ \t]*\n)"
        r"(?P<children>(?:(?P=lead)[ \t]+[a-z_]+:.*\n)*)",
        re.MULTILINE,
    )
    match = requires_pattern.search(raw_fm)
    if match:
        # Determine the child indent. If there are existing children, copy
        # their indent. Else default to lead + two spaces.
        children = match.group("children")
        child_indent_match = re.match(r"^([ \t]+)", children) if children else None
        child_indent = (
            child_indent_match.group(1) if child_indent_match else match.group("lead") + "  "
        )
        new_line = f"{child_indent}python_packages: {inline_list}\n"
        return raw_fm[: match.end()] + new_line + raw_fm[match.end() :]

    # Case 3: no `requires:` block — graft a minimal one under
    # `metadata.magician:`. If there's no `metadata.magician` either,
    # graft that too.
    if re.search(r"^[ \t]+magician:", raw_fm, re.MULTILINE):
        return re.sub(
            r"(^[ \t]+magician:\s*\n)",
            lambda m: f"{m.group(1)}    requires:\n      python_packages: {inline_list}\n",
            raw_fm,
            count=1,
        )

    if "metadata:" in raw_fm:
        return raw_fm.rstrip("\n") + (
            f"\n  magician:\n    requires:\n      python_packages: {inline_list}\n"
        )

    # Skill has no metadata block at all — append a minimal one.
    return raw_fm.rstrip("\n") + (
        f"\nmetadata:\n  magician:\n    requires:\n      python_packages: {inline_list}\n"
    )


def existing_python_packages(fm: dict) -> list[str]:
    requires = (fm.get("metadata") or {}).get("magician", {}).get("requires", {})
    if not isinstance(requires, dict):
        return []
    pkgs = requires.get("python_packages") or []
    if not isinstance(pkgs, list):
        return []
    return [str(p) for p in pkgs]


def update_skill(skill_dir: Path, dry_run: bool) -> tuple[str, str]:
    skill_md = skill_dir / "SKILL.md"
    if not skill_md.is_file():
        return ("no-skill-md", skill_dir.name)

    scripts_dir = skill_dir / "scripts"
    if not scripts_dir.is_dir():
        return ("no-scripts", skill_dir.name)

    py_files = sorted(scripts_dir.glob("*.py"))
    if not py_files:
        return ("no-py-scripts", skill_dir.name)

    imports: set[str] = set()
    for py in py_files:
        imports |= extract_imports(py.read_text(errors="ignore"))
    new_packages = to_install_packages(imports)
    if not new_packages:
        return ("no-deps", skill_dir.name)

    text = skill_md.read_text()
    fm, raw_fm, body = parse_frontmatter(text)
    existing = existing_python_packages(fm)
    merged_raw_fm = merge_python_packages(raw_fm, existing, new_packages)

    if merged_raw_fm == raw_fm:
        return ("unchanged", skill_dir.name)

    new_text = "---" + merged_raw_fm + "---" + body
    if dry_run:
        print(f"--- DRY-RUN: {skill_md.relative_to(REPO_ROOT)} ---")
        print(new_text)
        return ("planned", skill_dir.name)
    skill_md.write_text(new_text)
    return ("updated", f"{skill_dir.name} -> {new_packages}")


def main() -> int:
    import argparse

    parser = argparse.ArgumentParser()
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()

    counts: dict[str, int] = {}
    for entry in sorted(SKILLSHUB_DIR.iterdir()):
        if not entry.is_dir() or entry.name == "scripts":
            continue
        status, msg = update_skill(entry, args.dry_run)
        counts[status] = counts.get(status, 0) + 1
        if status in {"updated", "planned"}:
            print(f"  [{status:14}] {msg}")

    print()
    for status, n in sorted(counts.items()):
        print(f"  {status:18} {n}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
