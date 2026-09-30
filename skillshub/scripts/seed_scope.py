#!/usr/bin/env python3
"""Seed a scope's runtime tree directories so the rest of the setup
chain has a layout to write into without booting the magician daemon.

This handles the directory-creation portion of Rust's
`materialize_scope` (in `capabilities.rs`) — bot bundle deployment is
done separately by `make -C skillshub install-bot-bundles`. Useful
during development or after `clean-legacy-scope` to verify the new
layout is built correctly from scratch.

What this seeds (idempotent — never overwrites existing content):

  <scope>/bots/                            (mkdir)
  <scope>/bots/bot_configs.yaml            ← copied from skillshub/bots/
  <scope>/auth/                            (mkdir)
  <scope>/workdirs/                        (mkdir)
  <scope>/workdirs/home/                   (mkdir, scope-isolated $HOME)
  <scope>/skills/                          (mkdir, for workspace-layer
                                            skill overrides — empty by
                                            default)

Usage:
  python3 skillshub/scripts/seed_scope.py
  python3 skillshub/scripts/seed_scope.py --scope alice/staging
"""
from __future__ import annotations

import argparse
import shutil
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
DEFAULT_SCOPE = "anonymous/default"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--scope", default=DEFAULT_SCOPE)
    parser.add_argument("--data-root", default=str(REPO / "magician_data_v3"))
    args = parser.parse_args()

    scope_root = Path(args.data_root) / "scopes" / args.scope
    skillshub_bot_configs = REPO / "skillshub" / "bots" / "bot_configs.yaml"

    print(f"== {scope_root} ==")

    for sub in ("bots", "auth", "workdirs", "workdirs/home", "skills"):
        d = scope_root / sub
        existed = d.exists()
        d.mkdir(parents=True, exist_ok=True)
        print(f"  {'—' if existed else '✓'} {sub}/")

    bot_configs_dst = scope_root / "bots" / "bot_configs.yaml"
    if bot_configs_dst.exists():
        print(f"  — bots/bot_configs.yaml (preserved)")
    elif not skillshub_bot_configs.is_file():
        print(f"  ✗ skillshub/bots/bot_configs.yaml missing — cannot seed")
        return 1
    else:
        shutil.copy2(skillshub_bot_configs, bot_configs_dst)
        print(f"  ✓ bots/bot_configs.yaml (copied from {skillshub_bot_configs})")

    print()
    print(f"Scope ready. Next:")
    print(f"  make -C skillshub setup-gws-accounts SCOPE={args.scope}")
    print(f"  make -C skillshub install-bot-bundles SCOPE={args.scope}")
    print(f"  make -C skillshub setup-bot-envs    SCOPE={args.scope}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
