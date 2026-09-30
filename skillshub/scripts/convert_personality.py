#!/usr/bin/env python3
"""Convert legacy personality YAMLs to AgentSkills v1 personality-mode skills.

Reads each personality from
  magician_data_v3/system/capability_templates/personality/<name>.yaml
and writes
  skillshub/<kebab-name>/SKILL.md

Each output is a personality-mode skill: the YAML's flat top-level fields
(voice, expression_bias, suppression_rules, expression_triggers,
active_mode, summary) become the `metadata.magician.personality.*`
block. The body is a short operator-facing description derived from the
`summary` field if present, otherwise the first sentence of `voice`.

Skips:
- Personalities whose target skillshub/<kebab>/ already exists
  (witty was hand-converted in T19; rerunning is a no-op).

Run with --dry-run to preview without writing.
"""
from __future__ import annotations

import argparse
import sys
from pathlib import Path

try:
    import yaml
except ImportError:
    print("ERROR: PyYAML required (pip install pyyaml)", file=sys.stderr)
    sys.exit(2)

REPO_ROOT = Path(__file__).resolve().parents[2]
PERSONALITY_DIR = (
    REPO_ROOT
    / "magician_data_v3"
    / "system"
    / "capability_templates"
    / "personality"
)
SKILLSHUB_DIR = REPO_ROOT / "skillshub"

# Skills with hand-authored bodies that we don't want the converter to
# overwrite even on --force runs.
HAND_AUTHORED = {"witty"}

DESCRIPTION_MAX = 1024


def kebab(name: str) -> str:
    return name.lower().replace("_", "-")


def title(name: str) -> str:
    return " ".join(p.capitalize() for p in name.replace("-", " ").split())


def first_sentence(text: str, limit: int = 200) -> str:
    s = text.strip().split(".")[0].strip()
    if len(s) <= limit:
        return s + "."
    return s[: limit - 1].rstrip() + "…"


def truncate(s: str, limit: int) -> str:
    if len(s) <= limit:
        return s
    return s[: limit - 1].rstrip() + "…"


def yaml_inline(value) -> str:
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, (int, float)):
        return str(value)
    if value is None:
        return "null"
    s = str(value)
    return '"' + s.replace("\\", "\\\\").replace('"', '\\"') + '"'


def render_block(block: str, indent: str) -> str:
    """Render a multi-line string as a YAML literal block scalar."""
    text = block.rstrip("\n")
    if not text:
        return '""'
    lines = text.split("\n")
    return "|\n" + "\n".join(f"{indent}{line}".rstrip() for line in lines)


def render_skill_md(name: str, description: str, personality: dict, body: str) -> str:
    inner_indent = "        "
    p = personality
    fm_lines = [
        "---",
        f"name: {yaml_inline(name)}",
        f"description: {yaml_inline(description)}",
        "metadata:",
        "  magician:",
        "    personality:",
        f"      active_mode: {yaml_inline(p.get('active_mode', name))}",
        f"      voice: {render_block(p.get('voice', ''), inner_indent)}",
        f"      expression_bias: {render_block(p.get('expression_bias', ''), inner_indent)}",
        f"      suppression_rules: {render_block(p.get('suppression_rules', ''), inner_indent)}",
        f"      expression_triggers: {render_block(p.get('expression_triggers', ''), inner_indent)}",
        "---",
        "",
        f"# {title(name)}",
        "",
        body.rstrip(),
        "",
    ]
    return "\n".join(fm_lines)


def derive_description(spec: dict, name: str) -> str:
    summary = (spec.get("summary") or "").strip()
    if summary:
        return truncate(summary, DESCRIPTION_MAX)
    voice = (spec.get("voice") or "").strip()
    if voice:
        return truncate(
            f"Persona mode '{name}'. " + first_sentence(voice),
            DESCRIPTION_MAX,
        )
    return f"Persona mode '{name}'."


def derive_body(spec: dict, name: str) -> str:
    summary = (spec.get("summary") or "").strip()
    if summary:
        return summary + "\n"
    voice = (spec.get("voice") or "").strip()
    return f"A persona mode '{name}'. {first_sentence(voice)}\n"


def convert(yaml_path: Path, dry_run: bool, force: bool) -> tuple[str, str]:
    name = yaml_path.stem  # e.g. "true_friend"
    target_name = kebab(name)
    target_dir = SKILLSHUB_DIR / target_name

    if target_name in HAND_AUTHORED:
        return ("skip-hand-authored", target_name)
    if target_dir.exists() and not force:
        return ("skip-exists", target_name)

    try:
        spec = yaml.safe_load(yaml_path.read_text()) or {}
    except yaml.YAMLError as e:
        return ("error", f"{name}: YAML parse: {e}")
    if not isinstance(spec, dict):
        return ("error", f"{name}: not a mapping")

    description = derive_description(spec, target_name)
    body = derive_body(spec, target_name)

    # Always emit the kebab-form name as `active_mode` so the
    # personality_profile memory tier renders the AgentSkills-compliant
    # name everywhere (matches the skill's `name` field and the folder
    # on disk). Legacy YAMLs use underscore form; we deliberately
    # discard that here to keep one canonical name per personality.
    personality = {
        "active_mode": target_name,
        "voice": spec.get("voice", ""),
        "expression_bias": spec.get("expression_bias", ""),
        "suppression_rules": spec.get("suppression_rules", ""),
        "expression_triggers": spec.get("expression_triggers", ""),
    }

    rendered = render_skill_md(target_name, description, personality, body)

    if dry_run:
        print(f"--- DRY-RUN: {target_dir.relative_to(REPO_ROOT)} ---")
        print(rendered)
        return ("planned", target_name)

    target_dir.mkdir(parents=True, exist_ok=True)
    (target_dir / "SKILL.md").write_text(rendered)
    return ("written", target_name)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument(
        "--force",
        action="store_true",
        help="Overwrite existing skillshub/<name>/SKILL.md instead of skipping.",
    )
    parser.add_argument("--only", help="Convert only this personality (e.g. brutal).")
    args = parser.parse_args()

    if not PERSONALITY_DIR.is_dir():
        print(f"ERROR: personality dir not found: {PERSONALITY_DIR}", file=sys.stderr)
        return 2

    counts: dict[str, int] = {}
    for yaml_path in sorted(PERSONALITY_DIR.glob("*.yaml")):
        if args.only and yaml_path.stem != args.only:
            continue
        status, msg = convert(yaml_path, args.dry_run, args.force)
        counts[status] = counts.get(status, 0) + 1
        print(f"  [{status:14}] {msg}")

    print()
    for status, n in sorted(counts.items()):
        print(f"  {status:14} {n}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
