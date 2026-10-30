#!/usr/bin/env python3
"""Prepare an alias-free config pair; never install it or rewrite runtime state.

Only configuration-owned references establish assignment roots. Source defaults,
client transports, owned definitions and follow-up admission need their own
migration before this preview can be activated.
"""
from __future__ import annotations

import argparse
import copy
import hashlib
import json
import os
from pathlib import Path

import yaml

from magician_config_text import read_config_text
from migrate_llm_catalog import materialize_migration_router, qualify_assignment_fallbacks, remove_legacy_router_tables, render_tables
from prepare_llm_profile_retirement import InventoryLoader, canonical_target, selection_migration


def rewrite(value, replacements: dict[str, str]):
    if isinstance(value, dict):
        result = {}
        for key, item in value.items():
            new_key = replacements.get(key, key)
            if new_key in result:
                raise ValueError(f"selector-key collision after conversion: {new_key}")
            result[new_key] = rewrite(item, replacements)
        return result
    if isinstance(value, list):
        return [rewrite(item, replacements) for item in value]
    return replacements.get(value, value) if isinstance(value, str) else value


def strings(value):
    if isinstance(value, dict):
        for key, item in value.items():
            yield str(key)
            yield from strings(item)
    elif isinstance(value, list):
        for item in value:
            yield from strings(item)
    elif isinstance(value, str):
        yield value


def operation_contracts(router: dict) -> tuple[dict, int, int]:
    """Compare request and recursive fallback contracts, ignoring route IDs only.

    This is the script-reader proof. Rust inspector/provider parity remains a
    separate deployment gate; Python cannot attest a compiled runtime by itself.
    """
    profiles = materialize_migration_router(router)["profiles"]
    arms = 0

    def request(name):
        profile = copy.deepcopy(profiles[name])
        fallback = profile.get("metadata", {}).pop("fallback_profile", None)
        return {"request": profile, "fallback": request(fallback) if fallback else None}

    def selector(value):
        nonlocal arms
        if isinstance(value, dict):
            return {key: selector(item) for key, item in value.items()}
        if isinstance(value, list):
            return [selector(item) for item in value]
        if isinstance(value, str) and value in profiles:
            arms += 1
            return request(value)
        return value

    contracts = {name: selector(value) for name, value in router.get("operation_mapping", {}).items()}
    return contracts, arms, len(profiles)


def convert(root: dict, renames: dict[str, str], keep_assignments=()) -> tuple[dict, dict, dict[str, str]]:
    result = copy.deepcopy(root)
    router = result["llm"]["router"]
    if router.get("profiles") or router.get("adaptive_profiles"):
        raise ValueError("convert authored legacy profiles to a catalog first")
    catalog = router.pop("catalog")
    if not catalog.get("models"):
        raise ValueError("model catalog is required")
    aliases = catalog.get("aliases", {})
    replacements = {old: canonical_target(old, aliases) for old in aliases}
    coding = result.get("coding", {})
    coding_rows = coding.get("profiles", [])
    for old, converted in selection_migration(catalog, coding_rows)["coding"].items():
        if not converted["client_resolvable"]:
            raise ValueError(f"coding choice needs an explicit replacement: {old}")
        selection = converted["model_selection"]
        replacements[old] = f"pi/model/{selection['model_id']}/{selection['thinking']}"
    coding.pop("profiles", None)
    result = rewrite(result, replacements)
    assignments = catalog.get("assignments", {})
    qualify_assignment_fallbacks(catalog)
    required = set(keep_assignments)
    if required - set(assignments):
        raise ValueError("required source assignment is unknown")
    roots = (set(strings(result)) & set(assignments)) | required
    retained = set()

    def retain(name):
        if name in retained:
            return
        retained.add(name)
        fallback = assignments[name].get("fallback")
        if fallback:
            retain(fallback)

    for name in roots:
        retain(name)
    if set(renames) - retained:
        raise ValueError("assignment renames must refer to retained assignments")
    if len(set(renames.values())) != len(renames) or any(
        not new or new.startswith("migration/") or (new in retained and new != old)
        for old, new in renames.items()
    ):
        raise ValueError("assignment rename collides or is not a semantic workload ID")
    catalog["assignments"] = rewrite({name: assignments[name] for name in sorted(retained)}, renames)
    catalog.pop("aliases", None)
    result = rewrite(result, renames)
    result["llm"]["router"]["catalog"] = catalog
    changed = {old: renames.get(new, new) for old, new in replacements.items()}
    changed.update(renames)
    return result, {
        "mode": "non_activated_config_owner_preview",
        "original_assignments": len(assignments), "retained_assignments": len(retained),
        "aliases": 0, "authored_coding_profiles": 0,
        "configuration_assignment_roots": sorted(renames.get(name, name) for name in roots),
        "explicit_source_assignment_roots": sorted(renames.get(name, name) for name in required),
        "assignment_renames": renames,
        "remaining_hashed_assignments": sorted(name for name in catalog["assignments"] if name.startswith("migration/")),
        "activation_ready": False,
        "remaining_owners": ["source/default selector consumers", "client/API contracts", "owned durable pins", "queues and follow-up admission", "full startup and route/fallback parity"],
    }, changed


