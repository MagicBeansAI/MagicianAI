#!/usr/bin/env python3
"""Convert capability-pack YAMLs into AgentSkills v1 SKILL.md folders.

Reads each pack from
  magician_data_v3/system/capability_templates/packs/<name>.yaml
and writes
  skillshub/<kebab-name>/SKILL.md

For python_facade packs (where implementation.command starts with "python3"
and points at tools/python/<file>.py), the Python source is copied into
  skillshub/<kebab-name>/scripts/<file>.py

Skips:
- runtime_primitive packs (magician-internal state ops, shipped via
  capability packs through migration)
- packs whose target skillshub/<kebab-name>/ already exists (idempotent)

Run with --dry-run to preview without writing.
"""
from __future__ import annotations

import argparse
import re
import shutil
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

# Same list as the auditor; these don't fit the portable-skill model.
RUNTIME_PRIMITIVE_NAMES = {
    "create_agent", "create_dashboard", "create_proposal", "create_task",
    "delegation_files", "evaluate_harness", "inspect_agent",
    "list_agents", "list_episodes", "list_proposals",
    "notify_owner", "read_trace", "reassign_task", "retire_agent",
    "system_status", "task_state", "treasurer",
    "unpublish_dashboard", "update_agent", "update_delegation",
}

DESCRIPTION_MAX_CHARS = 1024


def kebab(name: str) -> str:
    """capability_pack_name → capability-pack-name."""
    return name.lower().replace("_", "-")


def truncate(s: str, limit: int) -> str:
    if len(s) <= limit:
        return s
    return s[: limit - 1].rstrip() + "…"


def extract_bins(spec: dict) -> list[str]:
    """Pull binary names from implementation.command + auth.* commands."""
    bins: list[str] = []
    seen: set[str] = set()

    def add(name: str) -> None:
        if name and name not in seen:
            bins.append(name)
            seen.add(name)

    impl = spec.get("implementation") or {}
    cmd = impl.get("command") or []
    if isinstance(cmd, list) and cmd:
        first = str(cmd[0])
        # python facade: first arg is "python3", second is the script path
        if first in {"python", "python3"}:
            add("python3")
            return bins
        # vendored bin via templated path: take the basename
        last_segment = first.rsplit("/", 1)[-1]
        # strip variable templating like {scope_capabilities_root}/x → x
        last_segment = re.sub(r"^\{[^}]*\}", "", last_segment).strip("/")
        if last_segment:
            add(last_segment)

    # auth.setup_command and similar may reference other bins; skip — too noisy.
    return bins


def extract_python_script(spec: dict) -> str | None:
    """Return the script filename (e.g. 'nanobanana2.py') if this is a python facade."""
    impl = spec.get("implementation") or {}
    cmd = impl.get("command") or []
    if not (isinstance(cmd, list) and len(cmd) >= 2):
        return None
    if str(cmd[0]) not in {"python", "python3"}:
        return None
    script_template = str(cmd[1])
    # Expect "{scope_capabilities_root}/tools/python/<name>.py"
    name = script_template.rsplit("/", 1)[-1]
    return name if name.endswith(".py") else None


def extract_env(spec: dict, python_path: Path | None) -> list[str]:
    """Best-effort scan for required env vars.

    Inner-loop env mappings (`_TOOL_*`) are runtime-injected and don't count.
    What we want is upstream secrets the script reads from os.environ.
    """
    env_set: set[str] = set()

    if python_path and python_path.exists():
        text = python_path.read_text(errors="ignore")
        # os.environ["KEY"] / os.environ.get("KEY") / os.getenv("KEY")
        patterns = [
            r"os\.environ\[\s*['\"]([A-Z][A-Z0-9_]*)['\"]\s*\]",
            r"os\.environ\.get\(\s*['\"]([A-Z][A-Z0-9_]*)['\"]",
            r"os\.getenv\(\s*['\"]([A-Z][A-Z0-9_]*)['\"]",
        ]
        for pat in patterns:
            for m in re.findall(pat, text):
                if m.startswith("_TOOL_"):
                    continue
                if m.startswith("MAGICIAN_"):
                    continue  # runtime-injected
                env_set.add(m)

    return sorted(env_set)


def yaml_dump_inline_lists(data: dict) -> str:
    """Dump frontmatter — keep top-level keys ordered, lists inline."""

    def _dump(obj, indent: int) -> str:
        pad = "  " * indent
        if isinstance(obj, dict):
            out = []
            for k, v in obj.items():
                if isinstance(v, dict):
                    out.append(f"{pad}{k}:")
                    out.append(_dump(v, indent + 1))
                elif isinstance(v, list):
                    inline = "[" + ", ".join(yaml_inline(item) for item in v) + "]"
                    out.append(f"{pad}{k}: {inline}")
                else:
                    out.append(f"{pad}{k}: {yaml_inline(v)}")
            return "\n".join(out)
        return yaml_inline(obj)

    return _dump(data, 0)


