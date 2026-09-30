#!/usr/bin/env python3
"""List installed AgentSkills v1 skills with name, kind, and description.

Walks the precedence-ordered skill roots under the live runtime root:

    workspace: $MAGICIAN_ROOT_DIR/scopes/<principal>/<workspace>/skills/<name>/
    system:    $MAGICIAN_ROOT_DIR/system/skills/<name>/

Produces one line per installed skill, with a tag indicating whether the
skill came from the workspace or system layer and whether it's a
procedure or personality-mode skill.

Usage:
  list_skills.py [--data-root <path>] [--scope <principal>/<workspace>]
                 [--source-only] [--json]

Without --scope, prints only system skills. With --scope, prints
workspace-layer skills first (highest precedence), then system fallback,
and notes which workspace-layer entries shadow system entries.

Without --data-root, defaults to `$MAGICIAN_ROOT_DIR`, then
`$MAGICIAN_STORAGE_PATH`, then `~/MagicianNotes`. With
--source-only, reads from skillshub/ instead — useful for previewing
what would land at install time.
"""
from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from runtime_root_shim import legacy_runtime_root  # noqa: E402

REPO_ROOT = Path(__file__).resolve().parents[2]
SKILLSHUB_DIR = REPO_ROOT / "skillshub"


def workspace_skills_root(data_root: Path, scope: str) -> Path:
    return data_root / "scopes" / scope / "skills"


def parse_frontmatter(skill_md: Path) -> dict | None:
    text = skill_md.read_text(errors="ignore")
    if not text.lstrip("\ufeff").startswith("---"):
        return None
    try:
        import yaml  # type: ignore
    except ImportError:
        # Stdlib-only fallback: parse name + description with regex. Sufficient
        # for the listing use case; magician's loader does the strict parse.
        body = text.split("---", 2)
        if len(body) < 3:
            return None
        fm = body[1]
        out: dict = {}
        for key in ("name", "description"):
            m = re.search(rf"^{key}:\s*(.+)$", fm, re.MULTILINE)
            if m:
                out[key] = m.group(1).strip().strip('"').strip("'")
        if "personality:" in fm:
            out["__kind"] = "personality-mode"
        else:
            out["__kind"] = "procedure"
        return out
    body = text.split("---", 2)
    if len(body) < 3:
        return None
    parsed = yaml.safe_load(body[1]) or {}
    if not isinstance(parsed, dict):
        return None
    mag = (parsed.get("metadata") or {}).get("magician") or {}
    if mag.get("personality"):
        parsed["__kind"] = "personality-mode"
    else:
        parsed["__kind"] = "procedure"
    return parsed


def discover(skill_root: Path) -> list[dict]:
    if not skill_root.is_dir():
        return []
    out: list[dict] = []
    for entry in sorted(skill_root.iterdir()):
        skill_md = entry / "SKILL.md"
        if not skill_md.is_file():
            continue
        fm = parse_frontmatter(skill_md)
        if not fm:
            continue
        out.append(
            {
                "name": fm.get("name") or entry.name,
                "description": fm.get("description", ""),
                "kind": fm.get("__kind", "procedure"),
                "path": str(entry),
            }
        )
    return out


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--data-root")
    parser.add_argument(
        "--scope",
        help="Workspace scope <principal>/<workspace> (e.g. anonymous/default)",
    )
    parser.add_argument(
        "--source-only",
        action="store_true",
        help="List the skillshub/ source instead of the installed runtime tree.",
    )
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args()

    if args.source_only:
        skills = discover(SKILLSHUB_DIR)
        layers = [("source", skills)]
    else:
        data_root = Path(
            args.data_root
            if args.data_root
            else legacy_runtime_root(owner="list_skills")
        )
        layers: list[tuple[str, list[dict]]] = []
        if args.scope:
            layers.append(
                (
                    "workspace",
                    discover(workspace_skills_root(data_root, args.scope)),
                ),
            )
        layers.append(("system", discover(data_root / "system" / "skills")))

    if args.json:
        merged: dict[str, dict] = {}
        for layer, skills in layers:
            for skill in skills:
                if skill["name"] in merged:
                    continue  # higher-precedence layer already provided it
                skill["layer"] = layer
                merged[skill["name"]] = skill
        print(json.dumps(sorted(merged.values(), key=lambda x: x["name"]), indent=2))
        return 0

    seen: set[str] = set()
    for layer, skills in layers:
        if not skills:
            continue
        print(f"=== {layer} ({len(skills)} skill{'s' if len(skills) != 1 else ''}) ===")
        for skill in skills:
            tag = " [shadowed]" if skill["name"] in seen else ""
            seen.add(skill["name"])
            kind_pad = "procedure       " if skill["kind"] == "procedure" else "personality-mode"
            desc = skill["description"]
            if len(desc) > 80:
                desc = desc[:79].rstrip() + "…"
            print(f"  {skill['name']:36}  {kind_pad}  {desc}{tag}")
        print()

    if not seen:
        print("(no skills installed)", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
