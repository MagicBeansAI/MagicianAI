#!/usr/bin/env python3
"""Per-scope skill values, and the migration off rendered per-scope manifests.

A value that differs per workspace (a self-hosted MCP endpoint) is a
`${NAME}` reference in the skill's one shared `SKILL.md`:

    endpoint: ${METABASE_BASE_URL}/api/metabase-mcp

The runtime resolves it at load from that scope's own
`<scope>/skills/<skill>/config/.env` (Rust `skills::scope_env`), so every
installed file stays a link and workspaces sharing a server share the
manifest but never each other's values. Setup only fills the `.env`.

Setup used to render a real `SKILL.md` into each scope instead, beside a
`.skill-render.json` marker. `retire_rendered_manifest` turns such a copy back
into the link when its bytes still match the digest the marker recorded; the
linkers (`link_runtime.py`, Rust `skills::scope_layer`) run it, and an
explicit `install_skill_layer.py` replaces the whole tree anyway. A real
`SKILL.md` without the marker is the scope's own and is never touched.
"""
from __future__ import annotations

import hashlib
import json
import os
import re
from pathlib import Path
from urllib.parse import urlsplit

LEGACY_RENDER_MARKER = ".skill-render.json"
LEGACY_RENDER_SCHEMA = "magician.skill-render.v1"
MANIFEST = "SKILL.md"


def read_scope_env(scoped_skill: Path) -> dict[str, str]:
    """`KEY=value` lines of the scope's skill `config/.env`, taken literally."""
    path = scoped_skill / "config" / ".env"
    values: dict[str, str] = {}
    if not path.is_file():
        return values
    for line in path.read_text(encoding="utf-8").splitlines():
        match = re.match(r"\s*(?:export\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*=(.*)$", line)
        if not match or line.lstrip().startswith("#"):
            continue
        value = match.group(2).strip()
        if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
            value = value[1:-1]
        values[match.group(1)] = value
    return values


def https_origin(value: str) -> str | None:
    """Normalized `https://host[:port]`, or None (the runtime's same rule)."""
    try:
        parts = urlsplit(value.strip().rstrip("/"))
        port = parts.port
    except ValueError:
        return None
    if (
        parts.scheme != "https"
        or not parts.hostname
        or parts.username
        or parts.password
        or parts.path
        or parts.query
        or parts.fragment
        or not re.fullmatch(r"[A-Za-z0-9.-]+", parts.hostname)
        or len(parts.hostname) > 253
        or parts.hostname.startswith((".", "-"))
        or ".." in parts.hostname
        or port == 0
        or _rewritten_as_ipv4(parts.hostname)
    ):
        return None
    suffix = f":{port}" if port and port != 443 else ""
    return f"https://{parts.hostname.lower()}{suffix}"


def _rewritten_as_ipv4(host: str) -> bool:
    """A host the runtime's URL parser reads as IPv4 and rewrites.

    WHATWG URL parsing treats a host whose last label is numeric (`10.1.2`,
    `1234`, `0x7f.1`) as an IPv4 address and normalizes it, so it would not be
    the origin the operator wrote. Only a canonical dotted quad survives.
    """
    last = host.rstrip(".").rsplit(".", 1)[-1].lower()
    if not (last.isdigit() or re.fullmatch(r"0x[0-9a-f]*", last)):
        return False
    octets = host.split(".")
    canonical = len(octets) == 4 and all(
        octet.isdigit() and str(int(octet)) == octet and int(octet) <= 255 for octet in octets
    )
    return not canonical


def write_atomic(path: Path, text: str, mode: int = 0o644) -> None:
    temporary = path.with_name(f".{path.name}.{os.getpid()}.new")
    if temporary.exists() or temporary.is_symlink():
        temporary.unlink()
    temporary.write_text(text, encoding="utf-8")
    temporary.chmod(mode)
    os.replace(temporary, path)


def is_legacy_rendered(scoped_skill: Path) -> bool:
    """A real SKILL.md the retired renderer wrote (setup-managed, not owned)."""
    marker = scoped_skill / LEGACY_RENDER_MARKER
    return marker.is_file() and not marker.is_symlink()


def retire_rendered_manifest(source: Path, scoped_skill: Path) -> str:
    """Relink a rendered copy to `source/SKILL.md`. Returns a status.

    `none` (nothing rendered here), `relinked`, `marker_removed` (a stale
    marker beside a link or no manifest) or `edited` (the copy changed after
    setup wrote it, so it is left as it is).
    """
    if not is_legacy_rendered(scoped_skill):
        return "none"
    marker = scoped_skill / LEGACY_RENDER_MARKER
    manifest = scoped_skill / MANIFEST
    if manifest.is_symlink() or not manifest.exists():
        marker.unlink()
        return "marker_removed"
    if not manifest.is_file():
        return "edited"
    try:
        recorded = json.loads(marker.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        recorded = {}
    current = hashlib.sha256(manifest.read_bytes()).hexdigest()
    if (
        not isinstance(recorded, dict)
        or recorded.get("schema_version") != LEGACY_RENDER_SCHEMA
        or recorded.get("manifest_sha256") != current
    ):
        return "edited"
    temporary = manifest.with_name(f".{manifest.name}.{os.getpid()}.new")
    if temporary.exists() or temporary.is_symlink():
        temporary.unlink()
    temporary.symlink_to((source / MANIFEST).absolute())
    os.replace(temporary, manifest)
    marker.unlink()
    return "relinked"