def rewrite_main(text: str, replacements: dict[str, str]) -> str:
    """Replace only selector scalars and coding rows; preserve comments and tags."""
    text = remove_legacy_router_tables(text)
    tree = yaml.compose(text)
    edits = []
    seen = set()

    def visit(node, path=()):
        if id(node) in seen:
            return
        seen.add(id(node))
        if isinstance(node, yaml.MappingNode):
            for index, (key, item) in enumerate(node.value):
                if path == ("coding",) and key.value == "profiles":
                    if not isinstance(item, yaml.SequenceNode):
                        raise ValueError("coding.profiles must be a sequence")
                    if node.flow_style:
                        if index + 1 < len(node.value):
                            start, end = key.start_mark.index, node.value[index + 1][0].start_mark.index
                        elif index:
                            start, end = node.value[index - 1][1].end_mark.index, item.end_mark.index
                        else:
                            start, end = key.start_mark.index, item.end_mark.index
                    else:
                        start = key.start_mark.index - key.start_mark.column
                        def content_end(value):
                            if isinstance(value, yaml.ScalarNode) or value.flow_style:
                                return value.end_mark
                            children = [part for pair in value.value for part in pair] if isinstance(value, yaml.MappingNode) else value.value
                            return max((content_end(child) for child in children), key=lambda mark: mark.index, default=value.end_mark)
                        last = content_end(item)
                        end = last.index
                        if last.column:
                            newline = text.find("\n", end)
                            end = len(text) if newline < 0 else newline + 1
                    replacement = " " * key.start_mark.column + "{}\n" if not node.flow_style and len(node.value) == 1 else ""
                    edits.append((start, end, replacement))
                    continue
                visit(key)
                visit(item, (*path, key.value))
        elif isinstance(node, yaml.SequenceNode):
            for item in node.value:
                visit(item, path)
        elif isinstance(node, yaml.ScalarNode) and node.tag == "tag:yaml.org,2002:str" and node.value in replacements:
            value = json.dumps(replacements[node.value])
            edits.append((node.start_mark.index, node.end_mark.index, value))

    visit(tree)
    for start, end, replacement in sorted(edits, reverse=True):
        text = text[:start] + replacement + text[end:]
    return text


def prepare(config: Path, output: Path, renames: dict[str, str], keep_assignments=()) -> dict:
    if output.exists() or output.resolve() == config.resolve().parent:
        raise ValueError("output must be a new directory separate from the source")
    source = yaml.load(read_config_text(config, expand_catalog=False), Loader=InventoryLoader)
    converted, plan, replacements = convert(source, renames, keep_assignments)
    main = rewrite_main(config.read_text(), replacements)
    tables = render_tables(converted["llm"]["router"]["catalog"], converted["llm"]["router"].get("operation_mapping", {}))
    # Parse the exact emitted pair, including main-document anchors and Rust
    # enum tags, and compare all unrelated configuration before publishing.
    from magician_config_text import splice_router_tables_into
    emitted = yaml.load(splice_router_tables_into(main, tables), Loader=InventoryLoader)
    expected = copy.deepcopy(converted)
    for key in ("profiles", "adaptive_profiles"):
        expected["llm"]["router"].pop(key, None)
    if emitted != expected:
        raise ValueError("emitted config differs from the reviewed selector-only conversion")
    before, before_arms, before_routes = operation_contracts(source["llm"]["router"])
    after, after_arms, after_routes = operation_contracts(emitted["llm"]["router"])
    if before != after or before_arms != after_arms:
        raise ValueError("operation request/fallback contracts changed during conversion")
    plan["script_reader_operation_parity"] = {
        "operations": len(before), "selector_arms": before_arms,
        "before_routes": before_routes, "after_routes": after_routes,
        "request_or_fallback_contract_differences": [],
    }
    inputs = [config, config.with_name("llm-router.yaml")]
    plan["source_sha256"] = {path.name: hashlib.sha256(path.read_bytes()).hexdigest() for path in inputs if path.exists()}
    plan["output_sha256"] = {"magician-config.yaml": hashlib.sha256(main.encode()).hexdigest(), "llm-router.yaml": hashlib.sha256(tables.encode()).hexdigest()}
    output.mkdir(mode=0o700, parents=True, exist_ok=False)
    for name, content in {"magician-config.yaml": main, "llm-router.yaml": tables, "plan.json": json.dumps(plan, indent=2) + "\n"}.items():
        with os.fdopen(os.open(output / name, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "w") as handle:
            handle.write(content)
    return plan


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--rename-assignment", action="append", default=[], metavar="OLD=WORKLOAD_ID")
    parser.add_argument("--keep-assignment", action="append", default=[], metavar="ASSIGNMENT_ID",
                        help="Preserve a source-owned workload, including its fallback closure")
    args = parser.parse_args()
    renames = {}
    for entry in args.rename_assignment:
        if "=" not in entry:
            parser.error("--rename-assignment requires OLD=WORKLOAD_ID")
        old, new = entry.split("=", 1)
        if old in renames:
            parser.error("duplicate assignment rename")
        renames[old] = new
    plan = prepare(args.config, args.output_dir, renames, args.keep_assignment)
    print(json.dumps({"output": str(args.output_dir), "assignments": plan["retained_assignments"], "aliases": 0, "activation_ready": False}))


if __name__ == "__main__":
    main()
