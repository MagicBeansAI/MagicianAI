"""Magician-specific extension: discover skills under `skillshub/` and
expose them via `cgraph_skills`, `/api/skills`, and a CLI subcommand.

Replaces the legacy `cmd_capabilities` discovery (which looked for a
`capabilities` Rust crate that no longer exists in this workspace).

### Skill kinds

| skill_type    | Detection                                              |
|---------------|--------------------------------------------------------|
| `tool`        | `SKILL.md` frontmatter carries `metadata.magician.runtime_contract` |
| `personality` | `skillshub/<name>/SKILL.md` whose frontmatter carries `metadata.magician.personality:` |
| `procedure`   | `skillshub/<name>/SKILL.md` with neither of the above   |
| `agent`       | `skillshub/bots/<name>/package.json`                    |

Each emits a `skill` node with attrs `{kind: "skill", skill_type, label,
path, version?, description?}` plus a `defines` edge from the skill to
its manifest file node so the graph stays internally consistent.
"""

from __future__ import annotations

import json
import re
from pathlib import Path
from typing import Any, Callable, Iterator
from urllib.parse import parse_qs

from codegraph_ext import CodegraphExtension


_FRONTMATTER_RE = re.compile(r"^---\s*\n(.*?)\n---", re.DOTALL)
_PERSONALITY_RE = re.compile(r"(?m)^ {4}personality\s*:")
_NAME_RE = re.compile(r"(?m)^name\s*:\s*\"?([^\"\n]+)\"?")
_VERSION_RE = re.compile(r"(?m)^version\s*:\s*\"?([^\"\n]+)\"?")
_DESCRIPTION_RE = re.compile(r"(?m)^description\s*:\s*\"?([^\"\n]+)\"?")
_RUNTIME_CONTRACT_RE = re.compile(r"(?m)^ {4}runtime_contract\s*:")


def _parse_skill_md(path: Path) -> dict[str, str]:
    """Pull a tiny set of name/version/description fields out of the
    YAML frontmatter without taking a YAML dependency. Quick-and-dirty
    but good enough — magician's SKILL.md format is hand-written and
    consistent."""
    try:
        text = path.read_text(encoding="utf-8")
    except OSError:
        return {}
    match = _FRONTMATTER_RE.search(text)
    body = match.group(1) if match else text
    out: dict[str, str] = {}
    nm = _NAME_RE.search(body)
    if nm:
        out["name"] = nm.group(1).strip()
    vm = _VERSION_RE.search(body)
    if vm:
        out["version"] = vm.group(1).strip()
    dm = _DESCRIPTION_RE.search(body)
    if dm:
        out["description"] = dm.group(1).strip()
    out["_has_personality"] = bool(_PERSONALITY_RE.search(body))
    out["_has_runtime_contract"] = bool(_RUNTIME_CONTRACT_RE.search(body))
    out["_raw_body"] = body  # used by callers that want richer parsing
    return out


def _classify(parsed: dict[str, str]) -> str:
    if parsed.get("_has_runtime_contract"):
        return "tool"
    if parsed.get("_has_personality"):
        return "personality"
    return "procedure"


