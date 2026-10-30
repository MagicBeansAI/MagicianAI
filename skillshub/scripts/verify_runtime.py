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
import re
import shutil
import subprocess
import sys
import tempfile

import shared_skill_bins
import skill_render

ROOT = Path(__file__).resolve().parents[1]
MEDIA = {"media-fetch", "youtube-search"}
ENGINES = {"cloak-browser", "lightpanda"}
VERIFICATION_REVISION = 3
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
    "higgsfield": "make setup-higgsfield-cli",
    "whatsapp": "make -C skillshub setup-whatsapp-bins",
    "whatsapp-mcp": "make -C skillshub setup-whatsapp-bins",
}
LINK_SCOPES = "make -C skillshub link-scopes"
# These companions must be pack-local: finding them on the host PATH would
# widen a governed child's PATH to an unrelated system tool directory.
PACK_LOCAL_BINS = {
    "media-fetch": {"ffmpeg", "ffprobe"},
    "whatsapp": {"wu", "node"},
    "whatsapp-mcp": {"wu", "node"},
}


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
        self._resolver = None

    @property
    def resolver(self):
        if self._resolver is None:
            self._resolver = shared_skill_bins.Resolver(self.root)
        return self._resolver

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
        stat = path.stat()
        self.files[recorded] = {"path": recorded, "executable": executable,
                                "size_bytes": stat.st_size, "modified_ns": stat.st_mtime_ns}

    def scope(self, scope_root: Path, only: set[str] | None = None, exclude: frozenset[str] = frozenset()):
        """Every `bin/` entry a scope's skills rely on must run from the scope."""
        where = f"{scope_root.parent.name}/{scope_root.name}"
        for manifest in sorted(self.root.glob("*/SKILL.md")):
            skill = manifest.parent
            if (only is not None and skill.name not in only) or skill.name in exclude:
                continue
            scoped = scope_root / "skills" / skill.name
            if shared_skill_bins.scope_removed(scoped):
                continue
            scoped_manifest = scoped / "SKILL.md"
            if (scoped_manifest.exists() and not scoped_manifest.is_symlink()
                    and not skill_render.is_render_managed(scoped)):
                continue  # the scope's own copy of this skill
            if (only is None and skill.name in (ENGINES | {"browser"})
                    and not (bin_names(skill / "bin") | bin_names(scoped / "bin"))):
                continue  # The separate browser/engine step has not provisioned it.
            self.file(scoped_manifest, absolute=True)
            required = PACK_LOCAL_BINS.get(skill.name, set()) | self.resolver.required_local(skill)
            names = (bin_names(skill / "bin") | bin_names(scoped / "bin")
                     | required) - shared_skill_bins.optional_bins(skill)
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
                # A non-executable source is pack data.
                data = name not in required and source.is_file() and not os.access(source, os.X_OK)
                self.file(path, executable=not data, absolute=True)

    def binary(self, skill: Path, name: str):
        local = skill / "bin" / name
        # Match the pack's activation boundary, never a developer's terminal
        # PATH or a shared install that the scope cannot actually execute.
        self.file(local, executable=True)
        # npm launchers may exist while their downloaded native payload is gone.
        if name == "gws":
            self.file(self.root / "node_modules/@googleworkspace/cli/node_modules/.bin_real/gws",
                      executable=True)

    def skills(self, media: bool):
        import yaml
        self.node()
        self.file(self.root / ".venv/bin/python", executable=True)
        self.file(self.root / "package-lock.json")
        count = 0
        for manifest in sorted(self.root.glob("*/SKILL.md")):
            if (manifest.parent.name in MEDIA) != media:
                continue
            if manifest.parent.name in ENGINES | {"browser"}:
                continue
            self.file(manifest)
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
            bins.update(self.resolver.required_local(manifest.parent))
            bins.difference_update(shared_skill_bins.optional_bins(manifest.parent))
            for binary in sorted(bins):
                self.binary(manifest.parent, binary)
        if not count:
            self.errors.append("no governed skill manifests found")

    def node(self):
        command = self.root / ".node/bin/node"
        pin = self.root / ".node-version"
        self.file(command, executable=True)
        self.file(pin)
        if pin.is_file():
            try:
                result = subprocess.run([str(command), "--version"], capture_output=True, text=True, timeout=10)
                expected = "v" + pin.read_text().strip().removeprefix("v")
                if result.returncode or result.stdout.strip() != expected:
                    self.errors.append("Node version differs from .node-version; run make -C skillshub setup-node")
            except (OSError, subprocess.SubprocessError) as error:
                self.errors.append(f"pinned Node cannot execute: {error}")

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
        self.node()
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
        self.file(self.root / "browser/SKILL.md")
        self.file(self.root / "Makefile")
        self.browser_cli(self.root / "browser/bin/agent-browser")

    def browser_cli(self, command: Path):
        """Offline native discovery checks: never open or close a browser."""
        makefile = self.root / "Makefile"
        match = re.search(r"^AGENT_BROWSER_VERSION\s*\?=\s*(\S+)", makefile.read_text(), re.M) if makefile.is_file() else None
        if not match:
            self.errors.append("browser: pinned version missing from Makefile")
            return
        env = dict(os.environ)
        env.pop("AGENT_BROWSER_SKILLS_DIR", None)
        for args, expected in ((["--version"], f"agent-browser {match[1]}"),
                               (["skills", "get", "core", "--full"], None)):
            try:
                result = subprocess.run([str(command), *args], env=env, capture_output=True,
                                        text=True, timeout=15)
                if result.returncode or not result.stdout.strip() or (expected and result.stdout.strip() != expected):
                    self.errors.append(f"{command}: {' '.join(args)} failed local verification")
            except (OSError, subprocess.SubprocessError) as error:
                self.errors.append(f"{command}: {error}")

    def engines(self):
        for skill in ENGINES:
            self.file(self.root / skill / "SKILL.md")
            for binary in shared_skill_bins.declared_bins(self.root / skill):
                self.binary(self.root / skill, binary)
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
    parts = Path(scope).parts
    if len(parts) != 2 or Path(scope).is_absolute() or any(p in (".", "..") for p in parts):
        raise ValueError("scope must be principal/workspace")
    scopes_root = data_root / "scopes"
    chosen = [scopes_root / scope]
    if all_scopes:
        for path in sorted(scopes_root.glob("*/*")):
            if path.is_dir() and path.relative_to(scopes_root).as_posix() != "system/system" and path not in chosen:
                chosen.append(path)
    return chosen


