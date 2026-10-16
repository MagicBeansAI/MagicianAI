#!/usr/bin/env python3
"""Classify every capability pack into a migration class.

Reads magician_data_v3/system/capability_templates/packs/*.yaml and writes
a one-row-per-pack classification to stdout. Drives Phase 3 conversion.

Classes:
- pure_prose         : guide-only, no parameters / scripts / commands / Python.
- cli_facade         : declares CLI invocation (has `tools:` mapping a tool to
                        a binary, OR has parameters but no Python source file
                        and no runtime backing).
- python_facade      : has a Python file under
                        magician_data_v3/system/capability_templates/tools/python/<pack>.py
                        OR `tools.<verb>.runtime: python` in its YAML.
- runtime_primitive  : invokes magician's own runtime (vault/ledger/agents/
                        memory/tasks). Heuristic: matches a known list of
                        runtime-only tool names.
- mixed              : everything else; needs human classification.
- already_migrated   : a same-named folder already exists at skillshub/<name>/.
"""
from __future__ import annotations

import sys
from pathlib import Path

try:
    import yaml
except ImportError:
    print("ERROR: PyYAML required (pip install pyyaml)", file=sys.stderr)
    sys.exit(2)

REPO_ROOT = Path(__file__).resolve().parents[2]
PACKS_DIR = REPO_ROOT / "magician_data_v3" / "system" / "capability_templates" / "packs"
PYTHON_TOOLS_DIR = REPO_ROOT / "magician_data_v3" / "system" / "capability_templates" / "tools" / "python"
SKILLSHUB_DIR = REPO_ROOT / "skillshub"

# Tool names whose entire purpose is to read/write magician's own state.
# These don't fit cleanly as procedures in the AgentSkills model — they're
# either runtime primitives (always available) or get exposed via dedicated
# magician HTTP endpoints rather than skill scripts.
RUNTIME_PRIMITIVE_NAMES = {
    "create_agent", "create_dashboard", "create_proposal", "create_task",
    "delegation_files", "evaluate_harness", "inspect_agent",
    "list_agents", "list_episodes", "list_proposals",
    "notify_owner", "read_trace", "reassign_task", "retire_agent",
    "system_status", "task_state", "treasurer",
    "unpublish_dashboard", "update_agent", "update_delegation",
}


def classify(pack_yaml: Path) -> tuple[str, str]:
    """Return (class, one-line note)."""
    name = pack_yaml.stem
    kebab = name.replace("_", "-")

    if (SKILLSHUB_DIR / kebab).exists():
        return ("already_migrated", f"skillshub/{kebab}/ exists")

    if name in RUNTIME_PRIMITIVE_NAMES:
        return ("runtime_primitive", "magician-internal state operation")

    try:
        spec = yaml.safe_load(pack_yaml.read_text()) or {}
    except yaml.YAMLError as e:
        return ("error", f"YAML parse error: {e}")

    has_parameters = bool(spec.get("parameters"))
    has_tools_block = bool(spec.get("tools"))
    has_command = bool(spec.get("command"))
    has_python = (PYTHON_TOOLS_DIR / f"{name}.py").exists()

    # tools block can declare runtime: python explicitly
    declares_python_runtime = False
    for tool in (spec.get("tools") or {}).values() if isinstance(spec.get("tools"), dict) else []:
        if isinstance(tool, dict) and tool.get("runtime") == "python":
            declares_python_runtime = True

    if has_python or declares_python_runtime:
        return ("python_facade", f"Python source at tools/python/{name}.py" if has_python else "tools.runtime: python")

    if has_command or has_tools_block:
        return ("cli_facade", "tools/command block defined")

    if has_parameters:
        return ("cli_facade", "parameters declared but no obvious backing — assume CLI facade pending review")

    return ("pure_prose", "guide-only — no parameters/scripts/python")


def main() -> int:
    if not PACKS_DIR.is_dir():
        print(f"ERROR: packs dir not found: {PACKS_DIR}", file=sys.stderr)
        return 2

    rows: list[tuple[str, str, str]] = []
    for pack in sorted(PACKS_DIR.glob("*.yaml")):
        cls, note = classify(pack)
        rows.append((pack.stem, cls, note))

    by_class: dict[str, int] = {}
    for _, c, _ in rows:
        by_class[c] = by_class.get(c, 0) + 1

    # Wide name column so it's readable regardless of pack name length.
    name_w = max(len(n) for n, _, _ in rows) + 2
    cls_w = max(len(c) for _, c, _ in rows) + 2

    print(f"{'PACK':{name_w}} {'CLASS':{cls_w}} NOTES")
    print(f"{'-' * (name_w - 2):{name_w}} {'-' * (cls_w - 2):{cls_w}} {'-' * 60}")
    for name, cls, note in rows:
        print(f"{name:{name_w}} {cls:{cls_w}} {note}")
    print()
    print(f"TOTAL: {len(rows)} packs")
    for cls in sorted(by_class):
        print(f"  {cls:22} {by_class[cls]:>4}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
