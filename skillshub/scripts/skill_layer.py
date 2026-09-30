#!/usr/bin/env python3
"""Categorize each skillshub skill as scope-layer or system-layer.

The principle: skills that need any per-deployment secret/auth state
(API keys, OAuth credentials, interactive login) install to the
**scope** layer (`<scope>/skills/<skill>/`) so each principal+workspace
gets its own isolated copy with its own `config/.env`. Skills that
need none of that (pure-shell tools, personalities) install to the
**system** layer (`<data_root>/system/skills/<skill>/`) where every
scope shares one copy.

A skill needs scope if any of these are true:
1. `config/.env.example` exists  (declares required env keys)
2. `SKILL.md` frontmatter has a governed runtime auth kind other than `none`
3. `SKILL.md` frontmatter `metadata.magician.requires.env` is non-empty

Usage as a module:
    from skill_layer import categorize_skills
    system, scope = categorize_skills(Path("skillshub"))

Usage as a CLI:
    python3 skillshub/scripts/skill_layer.py            # print both lists
    python3 skillshub/scripts/skill_layer.py --layer scope    # only scope
    python3 skillshub/scripts/skill_layer.py --layer system   # only system
"""
from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

import yaml

REPO = Path(__file__).resolve().parents[2]


def has_env_example(skill_dir: Path) -> bool:
    return (skill_dir / "config" / ".env.example").is_file()


def has_requires_env(skill_dir: Path) -> bool:
    skill_md = skill_dir / "SKILL.md"
    if not skill_md.is_file():
        return False
    text = skill_md.read_text()
    # Look for `env: ["KEY", ...]` or YAML list form. Empty list is fine.
    m = re.search(r"(?m)^\s+env:\s*(\[.*?\]|\n(?:\s+-\s+.+\n)+)", text)
    if not m:
        return False
    body = m.group(1).strip()
    if body == "[]":
        return False
    # Either inline list with at least one entry, or a YAML list with
    # at least one `- "KEY"` line.
    if body.startswith("["):
        # `["A", "B"]` — non-empty if there's any quote inside.
        return bool(re.search(r'"[A-Z_][A-Z0-9_]*"', body))
    return bool(re.search(r"(?m)^\s+-\s+\S+", body))


def governed_auth_kind(skill_dir: Path) -> str | None:
    """Return the declared universal-runtime auth kind, when present.

    Only the bounded frontmatter is parsed. A malformed package is left to the
    canonical skill validator; this classifier merely declines to infer an auth
    kind instead of creating a second validation surface.
    """
    skill_md = skill_dir / "SKILL.md"
    if not skill_md.is_file():
        return None
    source = skill_md.read_text(encoding="utf-8", errors="strict")
    if not source.startswith("---\n"):
        return None
    marker = source.find("\n---\n", 4)
    if marker < 0:
        return None
    try:
        document = yaml.safe_load(source[4:marker])
    except yaml.YAMLError:
        return None
    if not isinstance(document, dict):
        return None
    metadata = document.get("metadata")
    magician = metadata.get("magician") if isinstance(metadata, dict) else None
    contract = magician.get("runtime_contract") if isinstance(magician, dict) else None
    auth = contract.get("auth") if isinstance(contract, dict) else None
    kind = auth.get("kind") if isinstance(auth, dict) else None
    return kind.strip() if isinstance(kind, str) and kind.strip() else None


def needs_scope(skill_dir: Path) -> bool:
    auth_kind = governed_auth_kind(skill_dir)
    return (
        has_env_example(skill_dir)
        or auth_kind not in (None, "none")
        or has_requires_env(skill_dir)
    )


def categorize_skills(skillshub_root: Path) -> tuple[list[Path], list[Path]]:
    """Walk skillshub/ and return (system_skills, scope_skills) — sorted."""
    system: list[Path] = []
    scope: list[Path] = []
    if not skillshub_root.is_dir():
        return system, scope
    for entry in sorted(skillshub_root.iterdir()):
        if not entry.is_dir():
            continue
        if not (entry / "SKILL.md").is_file():
            continue
        if needs_scope(entry):
            scope.append(entry)
        else:
            system.append(entry)
    return system, scope


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--skillshub", default=str(REPO / "skillshub"))
    parser.add_argument("--layer", choices=["system", "scope", "both"], default="both")
    parser.add_argument("--names-only", action="store_true",
                        help="emit just basenames, one per line (for shell loops)")
    args = parser.parse_args()

    skillshub_root = Path(args.skillshub)
    system, scope = categorize_skills(skillshub_root)

    if args.names_only:
        targets = []
        if args.layer in ("system", "both"):
            targets.extend(p.name for p in system)
        if args.layer in ("scope", "both"):
            targets.extend(p.name for p in scope)
        for n in targets:
            print(n)
        return 0

    if args.layer in ("system", "both"):
        print(f"system layer ({len(system)} skill(s)):")
        for p in system:
            print(f"  {p.name}")
    if args.layer == "both":
        print()
    if args.layer in ("scope", "both"):
        print(f"scope layer ({len(scope)} skill(s)):")
        for p in scope:
            reasons = []
            if has_env_example(p):
                reasons.append(".env.example")
            auth_kind = governed_auth_kind(p)
            if auth_kind not in (None, "none"):
                reasons.append(f"runtime auth:{auth_kind}")
            if has_requires_env(p):
                reasons.append("requires.env")
            print(f"  {p.name}  ({', '.join(reasons)})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
