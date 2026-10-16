#!/usr/bin/env python3
"""Render a scope's own `SKILL.md` for skills whose manifest varies per scope.

Most skills are a file-level symlink mirror of `skillshub/<skill>/`. A skill
whose manifest must carry a per-scope value (a self-hosted MCP endpoint, say)
ships a *valid* template `SKILL.md` that uses a reserved placeholder origin,
plus `render.yaml` naming each placeholder and the scope input that replaces
it. Setup writes a real `SKILL.md` into each scope from that scope's own
`skills/<skill>/config/.env`, so workspaces sharing a server share the code but
never each other's endpoints, keys or logins.

    # skillshub/<skill>/render.yaml
    schema_version: magician.skill-render.v1
    substitutions:
      - placeholder: https://metabase.invalid     # RFC 2606 reserved
        input: METABASE_BASE_URL
        kind: https_origin

Rules:

- Inputs come only from the scope's skill `config/.env`, never from the
  server-wide runtime env: another workspace's values must not leak in.
- Only non-secret values belong here; rendered manifests are plain files.
- `.skill-render.json` beside the rendered `SKILL.md` records the template and
  input hashes, so a changed template or input re-renders and an unchanged one
  is left alone. That marker is also how the linkers (`link_runtime.py`,
  `install_skill_layer.py`, Rust `skills::scope_layer`) tell a setup-managed
  copy from a manifest the scope owns outright.
- Missing or invalid inputs leave the scope with no manifest for the skill, so
  a placeholder endpoint never reaches a catalog. A previously rendered copy is
  withdrawn the same way.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import sys
from dataclasses import dataclass, field
from pathlib import Path
from urllib.parse import urlsplit

import yaml

RENDER_SPEC = "render.yaml"
MARKER = ".skill-render.json"
SCHEMA = "magician.skill-render.v1"
MANIFEST = "SKILL.md"
KINDS = {"https_origin"}


def is_rendered_skill(source: Path) -> bool:
    return (source / RENDER_SPEC).is_file()


def is_render_managed(scoped_skill: Path) -> bool:
    """A real SKILL.md written by this renderer (not a scope-owned manifest)."""
    return (scoped_skill / MARKER).is_file()


@dataclass
class Spec:
    substitutions: list[dict[str, str]] = field(default_factory=list)


def load_spec(source: Path) -> Spec:
    data = yaml.safe_load((source / RENDER_SPEC).read_text(encoding="utf-8"))
    if not isinstance(data, dict) or data.get("schema_version") != SCHEMA:
        raise ValueError(f"{source.name}/{RENDER_SPEC}: schema_version must be {SCHEMA}")
    subs = data.get("substitutions")
    if not isinstance(subs, list) or not subs:
        raise ValueError(f"{source.name}/{RENDER_SPEC}: substitutions must be a non-empty list")
    seen = set()
    for sub in subs:
        if not isinstance(sub, dict) or set(sub) != {"placeholder", "input", "kind"}:
            raise ValueError(f"{source.name}/{RENDER_SPEC}: each substitution needs placeholder, input, kind")
        if sub["kind"] not in KINDS:
            raise ValueError(f"{source.name}/{RENDER_SPEC}: unsupported kind {sub['kind']!r}")
        if not re.fullmatch(r"[A-Z][A-Z0-9_]*", sub["input"]):
            raise ValueError(f"{source.name}/{RENDER_SPEC}: input must be an env-style name")
        if not sub["placeholder"].endswith(".invalid") or sub["placeholder"] in seen:
            raise ValueError(f"{source.name}/{RENDER_SPEC}: placeholder must be a unique *.invalid origin")
        seen.add(sub["placeholder"])
    return Spec(subs)


def read_scope_env(scoped_skill: Path) -> dict[str, str]:
    path = scoped_skill / "config" / ".env"
    values: dict[str, str] = {}
    if not path.is_file():
        return values
    for line in path.read_text(encoding="utf-8").splitlines():
        match = re.match(r"\s*([A-Za-z_][A-Za-z0-9_]*)\s*=(.*)$", line)
        if not match or line.lstrip().startswith("#"):
            continue
        value = match.group(2).strip()
        if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
            value = value[1:-1]
        values[match.group(1)] = value
    return values


def validate(kind: str, value: str) -> str | None:
    """Normalized value, or None when it is not acceptable for `kind`."""
    if kind == "https_origin":
        parts = urlsplit(value.strip().rstrip("/"))
        if (
            parts.scheme != "https"
            or not parts.hostname
            or parts.username
            or parts.password
            or parts.path
            or parts.query
            or parts.fragment
            or not re.fullmatch(r"[A-Za-z0-9.-]+", parts.hostname)
        ):
            return None
        port = f":{parts.port}" if parts.port else ""
        return f"https://{parts.hostname.lower()}{port}"
    return None


def _sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _write_atomic(path: Path, text: str, mode: int = 0o644) -> None:
    temporary = path.with_name(f".{path.name}.{os.getpid()}.new")
    if temporary.exists() or temporary.is_symlink():
        temporary.unlink()
    temporary.write_text(text, encoding="utf-8")
    temporary.chmod(mode)
    os.replace(temporary, path)


def _withdraw(scoped_skill: Path) -> bool:
    """Remove a previously rendered manifest (only one this renderer wrote)."""
    if not is_render_managed(scoped_skill):
        manifest = scoped_skill / MANIFEST
        # A template symlink left by an older linker is not a usable manifest.
        if manifest.is_symlink():
            manifest.unlink()
            return True
        return False
    for name in (MANIFEST, MARKER):
        target = scoped_skill / name
        if target.exists() or target.is_symlink():
            target.unlink()
    return True


def render_scope_skill(source: Path, scoped_skill: Path) -> tuple[str, str]:
    """Render one skill into one scope. Returns (status, detail).

    status is one of: rendered, unchanged, missing_inputs, scope_owned.
    """
    manifest = scoped_skill / MANIFEST
    if manifest.exists() and not manifest.is_symlink() and not is_render_managed(scoped_skill):
        return "scope_owned", "the scope owns its SKILL.md; left alone"
    spec = load_spec(source)
    template = (source / MANIFEST).read_text(encoding="utf-8")
    env = read_scope_env(scoped_skill)
    resolved: dict[str, str] = {}
    problems = []
    for sub in spec.substitutions:
        raw = env.get(sub["input"], "")
        value = validate(sub["kind"], raw) if raw else None
        if value is None:
            problems.append(f"{sub['input']} ({'not set' if not raw else 'not a valid ' + sub['kind']})")
        else:
            resolved[sub["placeholder"]] = value
    if problems:
        withdrew = _withdraw(scoped_skill)
        return "missing_inputs", "needs " + ", ".join(problems) + (" (rendered copy withdrawn)" if withdrew else "")
    rendered = template
    for placeholder, value in resolved.items():
        rendered = rendered.replace(placeholder, value)
    leftover = [s["placeholder"] for s in spec.substitutions if s["placeholder"] in rendered]
    if leftover:
        raise ValueError(f"{source.name}: placeholder survived rendering: {leftover}")
    provenance = {
        "schema_version": SCHEMA,
        "skill": source.name,
        "template_sha256": _sha256(template.encode() + (source / RENDER_SPEC).read_bytes()),
        "inputs_sha256": _sha256(json.dumps(resolved, sort_keys=True).encode()),
        "manifest_sha256": _sha256(rendered.encode()),
    }
    marker = scoped_skill / MARKER
    if (
        manifest.is_file()
        and not manifest.is_symlink()
        and marker.is_file()
        and json.loads(marker.read_text(encoding="utf-8")) == provenance
        and _sha256(manifest.read_bytes()) == provenance["manifest_sha256"]
    ):
        return "unchanged", "up to date"
    scoped_skill.mkdir(parents=True, exist_ok=True)
    if manifest.is_symlink():
        manifest.unlink()
    _write_atomic(manifest, rendered)
    _write_atomic(marker, json.dumps(provenance, indent=2, sort_keys=True) + "\n")
    return "rendered", "written from " + ", ".join(sorted(s["input"] for s in spec.substitutions))


def render_scope(skillshub: Path, scope: Path, names: set[str] | None = None, out=sys.stdout) -> int:
    """Render every render-declared skill installed in one scope. Returns failures."""
    failures = 0
    for source in sorted(p.parent for p in skillshub.glob(f"*/{RENDER_SPEC}")):
        if names is not None and source.name not in names:
            continue
        scoped_skill = scope / "skills" / source.name
        if not scoped_skill.is_dir():
            continue
        try:
            status, detail = render_scope_skill(source, scoped_skill)
        except (OSError, ValueError, yaml.YAMLError) as error:
            failures += 1
            print(f"  ✗ {source.name}: {error}", file=out)
            continue
        mark = {"rendered": "✓", "unchanged": "·", "missing_inputs": "—", "scope_owned": "·"}[status]
        if status != "unchanged":
            print(f"  {mark} {source.name} [{scope.parent.name}/{scope.name}]: {detail}", file=out)
    return failures


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--data-root", required=True, type=Path)
    parser.add_argument("--scope", default="anonymous/default")
    parser.add_argument("--all-scopes", action="store_true")
    parser.add_argument("--names", default="")
    args = parser.parse_args()
    skillshub = Path(__file__).resolve().parents[1]
    names = {n.strip() for n in args.names.split(",") if n.strip()} or None
    scopes_root = args.data_root / "scopes"
    scopes = [scopes_root / args.scope]
    if args.all_scopes:
        scopes += [p for p in sorted(scopes_root.glob("*/*")) if p.is_dir() and p not in scopes
                   and p.relative_to(scopes_root).as_posix() != "system/system"]
    failures = sum(render_scope(skillshub, scope, names) for scope in scopes if scope.is_dir())
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
