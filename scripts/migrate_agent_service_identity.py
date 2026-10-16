#!/usr/bin/env python3
"""Remove backend-service attribution from materialized agent definitions.

The packaged templates are the source of truth, but scoped definitions are
durable user data and are not replaced wholesale.  This migration therefore
uses exact, idempotent phrase replacements and preserves every unrelated byte.
"""

from __future__ import annotations

import argparse
from pathlib import Path


REPLACEMENTS = (
    ("You are Loom, Magician's dedicated brainstorming facilitator.",
     "You are the dedicated brainstorming facilitator."),
    ("You are Scribe, Magician's writing assistant.",
     "You are the contextual writing assistant."),
    ("You are the CEO operating harness for Magician.",
     "You are the CEO operating harness for the product."),
    ("building and growing Magician this week", "building and growing the product this week"),
    ("You lead Magician's marketing organization.", "You lead the product's marketing organization."),
    ("You own Magician's product operating loop.", "You own the product operating loop."),
    ("Magician's own usage", "the product's own usage"),
    ("You own Magician's revenue operating loop.", "You own the product's revenue operating loop."),
    ("You lead Magician's engineering organization", "You lead the product's engineering organization"),
    ("Magician's Pi-backed `run_coding_task` flow", "the managed Pi-backed `run_coding_task` flow"),
    ("You are Relay, the Harness SRE for Magician.", "You are the Harness SRE for the autonomous runtime."),
    ("Magician internal diagnostics:", "Backend-service diagnostics:"),
    ("You are the Internal Diagnostic Agent for Magician.",
     "You are the Internal Diagnostic Agent for the backend service."),
    ("Magician runtime health", "backend runtime health"),
    ("Magician operations", "backend operations"),
    ("magician's operational evidence", "the backend's operational evidence"),
    ("Magician internals", "backend internals"),
    ("a Magician diff-approval proposal", "a runtime-managed diff-approval proposal"),
    ("returns a Magician diff-approval proposal", "returns a runtime-managed diff-approval proposal"),
    ("magician's own analytics surface", "the backend's internal analytics surface"),
    ("Magician uses its profile-reusing default mode", "the runtime uses its profile-reusing default mode"),
    ("If they want Magician to work on it", "If they want their assistant to work on it"),
    ("$ref:code_distill_system:1.0.0", "$ref:code_distill_system:1.0.1"),
)


def definition_files(root: Path) -> list[Path]:
    if root.is_file():
        return [root]
    return sorted(root.glob("scopes/*/*/agent_runtime/agents/*/definition.agent.yaml"))


def migrate(path: Path, check: bool) -> tuple[bool, int]:
    original = path.read_text(encoding="utf-8")
    migrated = original
    replacements = 0
    for old, new in REPLACEMENTS:
        count = migrated.count(old)
        replacements += count
        migrated = migrated.replace(old, new)
    changed = migrated != original
    if changed and not check:
        path.write_text(migrated, encoding="utf-8")
    return changed, replacements


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "roots",
        nargs="*",
        type=Path,
        default=[Path("magician_data_v3")],
        help="Data roots or individual definition.agent.yaml files",
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help="Report definitions that still need migration without writing them",
    )
    args = parser.parse_args()

    files = [path for root in args.roots for path in definition_files(root)]
    changed_files = 0
    replacement_count = 0
    for path in files:
        changed, replacements = migrate(path, args.check)
        if changed:
            changed_files += 1
            replacement_count += replacements
            verb = "needs migration" if args.check else "migrated"
            print(f"{verb}: {path} ({replacements} replacement(s))")

    print(
        f"agent service-identity migration: scanned={len(files)} "
        f"changed={changed_files} replacements={replacement_count}"
    )
    return 1 if args.check and changed_files else 0


if __name__ == "__main__":
    raise SystemExit(main())