def yaml_inline(value) -> str:
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, (int, float)):
        return str(value)
    if value is None:
        return "null"
    s = str(value)
    # Always quote strings to be safe (avoid tag inference, special chars).
    return '"' + s.replace("\\", "\\\\").replace('"', '\\"') + '"'


def render_skill_md(name: str, description: str, requires: dict, install_hint: dict | None,
                    title: str, guide: str) -> str:
    fm: dict = {
        "name": name,
        "description": description,
    }
    if requires or install_hint:
        magician_block: dict = {}
        if requires:
            magician_block["requires"] = requires
        if install_hint:
            magician_block["install_hint"] = install_hint
        fm["metadata"] = {"magician": magician_block}

    body_lines = [f"# {title}", ""]
    if guide:
        body_lines.append(guide.rstrip())
        body_lines.append("")

    return "---\n" + yaml_dump_inline_lists(fm) + "\n---\n\n" + "\n".join(body_lines)


def derive_title(name: str) -> str:
    """jq → Jq, image-generation-via-nanobanana2 → Image Generation Via Nanobanana2."""
    parts = name.replace("-", " ").split()
    return " ".join(p.capitalize() for p in parts)


def derive_install_hint(spec: dict, bins: list[str], env: list[str]) -> dict | None:
    auth = spec.get("auth") or {}
    setup_cmd = auth.get("setup_command")

    parts: dict = {}
    if setup_cmd:
        parts["docs"] = f"OAuth setup: {setup_cmd}"
    elif env:
        parts["docs"] = f"requires env: {', '.join(env)} — set in vault before activating"
    elif bins:
        parts["docs"] = f"requires binary on PATH: {', '.join(bins)}"

    return parts or None


def convert_pack(pack: Path, dry_run: bool) -> tuple[str, str]:
    name = pack.stem

    if name in RUNTIME_PRIMITIVE_NAMES:
        return ("skip-runtime", name)

    # Phase 2 Hand-authored skills shouldn't be overwritten.
    target_name = kebab(name)
    target_dir = SKILLSHUB_DIR / target_name
    if target_dir.exists():
        return ("skip-exists", target_name)

    try:
        spec = yaml.safe_load(pack.read_text()) or {}
    except yaml.YAMLError as e:
        return ("error", f"{name}: YAML parse: {e}")

    description = (spec.get("description") or "").strip()
    if not description:
        return ("error", f"{name}: missing description")
    description = truncate(description, DESCRIPTION_MAX_CHARS)

    guide = (spec.get("guide") or "").strip()

    bins = extract_bins(spec)
    py_script_name = extract_python_script(spec)
    py_script_src = (PYTHON_TOOLS_DIR / py_script_name) if py_script_name else None

    env = extract_env(spec, py_script_src)

    requires: dict = {}
    if bins:
        requires["bins"] = bins
    if env:
        requires["env"] = env

    install_hint = derive_install_hint(spec, bins, env)

    title = derive_title(target_name)
    skill_md = render_skill_md(target_name, description, requires, install_hint, title, guide)

    if dry_run:
        print(f"--- DRY-RUN: {target_dir.relative_to(REPO_ROOT)} ---")
        print(skill_md)
        if py_script_src:
            print(f"  + scripts/{py_script_name}  <- {py_script_src.relative_to(REPO_ROOT)}")
        return ("planned", target_name)

    target_dir.mkdir(parents=True, exist_ok=True)
    (target_dir / "SKILL.md").write_text(skill_md)

    if py_script_src and py_script_src.exists():
        scripts_dir = target_dir / "scripts"
        scripts_dir.mkdir(exist_ok=True)
        dest = scripts_dir / py_script_name
        shutil.copy2(py_script_src, dest)
        # ensure executable bit on the copy so script_runtime can register it
        dest.chmod(0o755)

    return ("written", target_name)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--only", help="Convert only this pack name (e.g. jq).")
    args = parser.parse_args()

    if not PACKS_DIR.is_dir():
        print(f"ERROR: packs dir not found: {PACKS_DIR}", file=sys.stderr)
        return 2

    counts: dict[str, int] = {}
    for pack in sorted(PACKS_DIR.glob("*.yaml")):
        if args.only and pack.stem != args.only:
            continue
        status, msg = convert_pack(pack, args.dry_run)
        counts[status] = counts.get(status, 0) + 1
        print(f"  [{status:14}] {msg}")

    print()
    for status, n in sorted(counts.items()):
        print(f"  {status:14} {n}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
