#!/usr/bin/env python3
"""Retire the generated Metabase package, preserving each workspace's state.

Run explicitly with --apply. Origins stay within their original scope; no
API key is copied into the OAuth skill and no OAuth consent is automated.
"""
from __future__ import annotations

import argparse
import os
from pathlib import Path
import shutil
import time

import yaml

import install_skill_layer
import skill_render
import uninstall_skill_layer

ROOT = Path(__file__).resolve().parents[1]
PACKAGE_FILES = {
    "SKILL.md", "tool_schema.yaml", "render.yaml", ".skill-render.json",
    "scripts", "bin", ".pp-build", "generator-pin.json", "README.md",
}


def is_legacy(path: Path) -> bool:
    manifest = path / "SKILL.md"
    try:
        source = manifest.read_text()
        if source.startswith("---"):
            metadata = yaml.safe_load(source.split("---", 2)[1]) or {}
            contract = metadata.get("metadata", {}).get("magician", {}).get("runtime_contract", {})
            runtime = contract.get("runtime", {})
            if runtime.get("protocol") and runtime["protocol"] != "cli":
                return False
            if runtime.get("protocol") == "cli" and "metabase-pp-cli" in contract.get("requires", {}).get("bins", []):
                return True
    except OSError:
        pass
    return (path / "bin/metabase-pp-cli").exists() or (
        manifest.is_symlink() and manifest.resolve().parent.name == "metabase"
    )


def migrate_scope(scope: Path, source: Path = ROOT) -> str:
    skills = scope / "skills"
    if skills.is_symlink() or (skills / ".skill-state").is_symlink():
        raise ValueError("workspace skills and state roots must be real directories")
    with install_skill_layer.skill_install_lock(skills / "metabase"):
        return _migrate_scope(scope, source)


def _migrate_scope(scope: Path, source: Path) -> str:
    skills = scope / "skills"
    legacy, current = skills / "metabase", skills / "metabase-mcp"
    if not legacy.exists() and not legacy.is_symlink():
        return "no legacy package"
    if not is_legacy(legacy):
        raise ValueError(f"{scope.name}: unrecognized metabase package; left intact")
    # Installed config links normally point into this skills root's stable
    # state. Reject links into another workspace before reading or writing env.
    guarded = [current, current / "config/.env", skills / ".skill-state/metabase/removed"]
    if not legacy.is_symlink():
        guarded.append(legacy / "config/.env")
    if current.is_symlink() or any(
        not path.resolve().is_relative_to(skills.resolve()) for path in guarded
    ):
        raise ValueError(f"{scope.name}: package or state leaves its workspace; left intact")
    values = skill_render.read_scope_env(current)
    if not values.get("METABASE_BASE_URL"):
        old = "" if legacy.is_symlink() else skill_render.read_scope_env(legacy).get("METABASE_BASE_URL", "")
        if old:
            origin = skill_render.validate("https_origin", old)
            if origin is None:
                raise ValueError(f"{scope.name}: invalid legacy origin; left intact")
            config = current / "config"
            config.mkdir(parents=True, exist_ok=True)
            env = config / ".env"
            previous = env.read_text() if env.is_file() else ""
            skill_render._write_atomic(env, previous.rstrip() + f"\nMETABASE_BASE_URL={origin}\n", 0o600)
    install_skill_layer.install_skill(source / "metabase-mcp", current)
    # Rename first: the catalog can no longer execute the old package. Keep
    # config, auth and any unknown user files; remove only known package code.
    state = skills / ".skill-state/metabase"
    state.mkdir(parents=True, exist_ok=True)
    if legacy.is_symlink():
        legacy.unlink()
    else:
        retired = state / f"retired-{time.time_ns()}"
        for entry in legacy.iterdir():
            if entry.name not in PACKAGE_FILES and entry.is_symlink():
                target = entry.resolve()
                entry.unlink()
                entry.symlink_to(target, target_is_directory=target.is_dir())
        os.replace(legacy, retired)
        for name in PACKAGE_FILES:
            path = retired / name
            if path.is_symlink() or path.is_file():
                path.unlink()
            elif path.is_dir():
                shutil.rmtree(path)
        if not any(retired.iterdir()):
            retired.rmdir()
    uninstall_skill_layer.mark_removed(skills, "metabase")
    return "MCP installed; legacy code removed; private state retained"


def remove_legacy_binaries(data_root: Path) -> None:
    store = data_root / "system/skill-bin"
    if store.is_symlink() or store.parent.is_symlink():
        raise ValueError("legacy binary store must be a real managed directory")
    for binary in store.glob("*/metabase-pp-cli"):
        if not binary.parent.resolve().is_relative_to(store.resolve()):
            raise ValueError("legacy binary store entry leaves its managed root")
        binary.unlink()
        try:
            binary.parent.rmdir()
        except OSError:
            pass


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--data-root", required=True, type=Path)
    parser.add_argument("--apply", action="store_true")
    args = parser.parse_args()
    if not args.apply:
        parser.error("--apply is required to retire installed package code")
    for scope in sorted((args.data_root / "scopes").glob("*/*")):
        if scope.is_symlink() or scope.parent.is_symlink() or not scope.is_dir():
            continue
        if scope.relative_to(args.data_root / "scopes").as_posix() == "system/system":
            continue
        print(f"{scope.parent.name}/{scope.name}: {migrate_scope(scope)}")
    remove_legacy_binaries(args.data_root)


if __name__ == "__main__":
    main()
