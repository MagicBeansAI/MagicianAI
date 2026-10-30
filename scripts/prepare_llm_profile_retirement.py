#!/usr/bin/env python3
"""Read-only inventory for retiring profile aliases after a catalog migration.

This prepares a migration; it never edits configuration or runtime records.
Reports include selector IDs and field paths, never record text or credentials.
"""
from __future__ import annotations

import argparse
import collections
import hashlib
import json
import re
import os
from pathlib import Path

import yaml

from magician_config_text import read_config_text

MAX_RECORD_BYTES = 4 * 1024 * 1024
RUNTIME_RECORD_NAMES = {"definition.agent.yaml", "llm_routing_overrides.json"}
SOURCE_EXTENSIONS = {".rs", ".ts", ".svelte", ".swift", ".kt", ".py", ".sh", ".yaml", ".yml"}
CLIENT_MIGRATION_ARTIFACTS = {
    "ui/unified-ui/src/lib/llm/legacySelections.v1.json",
    "magios/Magios/LegacyLlmSelections.generated.swift",
    "magdroid/android/bridge/src/main/kotlin/ai/magicbeans/magdroid/chat/LegacyLlmSelections.generated.kt",
}
IGNORED_DIRECTORIES = {".git", ".local", ".worktrees", "node_modules", "target", "build", "dist", "coverage", "__pycache__", ".gradle", ".build", ".venv"}


class InventoryLoader(yaml.SafeLoader):
    """Treat Rust enum tags as inert structure, never Python objects."""


def inert_enum_tag(loader, _tag, node):
    if isinstance(node, yaml.MappingNode):
        return loader.construct_mapping(node, deep=True)
    if isinstance(node, yaml.SequenceNode):
        return loader.construct_sequence(node, deep=True)
    return loader.construct_scalar(node)


InventoryLoader.add_multi_constructor("!", inert_enum_tag)


def occurrences(value, known: set[str], path: tuple[str, ...] = ()) -> list[dict]:
    found = []
    if isinstance(value, dict):
        for key, item in value.items():
            # Definitions of the old-ID map are not uses of that map.
            if path == ("llm", "router", "catalog") and key == "aliases":
                continue
            if str(key) in known:
                found.append({"field": "/".join((*path, str(key))), "id": str(key), "kind": "key"})
            found.extend(occurrences(item, known, (*path, str(key))))
    elif isinstance(value, list):
        for index, item in enumerate(value):
            found.extend(occurrences(item, known, (*path, str(index))))
    elif isinstance(value, str) and value in known:
        found.append({"field": "/".join(path), "id": value, "kind": "value"})
    return found


def canonical_target(name: str, aliases: dict[str, str]) -> str:
    seen = set()
    while name in aliases:
        if name in seen:
            raise ValueError(f"cyclic alias {name}")
        seen.add(name)
        name = aliases[name]
    return name


def selection_migration(catalog: dict, coding: list[dict]) -> dict:
    """Offline conversion data only; operation owners must retain assignments.

    Model preferences retain provider/transport through the catalog model ID.
    They must not erase the workload constraints of an agent/operation pin.
    Native CLI preferences are owned by the engine catalog, not this YAML.
    """
    aliases = catalog.get("aliases", {})
    assignments = catalog.get("assignments", {})
    models = catalog.get("models", {})

    def selection(selector: str) -> dict:
        target = canonical_target(selector, aliases)
        assignment = assignments.get(target)
        if assignment:
            model_id = assignment["model"]
            thinking = assignment.get("thinking") or "auto"
        else:
            # Model IDs can contain slashes; match catalog IDs, never split at
            # the first slash or infer a provider/model from an old name.
            matches = [(name, level) for name, model in models.items()
                       for level in ["auto", *model.get("thinking", {}).get("levels", [])]
                       if target == f"model/{name}/{level}"]
            if len(matches) != 1:
                raise ValueError(f"selector has no unambiguous model selection: {selector}")
            model_id, thinking = matches[0]
        model = models.get(model_id)
        if model is None:
            raise ValueError(f"selector references an unknown model: {selector}")
        levels = ["auto", *model.get("thinking", {}).get("levels", [])]
        return {"canonical_target": target, "assignment": target if assignment else None,
                "model_selection": {"model_id": model_id, "thinking": thinking},
                "client_resolvable": model.get("selectable", True) and not model.get("embedding", False) and thinking in levels}

    routes = {name: selection(name) for name in sorted(aliases)}
    coding_choices = {}
    for row in coding:
        converted = selection(row["llm_profile"])
        model = models[converted["model_selection"]["model_id"]]
        coding_levels = ["auto", *(model.get("coding_thinking") or model.get("thinking", {})).get("levels", [])]
        coding_choices[row["id"]] = {
            **converted, "engine": "pi",
            "client_resolvable": converted["client_resolvable"] and model.get("coding", False)
                and converted["model_selection"]["thinking"] in coding_levels and row.get("enabled", True),
        }
    return {"schema_version": 1, "purpose": "one_time_offline_preference_conversion",
            "runtime_alias_resolver": False, "routes": routes, "coding": coding_choices,
            "owner_rules": ["Agent/operation pins retain their assignment and workload restrictions.",
                            "Preferences convert only when client_resolvable is true; otherwise require an explicit choice.",
                            "Native CLI selectors need the engine-catalog migration owner.",
                            "Do not install this map in the steady-state router."]}


