#!/usr/bin/env python3
"""Check actual skill companions and bot dependencies, without provider calls.

Successful checks can emit a file inventory for the component graph. This is
not a readiness stamp: the graph rechecks every inventoried file on each probe.

With `--data-root`, the checks also cover what the runtime actually executes:
each scope's `skills/<skill>/bin/<name>` must resolve to a usable executable.
The runtime refuses PATH fallbacks for pinned CLIs (agent-browser most of
all), so a shared install that verifies while a scope link dangles is still a
broken install. Scoped files enter the inventory as absolute paths, so the
component probe goes Absent when a copied or moved data root loses them.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]
MEDIA = {"media-fetch", "youtube-search"}
BOTS = ("agentmail", "gmail", "kapso", "telegram", "telegram-self", "whatsapp")
BOT_CLIS = {"gmail": "gws", "telegram-self": "tgcli", "whatsapp": "wu"}
# The make target that (re)creates each skill's generated `bin/` entries.
# Anything not listed comes from `setup-skill-bins` or is tracked in git.
BIN_PRODUCERS = {
    "browser": "make setup-agent-browser",
    "document-to-markdown": "make -C skillshub setup-document-to-markdown",
    "officecli-runtime": "make setup-officecli",
    "office-excel": "make setup-officecli",
    "office-word": "make setup-officecli",
    "office-powerpoint": "make setup-officecli",
    "pdftotext": "make -C skillshub setup-pdftotext",
    "ocr": "make -C skillshub setup-ocr",
    "media-fetch": "make -C skillshub setup-media-fetch",
    "marimo": "make -C skillshub setup-marimo",
    "metabase": "make setup-metabase-cli",
    "higgsfield": "make setup-higgsfield-cli",
    "whatsapp": "make -C skillshub install-bot-bundles",
}
LINK_SCOPES = "make -C skillshub link-scopes"


def bin_producer(skill: str) -> str:
    return BIN_PRODUCERS.get(skill, "make -C skillshub setup-skill-bins")


def bin_names(bin_dir: Path) -> set[str]:
    if not bin_dir.is_dir():
        return set()
    return {entry.name for entry in bin_dir.iterdir()
            if not entry.name.startswith((".", "__")) and not (entry.is_dir() and not entry.is_symlink())}


class Audit:
    def __init__(self, root: Path):
        self.root = root
        self.files: dict[str, dict] = {}
        self.errors: list[str] = []

    def file(self, path: Path, *, executable: bool = False, absolute: bool = False):
        if not path.is_file() or path.stat().st_size == 0:
            self.errors.append(f"missing/empty file: {path}")
            return
        if executable and not os.access(path, os.X_OK):
            self.errors.append(f"not executable: {path}")
            return
        if absolute:
            recorded = os.path.abspath(path)
        else:
            recorded = os.path.relpath(path.absolute(), self.root / ".runtime")
        self.files[recorded] = {"path": recorded, "executable": executable}

    def scope(self, scope_root: Path, only: set[str] | None = None, exclude: frozenset[str] = frozenset()):
        """Every `bin/` entry a scope's skills rely on must run from the scope."""
        where = f"{scope_root.parent.name}/{scope_root.name}"
        for manifest in sorted(self.root.glob("*/SKILL.md")):
            skill = manifest.parent
            if (only is not None and skill.name not in only) or skill.name in exclude:
                continue
            scoped = scope_root / "skills" / skill.name
            scoped_manifest = scoped / "SKILL.md"
            if scoped_manifest.exists() and not scoped_manifest.is_symlink():
                continue  # the scope's own copy of this skill
            names = bin_names(skill / "bin") | bin_names(scoped / "bin")
            if not names:
                continue
            if not scoped.is_dir():
                self.errors.append(f"{where}: skill {skill.name} is not installed in this scope; run `{LINK_SCOPES}`")
                continue
            for name in sorted(names):
                path = scoped / "bin" / name
                source = skill / "bin" / name
                build = f"run `{bin_producer(skill.name)}`, then `{LINK_SCOPES}`"
                if path.is_symlink() and not path.exists():
                    target = os.readlink(path)
                    if source.exists():
                        self.errors.append(f"{where}: {skill.name}/bin/{name} points at {target}, which is gone; run `{LINK_SCOPES}`")
                    else:
                        self.errors.append(f"{where}: {skill.name}/bin/{name} points at {target}, which is gone, and skillshub has no {source}; {build}")
                    continue
                if not path.exists():
                    if source.exists():
                        self.errors.append(f"{where}: {skill.name}/bin/{name} is not linked into this scope; run `{LINK_SCOPES}`")
                    else:
                        self.errors.append(f"{where}: {skill.name}/bin/{name} is missing here and in skillshub; {build}")
                    continue
                # A non-executable source is pack data (metabase's generated.json).
                data = source.is_file() and not os.access(source, os.X_OK)
                self.file(path, executable=not data, absolute=True)

    def binary(self, skill: Path, name: str):
        if skill.name == "media-fetch" and name in ("ffmpeg", "ffprobe"):
            # Host PATH availability does not prove the governed pack's wiring.
            self.file(skill / "bin" / name, executable=True)
            return
        # Match the dispatcher's scoped runtime, without falling through to an
        # unrelated host npm/Python installation for those managed dependencies.
        dirs = [skill / "bin", self.root / ".venv/bin", self.root / ".node/bin",
                self.root / "node_modules/.bin"]
        if skill.name == "marimo":
            dirs.insert(1, skill / ".venv/bin")
        path = shutil.which(name, path=os.pathsep.join(map(str, dirs)))
        if not path:
            path = shutil.which(name)
        if not path:
            self.errors.append(f"{skill.name}: missing declared binary {name}")
            return
        self.file(Path(path), executable=True)
        # npm launchers may exist while their downloaded native payload is gone.
        if name == "gws":
            self.file(self.root / "node_modules/@googleworkspace/cli/node_modules/.bin_real/gws",
                      executable=True)

    def skills(self, media: bool):
        import yaml
        self.file(self.root / ".node/bin/node", executable=True)
        self.file(self.root / ".venv/bin/python", executable=True)
        count = 0
        for manifest in sorted(self.root.glob("*/SKILL.md")):
            if (manifest.parent.name in MEDIA) != media:
                continue
            data = yaml.safe_load(manifest.read_text().split("---", 2)[1]) or {}
            metadata = (data.get("metadata") or {}).get("magician") or {}
            contract = metadata.get("runtime_contract") or {}
            if not contract:
                continue
            count += 1
            self.file(manifest)
            requires = contract.get("requires") or {}
            bins = set(requires.get("bins") or [])
            bins.update((metadata.get("requires") or {}).get("bins") or [])
            if requires.get("entrypoint"):
                bins.add(requires["entrypoint"])
            for action in ((metadata.get("runtime_actions") or {}).get("actions") or {}).values():
                bins.update((action.get("requires") or {}).get("bins") or [])
                if action.get("executable"):
                    bins.add(action["executable"])
            for binary in sorted(bins):
                self.binary(manifest.parent, binary)
        if not count:
            self.errors.append("no governed skill manifests found")

    def npm(self):
        lock_path = self.root / "package-lock.json"
        self.file(lock_path)
        lock = json.loads(lock_path.read_text())["packages"]
        # Check every required installed package against the lock. Optional
        # platform packages legitimately need not exist on this host.
        for relative, package in lock.items():
            if "node_modules/" not in relative or package.get("link"):
                continue
            manifest = self.root / relative / "package.json"
            if package.get("optional") and not manifest.is_file():
                continue
            self.file(manifest)
            if manifest.is_file():
                actual = json.loads(manifest.read_text())
                if actual.get("version") != package.get("version"):
                    self.errors.append(f"{relative}: version differs from package-lock.json")
        self.file(self.root / "node_modules/@higgsfield/cli/vendor/hf", executable=True)
        self.file(self.root / "node_modules/@googleworkspace/cli/node_modules/.bin_real/gws",
                  executable=True)
        for name in BOT_CLIS.values():
            self.file(self.root / "node_modules/.bin" / name, executable=True)
        # Native addons are runtime dependencies, even when the JS entry exists.
        for addon in (self.root / "node_modules").glob("**/build/Release/*.node"):
            self.file(addon)

    def bots(self, versions: bool):
        self.file(self.root / ".node/bin/node", executable=True)
        self.npm()
        for bot in BOTS:
            self.file(self.root / "bots" / bot / "dist/index.js")
        if versions:
            env = {**os.environ, "PATH": str(self.root / ".node/bin") + os.pathsep + os.environ.get("PATH", "")}
            with tempfile.TemporaryDirectory(prefix="magician-bot-check-") as directory:
                # Some CLIs create a state directory even for --version.
                env.update(WU_HOME=directory + "/wu", TGCLI_STORE=directory + "/tgcli", GWS_CONFIG_DIR=directory + "/gws")
                for name in BOT_CLIS.values():
                    command = self.root / "node_modules/.bin" / name
                    try:
                        proc = subprocess.run([str(command), "--version"], env=env,
                                              capture_output=True, text=True, timeout=15)
                        if proc.returncode or not proc.stdout.strip():
                            self.errors.append(f"{name}: local --version check failed (exit {proc.returncode})")
                    except (OSError, subprocess.TimeoutExpired) as error:
                        self.errors.append(f"{name}: {error}")
            # Loading a CLI alone does not force better-sqlite3 to load its ABI.
            proc = subprocess.run([str(self.root / ".node/bin/node"), "-e",
                                   "const DB=require('better-sqlite3'); const db=new DB(':memory:'); db.close()"],
                                  cwd=self.root, env=env, capture_output=True, text=True, timeout=15)
            if proc.returncode:
                self.errors.append("bot SQLite native addon cannot load with the pinned Node ABI")

    def browser(self):
        self.file(self.root / "browser/bin/agent-browser", executable=True)
        self.file(self.root / "browser/node_modules/agent-browser/skill-data/core/SKILL.md")

    def engines(self):
        self.file(self.root / "cloak-browser/scripts/resolve.py", executable=True)
        self.file(self.root / "cloak-browser/scripts/stealth-init.js")
        self.file(self.root / "lightpanda/scripts/resolve.py", executable=True)
        lightpanda = shutil.which("lightpanda")
        if lightpanda:
            self.file(Path(lightpanda), executable=True)
        else:
            self.errors.append("Lightpanda native executable missing")
        try:
            info = json.loads(subprocess.check_output(
                [str(self.root / ".venv/bin/python"), "-c",
                 "import json; from cloakbrowser import binary_info; print(json.dumps(binary_info()))"],
                text=True, timeout=10))
            if info.get("installed") and info.get("binary_path"):
                self.file(Path(info["binary_path"]), executable=True)
            else:
                self.errors.append("CloakBrowser SDK is installed but its native Chromium is missing")
        except (OSError, subprocess.SubprocessError, ValueError):
            self.errors.append("CloakBrowser local binary inventory failed")