def write_inventory(root: Path, group: str, audit: Audit):
    """A failed recheck must replace any previous success record, atomically."""
    dest = root / ".runtime" / f"{group}.yaml"
    dest.parent.mkdir(parents=True, exist_ok=True)
    payload = {"schema_version": 1, "verification_revision": VERIFICATION_REVISION,
               "files": [] if audit.errors else list(audit.files.values()),
               "errors": audit.errors}
    with tempfile.NamedTemporaryFile(mode="w", dir=dest.parent, prefix=f".{group}-", delete=False) as out:
        temporary = Path(out.name)
        json.dump(payload, out, indent=2)
        out.write("\n")
    try:
        os.replace(temporary, dest)
    finally:
        temporary.unlink(missing_ok=True)


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
        if args.write_inventory:
            # Even interruption or a malformed manifest must fail closed.
            audit.errors.append("verification in progress; rerun if interrupted")
            write_inventory(args.root, group, audit)
            audit.errors.clear()
        try:
            for script in ("verify_runtime.py", "shared_skill_bins.py"):
                audit.file(args.root / "scripts" / script)
            if group in ("skills", "media"):
                audit.skills(group == "media")
            elif group == "bots":
                audit.bots(args.versions)
            elif group == "scopes":
                for scope in scopes:
                    audit.scope(scope)
            else:
                getattr(audit, group)()
            # Include what the runtime executes, not just shared dependencies.
            for scope in scopes:
                if group == "browser":
                    audit.scope(scope, only={"browser"})
                    if not shared_skill_bins.scope_removed(scope / "skills/browser"):
                        audit.browser_cli(scope / "skills/browser/bin/agent-browser")
                elif group == "media":
                    audit.scope(scope, only=MEDIA)
                elif group == "bots":
                    audit.scope(scope, only=set(BOTS) | {"whatsapp-mcp"})
                elif group == "skills":
                    audit.scope(scope, exclude=frozenset(MEDIA | ENGINES | {"browser"} | set(BOTS)))
                elif group == "engines":
                    audit.scope(scope, only=ENGINES)
        except Exception as error:
            audit.errors.append(f"local verification failed ({type(error).__name__}): {error}")
        if audit.errors:
            failed = True
            for error in audit.errors:
                print(f"✗ {group}: {error}", file=sys.stderr)
        else:
            print(f"✓ {group}: {len(audit.files)} runtime files verified")
        if args.write_inventory:
            write_inventory(args.root, group, audit)
    return int(failed)


if __name__ == "__main__":
    sys.exit(main())