class MagicianSkillsExtension(CodegraphExtension):
    name = "magician_skills"

    # ── Hook 1: extra graph nodes/edges ─────────────────────────────

    def discover_nodes(self, root: Path, graph_data: dict[str, Any]) -> Iterator[dict]:
        skillshub = root / "skillshub"
        if not skillshub.is_dir():
            return
        for skill_md in sorted(skillshub.glob("*/SKILL.md")):
            pack_dir = skill_md.parent
            parsed = _parse_skill_md(skill_md)
            skill_type = _classify(parsed)
            name = parsed.get("name") or pack_dir.name
            node = {
                "id": f"skill::{pack_dir.name}",
                "kind": "skill",
                "label": name,
                "skill_type": skill_type,
                "path": str(skill_md.relative_to(root)),
            }
            if parsed.get("version"):
                node["version"] = parsed["version"]
            if parsed.get("description"):
                node["description"] = parsed["description"][:200]
            yield node

        # Bot adapters → agent skills.
        bots_dir = skillshub / "bots"
        if bots_dir.is_dir():
            for bot in sorted(bots_dir.iterdir()):
                pkg = bot / "package.json"
                if not pkg.is_file():
                    continue
                name = bot.name
                version = ""
                description = ""
                try:
                    data = json.loads(pkg.read_text(encoding="utf-8"))
                    name = data.get("name") or bot.name
                    if name.startswith("@") and "/" in name:
                        name = name.split("/", 1)[1]
                    version = data.get("version") or ""
                    description = data.get("description") or ""
                except (OSError, ValueError):
                    pass
                node = {
                    "id": f"skill::bots::{bot.name}",
                    "kind": "skill",
                    "label": name,
                    "skill_type": "agent",
                    "path": str(pkg.relative_to(root)),
                }
                if version:
                    node["version"] = version
                if description:
                    node["description"] = description[:200]
                yield node

    def discover_edges(self, root: Path, graph_data: dict[str, Any]) -> Iterator[dict]:
        # Skill node → its manifest file node. The file node already
        # exists thanks to the core walker, so we just point at it.
        skillshub = root / "skillshub"
        if not skillshub.is_dir():
            return
        for skill_md in sorted(skillshub.glob("*/SKILL.md")):
            rel = str(skill_md.relative_to(root))
            yield {
                "from": f"skill::{skill_md.parent.name}",
                "to": f"file::{rel}",
                "kind": "defines",
            }
        bots_dir = skillshub / "bots"
        if bots_dir.is_dir():
            for bot in sorted(bots_dir.iterdir()):
                pkg = bot / "package.json"
                if not pkg.is_file():
                    continue
                yield {
                    "from": f"skill::bots::{bot.name}",
                    "to": f"file::{str(pkg.relative_to(root))}",
                    "kind": "defines",
                }

    # ── Hook 2: MCP tool ────────────────────────────────────────────

    def mcp_tools(self) -> dict[str, dict[str, Any]]:
        return {
            "cgraph_skills": {
                "fn": self._cgraph_skills,
                "description": (
                    "List magician skills discovered under `skillshub/`. "
                    "skill_type is one of `tool` / `personality` / `procedure` / `agent`. "
                    "Pass `type=tool` (etc.) to filter, or `type=\"\"` (default) for all."
                ),
            }
        }

    def _cgraph_skills(self, type: str = "", limit: int = 200) -> str:
        nodes = self._load_skill_nodes()
        if type:
            nodes = [n for n in nodes if n.get("skill_type") == type]
        nodes.sort(key=lambda n: (n.get("skill_type", ""), n.get("label", "")))
        capped = nodes[:limit]
        result = {
            "total": len(nodes),
            "returned": len(capped),
            "type_filter": type or None,
            "counts": self._counts_by_type(self._load_skill_nodes()),
            "skills": [
                {k: v for k, v in n.items() if k != "kind"}
                for n in capped
            ],
        }
        if len(nodes) > len(capped):
            result["truncated"] = True
        return json.dumps(result, indent=2, default=str)

    # ── Hook 3: HTTP route ──────────────────────────────────────────

    def http_routes(self) -> dict[str, Callable]:
        return {"/api/skills": self._http_skills}

    def _http_skills(self, parsed_url) -> dict:
        params = parse_qs(getattr(parsed_url, "query", "") or "")
        skill_type = (params.get("type", [""])[0]).strip()
        try:
            limit = int(params.get("limit", ["200"])[0])
        except ValueError:
            limit = 200
        nodes = self._load_skill_nodes()
        counts = self._counts_by_type(nodes)
        if skill_type:
            nodes = [n for n in nodes if n.get("skill_type") == skill_type]
        nodes.sort(key=lambda n: (n.get("skill_type", ""), n.get("label", "")))
        capped = nodes[:limit]
        return {
            "total": len(nodes),
            "returned": len(capped),
            "type_filter": skill_type or None,
            "counts": counts,
            "skills": [
                {k: v for k, v in n.items() if k != "kind"}
                for n in capped
            ],
            "truncated": len(nodes) > len(capped),
        }

    # ── Hook 4: CLI subcommand ──────────────────────────────────────

    def cli_commands(self) -> dict[str, Callable]:
        return {"skills": self._cli_skills}

    # ── Hook 5: slash-command spec for the 2D / 3D viewers ──────────

    def slash_commands(self):
        return [{
            "cmd": "/skills",
            "arg": "[type]",
            "description": "List magician skills — tools / agents / personalities / procedures",
            # `kinds` here are the node-kinds that should be offered
            # as autocomplete entries when the user types `/skills …`.
            # Skills are a custom kind injected by this extension.
            "kinds": ["skill"],
            "http_route": "/api/skills",
            "arg_param": "type",
            "summary": {
                "header_fields": [
                    {"label": "Total",  "from": "total"},
                    {"label": "Counts", "from": "counts", "format": "kv"},
                ],
                "list_field": "skills",
                "row_template": "{skill_type}  {label}  v{version}  @ {path}",
            },
        }]

    def _cli_skills(self, argv: list[str]) -> int:
        import argparse
        parser = argparse.ArgumentParser(prog="codegraph_ext_cli skills",
                                         description="List magician skills.")
        parser.add_argument("--type", default="",
                            choices=("", "tool", "personality", "procedure", "agent"))
        parser.add_argument("--limit", type=int, default=200)
        args = parser.parse_args(argv)
        out = self._cgraph_skills(type=args.type, limit=args.limit)
        print(out)
        return 0

    # ── Helpers ─────────────────────────────────────────────────────

    def _graph_path(self) -> Path:
        return Path(__file__).resolve().parents[2] / "docs" / "codegraph" / "graph.json"

    def _load_skill_nodes(self) -> list[dict]:
        try:
            graph = json.loads(self._graph_path().read_text(encoding="utf-8"))
        except (OSError, ValueError):
            return []
        return [n for n in graph.get("nodes", []) if n.get("kind") == "skill"]

    def _counts_by_type(self, nodes: list[dict]) -> dict[str, int]:
        counts: dict[str, int] = {}
        for n in nodes:
            t = n.get("skill_type", "?")
            counts[t] = counts.get(t, 0) + 1
        return counts