def selected_scopes(data_root: Path, scope: str, all_scopes: bool) -> list[Path]:
    scopes_root = data_root / "scopes"
    chosen = [scopes_root / scope]
    if all_scopes:
        for path in sorted(scopes_root.glob("*/*")):
            if path.is_dir() and path.relative_to(scopes_root).as_posix() != "system/system" and path not in chosen:
                chosen.append(path)
    return chosen


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--group", choices=("skills", "media", "bots", "npm", "browser", "engines", "scopes", "all"), default="all")
    parser.add_argument("--write-inventory", action="store_true")
    parser.add_argument("--versions", action="store_true", help="also run offline bot CLI and native ABI checks")
    parser.add_argument("--data-root", type=Path, default=None,
                        help="runtime root; adds the scoped bin/ checks the runtime depends on")
    parser.add_argument("--scope", default="anonymous/default")
    parser.add_argument("--all-scopes", action="store_true", help="check every tenant scope, not just --scope")
    args = parser.parse_args()
    if args.group == "scopes" and args.data_root is None:
        parser.error("--group scopes needs --data-root")
    scopes = selected_scopes(args.data_root, args.scope, args.all_scopes) if args.data_root else []
    groups = ("skills", "media", "bots", "browser", "engines") if args.group == "all" else (args.group,)
    if args.group == "all" and scopes:
        groups += ("scopes",)
    failed = False
    for group in groups:
        audit = Audit(args.root.resolve())
        if group in ("skills", "media"):
            audit.skills(group == "media")
        elif group == "bots":
            audit.bots(args.versions)
        elif group == "scopes":
            for scope in scopes:
                audit.scope(scope)
        else:
            getattr(audit, group)()
        # Each component's inventory also covers its own skills' scoped bins,
        # so the component graph reports what the runtime will execute.
        for scope in scopes:
            if group == "browser":
                audit.scope(scope, only={"browser"})
            elif group == "media":
                audit.scope(scope, only=MEDIA)
            elif group == "bots":
                audit.scope(scope, only=set(BOTS))
            elif group == "skills":
                audit.scope(scope, exclude=frozenset(MEDIA | {"browser"} | set(BOTS)))
        if audit.errors:
            failed = True
            for error in audit.errors:
                print(f"✗ {group}: {error}", file=sys.stderr)
        else:
            print(f"✓ {group}: {len(audit.files)} runtime files verified")
            if args.write_inventory:
                dest = args.root / ".runtime" / f"{group}.yaml"
                dest.parent.mkdir(parents=True, exist_ok=True)
                temporary = dest.with_suffix(".tmp")
                temporary.write_text(json.dumps({"schema_version": 1, "files": list(audit.files.values())}, indent=2) + "\n")
                os.replace(temporary, dest)
    return int(failed)


if __name__ == "__main__":
    sys.exit(main())