def inventory_config(config: Path) -> tuple[dict, set[str]]:
    raw = yaml.safe_load(read_config_text(config, expand_catalog=False))
    router = raw["llm"]["router"]
    catalog = router.get("catalog", {})
    if not catalog.get("models"):
        raise ValueError("migrate the generation config to a catalog before preparing alias retirement")
    aliases = catalog.get("aliases", {})
    assignments = catalog.get("assignments", {})
    choices = {f"model/{key}/{level}" for key, model in catalog.get("models", {}).items()
               for level in ["auto", *model.get("thinking", {}).get("levels", [])]}
    resolved = {old: canonical_target(old, aliases) for old in aliases}
    invalid = sorted({target for target in resolved.values() if target not in assignments and target not in choices})
    if invalid:
        raise ValueError(f"unresolved alias targets: {invalid}")
    coding = raw.get("coding", {}).get("profiles", [])
    known = set(aliases) | {item["id"] for item in coding}
    fallback_edges = []
    for name, assignment in assignments.items():
        legacy = assignment.get("metadata", {}).get("fallback_profile")
        target = assignment.get("fallback", legacy)
        if target:
            canonical = canonical_target(target, aliases)
            if canonical not in assignments and canonical not in choices:
                raise ValueError(f"unknown fallback {target}")
            fallback_edges.append({"assignment": name, "target": target, "legacy_target": legacy, "canonical_target": canonical})
    references = occurrences(raw, known)
    sources = [config]
    if config.with_name("llm-router.yaml").is_file():
        sources.append(config.with_name("llm-router.yaml"))
    return {
        "config": str(config.resolve()),
        "source_sha256": {str(path.resolve()): hashlib.sha256(path.read_bytes()).hexdigest() for path in sources},
        "authored_generation_profiles": len(router.get("profiles", {})),
        "authored_adaptive_profiles": len(router.get("adaptive_profiles", {})),
        "models": len(catalog.get("models", {})),
        "assignments": len(assignments),
        "alias_map": resolved,
        "legacy_coding_selectors": len(coding),
        "selection_migration": selection_migration(catalog, coding),
        "fallback_edges": fallback_edges,
        "active_config_references": references,
        "active_config_references_by_section": dict(collections.Counter(item["field"].split("/")[0] for item in references)),
    }, known


