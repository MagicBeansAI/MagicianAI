#!/usr/bin/env python3
"""Sync the desktop skill's `action_name` enum with the installed driver.

`action_name` was an unconstrained string whose description pointed at a
catalog the schema never carried, so a model had to guess a subcommand name.
Codex guessed `activate_app` and then `focus_window` — neither is a driver tool
— and the driver rejects an unknown name as `Permission denied: tool
'activate_app' has no reviewed risk classification`, which reads like a gated
capability rather than a typo; it stalled with nothing left to try. Grok, given
the same single free-text tool, went looking for another agent to own the
capability instead of calling it. An enum makes a wrong name unrepresentable.

The list is the driver's own (`cua-driver list-tools`) plus the three lifecycle
actions this controller serves itself, so it cannot drift from what is
installed. Run after a driver upgrade; `--check` fails when the skill is stale.

usage: sync_desktop_action_enum.py [--check]
"""
from __future__ import annotations

import os
import re
import subprocess
import sys

SKILL = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                     "skillshub", "macos-ui-automation", "SKILL.md")
# `serve` / `status` / `stop` are the controller's own daemon lifecycle verbs;
# the driver exposes them as subcommands, not as listed tools.
CONTROLLER_ACTIONS = ("serve", "status", "stop")


def driver_binary() -> str:
    for candidate in (os.environ.get("MAGICIAN_CUA_DRIVER_BIN"),
                      os.path.expanduser("~/.local/bin/cua-driver"), "cua-driver"):
        if candidate and (os.path.isabs(candidate) is False or os.access(candidate, os.X_OK)):
            return candidate
    return "cua-driver"


def driver_actions() -> list[str]:
    listed = subprocess.run([driver_binary(), "list-tools"], capture_output=True, text=True, timeout=60)
    if listed.returncode != 0:
        raise SystemExit(f"cua-driver list-tools failed: {listed.stderr.strip()[:200]}")
    names = {
        line.split(":", 1)[0].strip()
        for line in listed.stdout.splitlines()
        if line.strip() and not line.startswith((" ", "\t"))
    }
    names.update(CONTROLLER_ACTIONS)
    return sorted(name for name in names if re.fullmatch(r"[a-z_][a-z0-9_]*", name))


def rendered_enum(actions: list[str]) -> str:
    return "              enum_values: [" + ", ".join(actions) + "]\n"


def main() -> int:
    check = "--check" in sys.argv[1:]
    actions = driver_actions()
    source = open(SKILL, encoding="utf-8").read()
    anchor = re.search(
        r"(            action_name:\n              type: string\n)"
        r"(              enum_values: \[[^\]]*\]\n)?",
        source,
    )
    if not anchor:
        raise SystemExit("action_name declaration not found in SKILL.md")
    wanted = rendered_enum(actions)
    if anchor.group(2) == wanted:
        print(f"desktop action enum is current ({len(actions)} actions)")
        return 0
    if check:
        print(f"SKILL.md action_name enum is stale; run {os.path.basename(__file__)}", file=sys.stderr)
        return 1
    updated = source[: anchor.start()] + anchor.group(1) + wanted + source[anchor.end():]
    open(SKILL, "w", encoding="utf-8").write(updated)
    print(f"desktop action enum synced ({len(actions)} actions)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
