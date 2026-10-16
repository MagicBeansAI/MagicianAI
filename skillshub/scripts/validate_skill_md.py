#!/usr/bin/env python3
"""Validate every SKILL.md against the magician spec (AgentSkills v1 + extensions).

Usage:
  validate_skill_md.py <skill-dir> [<skill-dir> ...]

Each <skill-dir> contains a SKILL.md. Exits non-zero on any validation
failure; prints a summary to stderr.
"""
from __future__ import annotations

import re
import sys
from pathlib import Path

try:
    import yaml
except ImportError:
    print("ERROR: PyYAML required (pip install pyyaml or apt install python3-yaml)", file=sys.stderr)
    sys.exit(2)

NAME_RE = re.compile(r"^[a-z0-9](?:[a-z0-9-]{0,62}[a-z0-9])?$")
DESC_MAX = 1024
COMPAT_MAX = 500
# SemVer 2.0 — MAJOR.MINOR.PATCH with optional `-prerelease` and `+build`.
# Skill version is OPTIONAL today (back-compat with pre-versioning SKILL.md
# files); when present it must be a string parseable as semver-ish.
VERSION_RE = re.compile(r"^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z\.-]+)?$")
CONTENT_SOURCE_SCHEMA_VERSION = 1
CONTENT_READER_SCHEMA_VERSION = 1
OBSERVE_SOURCE_SCHEMA_VERSION = 1
LEGACY_SKILL_SIDECARS = {
    "tool_schema.yaml": "runtime_contract and runtime_actions",
    "content_source.yaml": "content_source",
    "content_reader.yaml": "content_reader",
    "observe_source.yaml": "observe_source",
}


def validate_product_extension(
    skill_dir: Path,
    magician: dict,
    *,
    key: str,
    schema_version: int,
    required_mappings: tuple[str, ...],
    has_governed_action_catalog: bool,
    capability_backed: bool,
) -> list[str]:
    extension = magician.get(key)
    if extension is None:
        return []
    location = f"{skill_dir / 'SKILL.md'}: metadata.magician.{key}"
    errors: list[str] = []
    if not isinstance(extension, dict):
        return [f"{location} must be a mapping"]
    if extension.get("schema_version") != schema_version:
        errors.append(f"{location}.schema_version must be {schema_version}")
    for field in required_mappings:
        if not isinstance(extension.get(field), dict):
            errors.append(f"{location}.{field} must be a mapping")
    capability = extension.get("capability")
    if isinstance(capability, dict) and capability.get("name") != skill_dir.name:
        errors.append(
            f"{location}.capability.name must match owning skill '{skill_dir.name}'"
        )
    if capability_backed and not has_governed_action_catalog:
        errors.append(
            f"{location} requires runtime_contract and runtime_actions in the same SKILL.md"
        )
    return errors