def inventory_runtime(root: Path, known: set[str]) -> dict:
    import os
    records, errors = [], []
    scanned = 0
    # Owned definitions and override stores only. Conversation text, secrets,
    # journal/receipt history and model files are not searched or rewritten.
    for base in (root / "system", root / "scopes"):
        if not base.is_dir():
            continue
        for directory, dirs, files in os.walk(base, followlinks=False):
            dirs[:] = sorted(d for d in dirs if d not in IGNORED_DIRECTORIES and d not in {"versions", "archive", "history", "secrets"})
            for name in sorted(set(files) & RUNTIME_RECORD_NAMES):
                path = Path(directory) / name
                try:
                    if path.is_symlink() or not path.resolve().is_relative_to(root.resolve()):
                        raise ValueError("record is outside the runtime root")
                    if path.stat().st_size > MAX_RECORD_BYTES:
                        raise ValueError("record exceeds bounded inventory limit")
                    content = path.read_bytes()
                    value = yaml.load(content, Loader=InventoryLoader)
                    hits = occurrences(value, known)
                    scanned += 1
                    if hits:
                        relative = path.relative_to(root)
                        owner = record_owner(relative)
                        record = {"path": str(relative), "owner": owner,
                                  "sha256": hashlib.sha256(content).hexdigest(), "references": hits}
                        if owner == "execution_snapshot_requires_lifecycle_audit":
                            record["lifecycle"] = execution_lifecycle(root, relative.parent)
                        records.append(record)
                except (OSError, ValueError, yaml.YAMLError) as error:
                    errors.append({"path": str(path.relative_to(root)), "error": type(error).__name__})
    return {"root": str(root.resolve()), "records_scanned": scanned, "records_with_legacy_references": records,
            "records_with_references_by_owner": dict(collections.Counter(record["owner"] for record in records)), "errors": errors,
            "not_covered": ["browser localStorage", "iOS UserDefaults", "Android preferences", "in-memory chat queues", "active/paused sealed execution state", "scope storage backends not represented by owned definition/override files"]}


def record_owner(relative: Path) -> str:
    """Storage ownership, not an assertion that a recorded execution is active."""
    parts = relative.parts
    if len(parts) >= 7 and parts[0] == "scopes":
        if parts[3:5] == ("coding_engine", "runs"):
            return "coding_source_snapshot"
        if parts[3] == "tasks" and parts[5] == "executions":
            return "execution_snapshot_requires_lifecycle_audit"
        if parts[3:5] == ("agent_runtime", "agents") and parts[-1] == "definition.agent.yaml":
            return "agent_definition_store"
    if parts[:3] == ("system", "agent_templates", "agents"):
        return "packaged_agent_template"
    if parts == ("system", "llm_routing_overrides.json"):
        return "install_routing_store"
    return "unclassified_owned_file_requires_review"


def execution_lifecycle(root: Path, relative: Path) -> dict:
    """Read only bounded lifecycle fields; never include task/event bodies."""
    try:
        values, hashes = {}, {}
        for name in ("state.json", "execution.json"):
            path = root / relative / name
            cursor = root
            for part in (*relative.parts, name):
                cursor = cursor / part
                if cursor.is_symlink():
                    raise ValueError("symlinked lifecycle input")
            if not path.resolve().is_relative_to(root.resolve()) or path.stat().st_size > MAX_RECORD_BYTES:
                raise ValueError("lifecycle input is outside the bounded inventory")
            data = path.read_bytes()
            values[name] = json.loads(data)
            hashes[name] = hashlib.sha256(data).hexdigest()
        state = values["state.json"].get("status")
        execution = values["execution.json"].get("execution", {}).get("waiting_state")
        known = {"completed", "cancelled", "failed", "running", "paused", "pending", "waiting"}
        # Unknown/free-form values are not echoed into the report.
        normalize = lambda value: value.lower() if isinstance(value, str) and value.lower() in known else "unknown"
        state, execution = normalize(state), normalize(execution)
        return {"state_status": state, "execution_waiting_state": execution,
                "terminal_consistent": state == execution and state in {"completed", "cancelled", "failed"},
                "source_sha256": hashes}
    except (OSError, ValueError, TypeError, AttributeError) as error:
        return {"terminal_consistent": False, "requires_owner_review": True, "error": type(error).__name__}


def inventory_sources(repo: Path, known: set[str]) -> list[dict]:
    import os
    if not known:
        return []
    pattern = re.compile(r'''["'](''' + "|".join(re.escape(item) for item in sorted(known, key=len, reverse=True)) + r''')["']''')
    found = []
    for directory, dirs, files in os.walk(repo, followlinks=False):
        dirs[:] = sorted(d for d in dirs if d not in IGNORED_DIRECTORIES and d not in {"data", "docs", "magician_data_v3"})
        for name in sorted(files):
            path = Path(directory) / name
            relative = path.relative_to(repo)
            migration_only = relative.as_posix() in CLIENT_MIGRATION_ARTIFACTS
            if (path.suffix not in SOURCE_EXTENSIONS and not migration_only) or path.is_symlink() or path.stat().st_size > MAX_RECORD_BYTES:
                continue
            try:
                for number, line in enumerate(path.read_text().splitlines(), 1):
                    ids = sorted(set(pattern.findall(line)))
                    if ids:
                        found.append({"path": str(relative), "line": number, "ids": ids,
                                      "review": "one_time_client_preference_migration" if migration_only else
                                      "test_or_fixture" if "test" in str(relative).lower() or "fixture" in str(relative).lower() else "source_owner_review"})
            except (OSError, UnicodeError):
                continue
    return found


