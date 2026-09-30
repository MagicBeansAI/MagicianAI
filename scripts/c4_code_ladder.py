"""Code-ladder derivation for the c4 index.

Turns the raw code graph (crates/modules/files/symbols) into the tiered
code ladder rendered below the curated architecture layers: tiers ->
crates -> modules, with stats and doc-harvested descriptions.

Tier assignment mixes a name/path rule table (non-Rust components and
known roles) with a Kahn topological depth over depends_on edges for the
remaining Rust crates (depth 0 = foundation, deeper = mid).
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

TIER_ORDER = [
    "foundation",
    "mid",
    "orchestrator",
    "satellites",
    "bins",
    "apps",
    "bots",
    "skills",
    "standalone",
]

# Crates with a fixed architectural role, by exact crate name.
ROLE_BY_NAME = {
    "magician": "orchestrator",
    "magician-api": "satellites",
    "magician-comms": "satellites",
    "magician-bin": "bins",
}

# Rule-based tiers for components that never participate in cargo
# depends_on edges (non-Rust or satellite islands).
RULE_PATTERNS: list[tuple[str, str]] = [
    ("unified-ui", "apps"),
    ("Magios", "apps"),
    ("magician-desktop", "apps"),
    ("magdroid", "apps"),
    ("magesp", "apps"),
    ("magicutor-extension", "apps"),
    ("repo-root", "standalone"),
    ("magic-supervisor", "standalone"),
    ("document-to-markdown-cli", "standalone"),
    ("research-planner", "standalone"),
    ("lvglgdb", "standalone"),
]

# docs/components/<key>/README.md lookup key per crate name when it
# differs from the crate name itself.
DOC_KEY_BY_NAME = {
    "Magios": "magios",
    "magician-desktop": "desktop",
    "unified-ui": "unified-ui",
    "magic-supervisor": "magic-supervisor",
    "magicutor": "magicutor",
    "magicutor-extension": "magicutor",
    "bots": "bots",
    "skillshub": "scripts",
}

# Tech marker inference from crate name/path.
def infer_tech(name: str, path: str) -> str:
    text = f"{name} {path}"
    if "src-tauri" in path or name == "magician-desktop":
        return "tauri-rust"
    if name == "unified-ui":
        return "svelte"
    if name == "Magios" or path.endswith(".xcodeproj") or "/" + "magios" in text:
        return "swift"
    if name == "magesp":
        return "esp-idf"
    if name == "repo-root":
        return "workspace"
    if name.startswith("skill-") or name == "skillshub":
        return "skills"
    if name.startswith("bot-") or name == "bots":
        return "node"
    if name == "research-planner":
        return "reference-app"
    return "rust"


def _rule_tier(name: str) -> str | None:
    for pattern, tier in RULE_PATTERNS:
        if name == pattern:
            return tier
    if name.startswith("bot-") or name == "bots":
        return "bots"
    if name.startswith("skill-") or name == "skillshub":
        return "skills"
    if name == "magdroid" or name == "magesp":
        return "apps"
    return None


def _kahn_depths(nodes: list[dict[str, Any]], edges: list[dict[str, Any]]) -> dict[str, int]:
    """Longest-path depth from roots over depends_on edges (crate ids)."""
    crate_ids = {n["id"] for n in nodes if n.get("kind") == "crate"}
    deps: dict[str, set[str]] = {cid: set() for cid in crate_ids}
    for edge in edges:
        if edge.get("kind") != "depends_on":
            continue
        src, dst = edge.get("from", ""), edge.get("to", "")
        if src in crate_ids and dst in crate_ids:
            deps[src].add(dst)

    depths: dict[str, int] = {}
    remaining = dict(deps)

    def visit(cid: str, seen: frozenset[str]) -> int:
        if cid in depths:
            return depths[cid]
        if cid in seen:
            return 0  # cycle guard: treat as leaf
        parents = remaining.get(cid, ())
        if not parents:
            depths[cid] = 0
            return 0
        depth = 1 + max(visit(parent, seen | {cid}) for parent in sorted(parents))
        depths[cid] = depth
        return depth

    for cid in sorted(crate_ids):
        visit(cid, frozenset())
    return depths


def _first_paragraph(text: str) -> str | None:
    lines = text.splitlines()
    paragraph: list[str] = []
    for line in lines:
        stripped = line.strip()
        if not stripped:
            if paragraph:
                break
            continue
        if stripped.startswith("#"):
            if paragraph:
                break
            continue  # skip leading headings
        paragraph.append(stripped)
    result = " ".join(paragraph).strip()
    return result or None


def harvest_description(crate_name: str, crate_path: str, repo_root: Path) -> str | None:
    """docs/components README first paragraph, else manifest description."""
    doc_key = DOC_KEY_BY_NAME.get(crate_name, crate_name)
    doc = repo_root / "docs" / "components" / doc_key / "README.md"
    if doc.is_file():
        para = _first_paragraph(doc.read_text(encoding="utf-8"))
        if para:
            return para

    crate_dir = repo_root / crate_path if crate_path else None
    if crate_dir and crate_dir.is_dir():
        cargo = crate_dir / "Cargo.toml"
        if cargo.is_file():
            for line in cargo.read_text(encoding="utf-8").splitlines():
                stripped = line.strip()
                if stripped.startswith('description = "'):
                    return stripped[len('description = "') : -1] or None
                if stripped.startswith('description = "') is False and stripped.startswith("description"):
                    # multi-line or single-quote forms: fall through
                    continue
        package = crate_dir / "package.json"
        if package.is_file():
            try:
                import json

                data = json.loads(package.read_text(encoding="utf-8"))
                desc = str(data.get("description") or "").strip()
                if desc:
                    return desc
            except Exception:
                pass
    return None


def build_code_ladder(graph: dict[str, Any], repo_root: Path) -> dict[str, Any]:
    nodes = graph.get("nodes", [])
    edges = graph.get("edges", [])

    crates = [n for n in nodes if n.get("kind") == "crate"]
    depths = _kahn_depths(crates, edges)

    # Aggregate stats by crate attr on non-crate nodes.
    stats: dict[str, dict[str, int]] = {}
    symbols_by_module: dict[str, dict[str, int]] = {}

    def bump(crate: str, key: str) -> None:
        entry = stats.setdefault(crate, {"symbols": 0, "files": 0, "modules": 0, "endpoints": 0})
        entry[key] += 1

    for node in nodes:
        kind = node.get("kind")
        if kind == "crate":
            continue
        crate = str(node.get("crate") or "")
        if kind == "module":
            # Prefer id prefix (module::<crate>::...) over the crate attr.
            parts = str(node.get("id", "")).split("::")
            if len(parts) >= 2 and parts[0] == "module":
                crate = parts[1]
        if not crate:
            continue
        if kind == "function" or kind in {
            "struct",
            "enum",
            "trait",
            "const",
            "static",
            "type_alias",
            "section",
        }:
            bump(crate, "symbols")
            module = str(node.get("module") or "")
            if module:
                counter = symbols_by_module.setdefault(module, {})
                counter[kind] = counter.get(kind, 0) + 1
        elif kind == "file":
            bump(crate, "files")
        elif kind == "endpoint":
            bump(crate, "endpoints")

    depends: dict[str, list[str]] = {}
    for edge in edges:
        if edge.get("kind") != "depends_on":
            continue
        depends.setdefault(edge.get("from", ""), []).append(edge.get("to", ""))

    module_index: dict[str, list[dict[str, Any]]] = {}
    for node in nodes:
        if node.get("kind") != "module":
            continue
        node_id = str(node.get("id", ""))
        parts = node_id.split("::")
        if len(parts) < 2:
            continue
        crate = parts[1]
        label = "::".join(parts[2:]) or crate
        entry = {
            "id": node_id,
            "label": label,
            "symbols": sum(symbols_by_module.get(node_id.replace("module::", "", 1), {}).values()),
            "kind_counts": symbols_by_module.get(node_id.replace("module::", "", 1), {}),
        }
        module_index.setdefault(crate, []).append(entry)

    # Prune empty leaf modules (no symbols, no child modules) from the
    # render grid — they are layout noise and bloat the artifact.
    for crate, entries in module_index.items():
        kept_ids = {e["id"] for e in entries}
        module_index[crate] = [
            e
            for e in entries
            if e["symbols"] > 0
            or any(other != e["id"] and other.startswith(e["id"] + "::") for other in kept_ids)
        ]

    tier_members: dict[str, list[dict[str, Any]]] = {tier: [] for tier in TIER_ORDER}
    for crate in crates:
        name = str(crate.get("label") or crate.get("id", "").split("::")[-1])
        crate_id = str(crate.get("id"))
        path = str(crate.get("path") or "")

        tier = ROLE_BY_NAME.get(name) or _rule_tier(name)
        if tier is None:
            tier = "foundation" if depths.get(crate_id, 0) == 0 else "mid"

        modules = sorted(module_index.get(name, []), key=lambda m: m["id"])
        entry_stats = stats.get(name, {"symbols": 0, "files": 0, "modules": 0, "endpoints": 0})
        tier_members[tier].append(
            {
                "id": crate_id,
                "label": name,
                "tech": infer_tech(name, path),
                "description": harvest_description(name, path, repo_root),
                "deps": sorted(depends.get(crate_id, [])),
                "symbols": entry_stats["symbols"],
                "files": entry_stats["files"],
                "modules": entry_stats["modules"] if entry_stats["modules"] else len(modules),
                "endpoints": entry_stats["endpoints"],
                "modules_list": modules,
            }
        )

    tiers = [
        {"id": tier, "label": tier.replace("-", " ").title(), "crates": sorted(members, key=lambda c: c["id"])}
        for tier, members in tier_members.items()
        if members
    ]
    return {"tiers": tiers}
