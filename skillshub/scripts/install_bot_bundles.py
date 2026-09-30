#!/usr/bin/env python3
"""Symlink each bundled bot dist/index.js into a scope's bots/ tree.

Source-of-truth: `skillshub/bots/<bot>/dist/index.js` (built by
`make -C skillshub setup-bots` via esbuild). The scope-level path
is a symlink back to that source.

Per-scope state (what is NOT shared):
  - `<scope>/bots/<bot>/.env*`           - secrets, tokens, OAuth state
  - `<scope>/bots/<bot>/workdirs/`        - any runtime-managed state
  - `<scope>/workdirs/home/.wu/`          - wu-cli session keys & store

Per-scope SHIMS (what is shared via symlink, scope holds only the symlink):
  - `<scope>/bots/<bot>/dist/index.js`    → `skillshub/bots/<bot>/dist/index.js`

Runtime-only npm deps (wu-cli, tgcli, @googleworkspace/cli, etc.) used
to be installed per-scope. They are no longer. Node's default symlink
behavior resolves module imports starting from the *symlink target* —
so an `import "@ibrahimwithi/wu-cli"` inside the symlinked
`<scope>/bots/whatsapp/dist/index.js` resolves through skillshub and
finds the hoisted package at `skillshub/node_modules/@ibrahimwithi/
wu-cli`. The npm workspaces declaration in `skillshub/package.json`
guarantees those deps are installed exactly once at the workspace root.

Migration: existing scopes still carry a legacy shim package.json,
package-lock.json, and 30-85 MB node_modules per bot. This script
removes those on first run — they are redundant and just waste disk.

The whatsapp skill wrapper finds its `wu` binary via
`skillshub/whatsapp/bin/wu` → `skillshub/node_modules/.bin/wu`. That
single symlink lives at the skillshub source layer; no per-scope
shim is needed because the skill's `scripts/magician-wu.mjs` is itself
a symlink to skillshub, and `import.meta.url` resolves through to
the source. Per-scope state stays via `WU_HOME` (set by the runtime).

Usage:
  python3 skillshub/scripts/install_bot_bundles.py
  python3 skillshub/scripts/install_bot_bundles.py --scope alice/staging
  python3 skillshub/scripts/install_bot_bundles.py --all-scopes
"""
from __future__ import annotations

import argparse
import shutil
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
DEFAULT_SCOPE = "anonymous/default"

# Legacy files written by the pre-refactor install path. We remove them
# on every run so scopes converge on the symlink-only layout. Kept
# tightly scoped — we never touch `.env*` or `workdirs/`.
LEGACY_BOT_FILES = ["package.json", "package-lock.json"]
LEGACY_BOT_DIRS = ["node_modules"]


def discover_scopes(data_root: Path) -> list[str]:
    """Enumerate <principal>/<workspace> directory pairs under scopes/.

    Skips the `system/system` bucket: it's the transport-log fall-through
    sink for unscoped diagnostic events (see `transport_log.rs`), not a
    user scope. Installing bots there would copy ~170MB of node_modules
    into a bucket that never runs them.
    """
    out: list[str] = []
    scopes_root = data_root / "scopes"
    if not scopes_root.is_dir():
        return out
    for principal in sorted(scopes_root.iterdir()):
        if not principal.is_dir():
            continue
        for workspace in sorted(principal.iterdir()):
            if not workspace.is_dir():
                continue
            if principal.name == "system" and workspace.name == "system":
                continue
            out.append(f"{principal.name}/{workspace.name}")
    return out


def prune_legacy_runtime_install(bot_name: str, scope_bot_dir: Path) -> None:
    """Remove the legacy per-scope runtime install: shim package.json,
    package-lock.json, and the full node_modules tree. Node's module
    resolution will walk from the symlinked dist/index.js into
    skillshub's hoisted root, so none of these are needed.

    Defensive: only touches the three legacy paths and only if they
    exist. `.env*` files and `workdirs/` are untouched.
    """
    for name in LEGACY_BOT_FILES:
        path = scope_bot_dir / name
        if path.is_file() and not path.is_symlink():
            path.unlink()
            print(f"    ✓ {bot_name}: removed legacy {name}")
    for name in LEGACY_BOT_DIRS:
        path = scope_bot_dir / name
        if path.is_dir() and not path.is_symlink():
            shutil.rmtree(path)
            print(f"    ✓ {bot_name}: removed legacy {name}/")