def backup_inventory(report: dict, destination: Path) -> dict:
    """Snapshot only inventoried inputs; refuse stale sources before writing."""
    sources = [(Path(path), Path("config") / Path(path).name, digest)
               for path, digest in report["source_sha256"].items()]
    for key in ("runtime", "packaged_templates"):
        owner = report.get(key)
        if not owner:
            continue
        if owner["errors"]:
            raise ValueError("resolve inventory errors before taking the migration snapshot")
        root = Path(owner["root"])
        for record in owner["records_with_legacy_references"]:
            relative = Path(record["path"])
            if relative.is_absolute() or ".." in relative.parts:
                raise ValueError("invalid inventoried record path")
            sources.append((root / relative, Path(key) / relative, record["sha256"]))
    content = []
    for source, relative, expected in sources:
        data = source.read_bytes()
        if hashlib.sha256(data).hexdigest() != expected:
            raise ValueError("an input changed after inventory; refresh the inventory before backup")
        content.append((source, relative, expected, data))
    destination.mkdir(mode=0o700, parents=True, exist_ok=False)
    manifest = {"scope": "inventoried_config_and_owned_definition_files_only", "files": [],
                "not_included": (report.get("runtime") or {}).get("not_covered", [])}
    for source, relative, expected, data in content:
        output = destination / relative
        output.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        with os.fdopen(os.open(output, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "wb") as handle:
            handle.write(data)
        manifest["files"].append({"source": str(source.resolve()), "backup": str(relative), "sha256": expected})
    with os.fdopen(os.open(destination / "manifest.json", os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "w") as handle:
        json.dump(manifest, handle, indent=2)
        handle.write("\n")
    return {"directory": str(destination.resolve()), "files": len(content), "manifest": str((destination / "manifest.json").resolve()), "scope": manifest["scope"], "not_included": manifest["not_included"]}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", type=Path, required=True)
    parser.add_argument("--runtime-root", type=Path)
    parser.add_argument("--repo", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--backup-dir", type=Path, help="new private directory outside runtime data; copies inputs only")
    args = parser.parse_args()
    if args.runtime_root and args.output.resolve().is_relative_to(args.runtime_root.resolve()):
        parser.error("write the inventory outside the runtime root")
    if args.output.resolve() in {args.config.resolve(), args.config.with_name("llm-router.yaml").resolve()}:
        parser.error("the report must not overwrite its configuration inputs")
    if args.backup_dir and args.runtime_root and args.backup_dir.resolve().is_relative_to(args.runtime_root.resolve()):
        parser.error("write the snapshot outside the runtime root")
    report, known = inventory_config(args.config)
    report["mode"] = "read_only_preparation"
    report["runtime"] = inventory_runtime(args.runtime_root, known) if args.runtime_root else None
    report["source_references"] = inventory_sources(args.repo, known) if args.repo else None
    report["packaged_templates"] = inventory_runtime(args.repo / "magician_data_v3", known) if args.repo else None
    report["retirement_ready"] = False
    report["required_before_removal"] = ["first-class assignment fallbacks", "owned active-reference migration", "Web/iOS/Android preference migration", "sealed-run and approval re-admission or verified completion", "full residual-reference audit", "backup/rollback and current-source qualification"]
    report["backup"] = backup_inventory(report, args.backup_dir) if args.backup_dir else None
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"report": str(args.output), "aliases": len(report["alias_map"]),
                      "fallbacks": len(report["fallback_edges"]), "active_config_references": len(report["active_config_references"]),
                      "runtime_records_with_references": len((report["runtime"] or {}).get("records_with_legacy_references", [])),
                      "source_reference_locations": len(report["source_references"] or []), "retirement_ready": False}))


if __name__ == "__main__":
    main()