def validate(skill_dir: Path) -> list[str]:
    errors: list[str] = []
    skill_md = skill_dir / "SKILL.md"
    if not skill_md.exists():
        return [f"{skill_dir}: SKILL.md missing"]

    raw = skill_md.read_text(encoding="utf-8")
    if not raw.startswith("---\n"):
        return [f"{skill_md}: missing YAML frontmatter (must start with '---\\n')"]
    end = raw.find("\n---\n", 4)
    if end < 0:
        return [f"{skill_md}: unterminated frontmatter (no closing '---')"]

    try:
        fm = yaml.safe_load(raw[4:end]) or {}
    except yaml.YAMLError as e:
        return [f"{skill_md}: frontmatter YAML parse error: {e}"]

    if not isinstance(fm, dict):
        return [f"{skill_md}: frontmatter is not a mapping"]

    name = fm.get("name", "")
    if not name:
        errors.append(f"{skill_md}: 'name' is required")
    elif not isinstance(name, str):
        errors.append(f"{skill_md}: 'name' must be a string")
    elif not NAME_RE.match(name):
        errors.append(
            f"{skill_md}: name '{name}' violates AgentSkills naming "
            f"(lowercase + hyphens, 1-64 chars, no leading/trailing/consecutive hyphens)"
        )
    elif "--" in name:
        errors.append(f"{skill_md}: name '{name}' contains consecutive hyphens")
    elif name != skill_dir.name:
        errors.append(
            f"{skill_md}: name '{name}' must match parent directory '{skill_dir.name}'"
        )

    desc = fm.get("description", "")
    if not desc:
        errors.append(f"{skill_md}: 'description' is required")
    elif not isinstance(desc, str):
        errors.append(f"{skill_md}: 'description' must be a string")
    elif len(desc) > DESC_MAX:
        errors.append(f"{skill_md}: 'description' is {len(desc)} chars (max {DESC_MAX})")

    compat = fm.get("compatibility")
    if compat is not None:
        if not isinstance(compat, str):
            errors.append(f"{skill_md}: 'compatibility' must be a string")
        elif len(compat) > COMPAT_MAX:
            errors.append(f"{skill_md}: 'compatibility' is {len(compat)} chars (max {COMPAT_MAX})")

    version = fm.get("version")
    if version is not None:
        if not isinstance(version, str):
            errors.append(f"{skill_md}: 'version' must be a string (semver: MAJOR.MINOR.PATCH)")
        elif not VERSION_RE.match(version):
            errors.append(
                f"{skill_md}: version '{version}' is not semver-shaped "
                f"(MAJOR.MINOR.PATCH, optional -prerelease/+build)"
            )

    metadata = fm.get("metadata") or {}
    if not isinstance(metadata, dict):
        errors.append(f"{skill_md}: 'metadata' must be a mapping")
        return errors

    magician = metadata.get("magician") or {}
    if magician and not isinstance(magician, dict):
        errors.append(f"{skill_md}: 'metadata.magician' must be a mapping")
        return errors

    if "agent" in magician:
        errors.append(
            f"{skill_md}: metadata.magician.agent is not a recognised block — "
            f"agent definitions live as YAML under "
            f"magician_data_v3/system/agent_templates/agents/<name>/"
        )

    has_personality = "personality" in magician and magician["personality"] is not None
    if has_personality:
        p = magician["personality"]
        if not isinstance(p, dict):
            errors.append(f"{skill_md}: metadata.magician.personality must be a mapping")
        else:
            if not p.get("active_mode"):
                errors.append(f"{skill_md}: metadata.magician.personality.active_mode is required")

    runtime_actions = magician.get("runtime_actions")
    has_governed_action_catalog = isinstance(magician.get("runtime_contract"), dict) and (
        isinstance(runtime_actions, dict) and bool(runtime_actions)
    )
    for sidecar_name, replacement in LEGACY_SKILL_SIDECARS.items():
        sidecar = skill_dir / sidecar_name
        if sidecar.exists() or sidecar.is_symlink():
            errors.append(
                f"{sidecar}: legacy skill sidecars are unsupported; move the declaration "
                f"into SKILL.md metadata.magician.{replacement}"
            )
    for key, version, required_mappings, capability_backed in (
        (
            "content_source",
            CONTENT_SOURCE_SCHEMA_VERSION,
            ("adapter", "capability", "output"),
            True,
        ),
        (
            "content_reader",
            CONTENT_READER_SCHEMA_VERSION,
            ("reader", "capability", "output"),
            True,
        ),
        (
            "observe_source",
            OBSERVE_SOURCE_SCHEMA_VERSION,
            ("source",),
            False,
        ),
    ):
        errors.extend(
            validate_product_extension(
                skill_dir,
                magician,
                key=key,
                schema_version=version,
                required_mappings=required_mappings,
                has_governed_action_catalog=has_governed_action_catalog,
                capability_backed=capability_backed,
            )
        )
    observe = magician.get("observe_source")
    if isinstance(observe, dict) and not isinstance(observe.get("profiles"), list):
        errors.append(
            f"{skill_md}: metadata.magician.observe_source.profiles must be a list"
        )

    return errors


def main() -> int:
    if len(sys.argv) < 2:
        print("usage: validate_skill_md.py <skill-dir> [<skill-dir> ...]", file=sys.stderr)
        return 2

    all_errors: list[str] = []
    n_validated = 0
    for arg in sys.argv[1:]:
        d = Path(arg)
        if not d.is_dir():
            continue
        n_validated += 1
        all_errors.extend(validate(d))

    if all_errors:
        print("\n".join(all_errors), file=sys.stderr)
        print(
            f"\nFAIL: {len(all_errors)} validation error(s) in {n_validated} skill(s)",
            file=sys.stderr,
        )
        return 1

    print(f"OK: {n_validated} skill(s) validated", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