def seed_whatsapp_wu_config(scope_root: Path) -> None:
    """Seed `<scope>/workdirs/home/.wu/config.yaml` for the whatsapp bot.

    The bot daemon runs with `HOME` redirected to the scope's
    `workdirs/home/`, so `@ibrahimwithi/wu-cli` resolves `WU_HOME` to
    `<scope>/workdirs/home/.wu/`. wu-cli's `loadConfig()` reads
    `<WU_HOME>/config.yaml` and silently returns an empty schema on
    `ENOENT` — leaving `constraints: undefined`, which makes
    `shouldCollect(jid, config)` return `false` for *every* JID. The
    listener's `messaging-history.set` handler then writes contacts
    (no shouldCollect filter) but drops every chat and message.
    Result: agent sees `chats=0, messages=0` even though Baileys is
    syncing, the bot is connected, and the auth state is healthy.

    Seeding the file with `constraints: { default: full, chats: {} }`
    matches the canonical shared `~/.wu/config.yaml` and unblocks the
    listener's chat/message persistence on first start. Idempotent —
    leaves an existing file alone (operator may have customized it).
    """
    config_path = scope_root / "workdirs" / "home" / ".wu" / "config.yaml"
    if config_path.is_file():
        print(f"    ✓ whatsapp: wu config already at {config_path.relative_to(scope_root)}")
        return
    config_path.parent.mkdir(parents=True, exist_ok=True)
    config_path.write_text(
        "constraints:\n"
        "  default: full\n"
        "  chats: {}\n"
    )
    print(f"    ✓ whatsapp: seeded wu config at {config_path.relative_to(scope_root)}")


def prune_legacy_whatsapp_skill_bin(scope_root: Path) -> None:
    """Remove the legacy per-scope `<scope>/skills/whatsapp/bin/` shim.

    The whatsapp skill wrapper now resolves its `wu` binary through
    the skillshub-side `skillshub/whatsapp/bin/wu` symlink (which in
    turn points at the workspace-hoisted `node_modules/.bin/wu`). The
    per-scope skill bin/ directory only used to exist so the wrapper
    could find a scope-local wu without crossing back into skillshub
    — that's no longer the resolution path.
    """
    skill_bin = scope_root / "skills" / "whatsapp" / "bin"
    if skill_bin.is_dir() and not skill_bin.is_symlink():
        shutil.rmtree(skill_bin)
        print(f"    ✓ whatsapp skill cli: removed legacy {skill_bin.relative_to(scope_root)}/")


def install_into_scope(scope_root: Path, source_bots: Path) -> tuple[int, int]:
    if not source_bots.is_dir():
        print(f"  ✗ source bots root missing: {source_bots}")
        return (0, 0)
    written = 0
    failures = 0
    bots_root = scope_root / "bots"
    bots_root.mkdir(parents=True, exist_ok=True)
    for bot_dir in sorted(source_bots.iterdir()):
        if not bot_dir.is_dir():
            continue
        # `sdk` is a build-time package — bundled INTO each consumer
        # bot's dist/index.js by esbuild. Never launched on its own,
        # so it has no place at the scope.
        if bot_dir.name == "sdk":
            continue
        src_bundle = bot_dir / "dist" / "index.js"
        if not src_bundle.is_file():
            continue
        scope_bot_dir = bots_root / bot_dir.name
        dest = scope_bot_dir / "dist" / "index.js"
        dest.parent.mkdir(parents=True, exist_ok=True)
        # Replace whatever's at dest — could be a stale copy from the
        # pre-symlink era, a broken symlink, or nothing.
        if dest.is_symlink() or dest.exists():
            dest.unlink()
        dest.symlink_to(src_bundle.resolve())
        size = src_bundle.stat().st_size
        print(f"    ✓ {bot_dir.name}/dist/index.js → source ({size // 1024}K)")
        written += 1

        # Converge on the symlink-only layout: nuke any legacy per-scope
        # node_modules / package.json / package-lock.json. Node's default
        # symlink-resolution walks from the dist symlink's target into
        # skillshub's hoisted workspace node_modules, so the legacy
        # per-scope install is now pure waste.
        prune_legacy_runtime_install(bot_dir.name, scope_bot_dir)

        if bot_dir.name == "whatsapp":
            seed_whatsapp_wu_config(scope_root)
            prune_legacy_whatsapp_skill_bin(scope_root)
    return (written, failures)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--scope", default=None)
    parser.add_argument("--all-scopes", action="store_true")
    parser.add_argument("--data-root", default=str(REPO / "magician_data_v3"))
    args = parser.parse_args()

    data_root = Path(args.data_root)
    source_bots = REPO / "skillshub" / "bots"

    scopes = (
        discover_scopes(data_root)
        if args.all_scopes
        else [args.scope or DEFAULT_SCOPE]
    )
    if not scopes:
        print(f"  no scopes found under {data_root}/scopes/")
        return 0

    total = 0
    total_failures = 0
    for scope in scopes:
        scope_root = data_root / "scopes" / scope
        if not scope_root.is_dir():
            print(f"  — {scope}: scope root missing, skipping")
            continue
        print(f"== {scope} ==")
        written, failures = install_into_scope(scope_root, source_bots)
        total += written
        total_failures += failures

    if total_failures > 0:
        print(f"  done: {total} bot bundle(s) installed, {total_failures} runtime-deps failure(s).")
        return 1
    print(f"  done: {total} bot bundle(s) installed.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
