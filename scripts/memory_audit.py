#!/usr/bin/env python3
"""Audit and optionally repair scoped Magician memory files.

Default mode is read-only. Use --migrate-shapes to rewrite known legacy tier
shapes such as entities.fields.value -> entities.fields.entities.
"""

from __future__ import annotations

import argparse
import json
import re
from collections import Counter
from pathlib import Path
from typing import Any

try:
    import yaml
except Exception:  # pragma: no cover - optional developer dependency
    yaml = None


if yaml is not None:
    class LooseYamlLoader(yaml.SafeLoader):
        """YAML loader that preserves values behind repo-specific tags."""

    def _construct_unknown_yaml_tag(loader: Any, _tag_suffix: str, node: Any) -> Any:
        if isinstance(node, yaml.ScalarNode):
            return loader.construct_scalar(node)
        if isinstance(node, yaml.SequenceNode):
            return loader.construct_sequence(node)
        if isinstance(node, yaml.MappingNode):
            return loader.construct_mapping(node)
        return None

    LooseYamlLoader.add_multi_constructor("!", _construct_unknown_yaml_tag)
else:
    LooseYamlLoader = None


KNOWN_COLLECTION_ROOTS = {
    "entities": "entities",
    "environment_knowledge": "environments",
}

CONSOLIDATION_AUDIT_FILE = "memory_consolidation_audit.jsonl"
AGENT_DEFINITION_FILE = "definition.agent.yaml"
NON_AGENT_MEMORY_DIRS = {"approvals", "proposals"}

TASK_OR_EXEC_ID_RE = re.compile(
    r"\b(?:task|exec|execution)_[0-9a-f]{16,}\b", re.IGNORECASE
)
BARE_HEX_TASK_RE = re.compile(r"\bTask [0-9a-f]{24,}\b", re.IGNORECASE)


def load_json(path: Path) -> Any | None:
    try:
        return json.loads(path.read_text())
    except Exception:
        return None


def write_json(path: Path, value: Any) -> None:
    path.write_text(json.dumps(value, indent=2, sort_keys=False) + "\n")


def memory_root(scope_root: Path) -> Path:
    root = scope_root / "memory"
    if not root.exists():
        raise SystemExit(f"memory root not found: {root}")
    return root


def iter_json_files(root: Path):
    yield from root.rglob("*.json")


def tier_name_from_path(path: Path) -> str | None:
    if path.parent.name != "tiers":
        return None
    name = path.stem
    for suffix in ("_task_",):
        if suffix in name:
            return name.split(suffix, 1)[0]
    return name


def user_tier_name_from_path(root: Path, path: Path) -> str | None:
    rel = path.relative_to(root)
    parts = rel.parts
    if len(parts) == 2 and parts[0] == "users" and path.name != "knowledge.json":
        return path.stem
    return None


def audit(
    scope_root: Path,
    migrate_shapes: bool,
    prune_transient_entities: bool,
    prune_noisy_entities: bool,
    agent_rule_audit: bool,
) -> dict[str, Any]:
    root = memory_root(scope_root)
    counts: Counter[str] = Counter()
    shape_mismatches: list[dict[str, Any]] = []
    duplicate_keys: list[dict[str, Any]] = []
    transient_entities: list[dict[str, Any]] = []
    noisy_entities: list[dict[str, Any]] = []
    migrated: list[str] = []
    pruned: list[dict[str, Any]] = []
    pruned_noisy: list[dict[str, Any]] = []
    agents: Counter[str] = Counter()
    episode_candidate_types: Counter[str] = Counter()
    episode_count = 0
    episodes_with_memory_candidates = 0
    memory_candidate_count = 0
    consolidation_audit_record_count = 0

    for path in iter_json_files(root):
        rel = path.relative_to(root)
        parts = rel.parts
        if len(parts) >= 3 and parts[0] == "agents":
            agents[parts[1]] += 1
            counts[f"agent:{parts[1]}:{parts[2]}"] += 1
        elif parts and parts[0] == "users":
            counts["users"] += 1

        value = load_json(path)
        if not isinstance(value, dict):
            continue

        if value.get("record_type") in {"memory_episode", "execution_terminal"}:
            episode_count += 1
            candidates = value.get("memory_candidates")
            if isinstance(candidates, list) and candidates:
                episodes_with_memory_candidates += 1
                memory_candidate_count += len(candidates)
                for candidate in candidates:
                    if isinstance(candidate, dict):
                        candidate_type = candidate.get("candidate_type")
                        if isinstance(candidate_type, str) and candidate_type.strip():
                            episode_candidate_types[candidate_type.strip()] += 1

        tier_name = tier_name_from_path(path) or user_tier_name_from_path(root, path)
        fields = value.get("fields")
        if tier_name and isinstance(fields, dict):
            expected_root = KNOWN_COLLECTION_ROOTS.get(tier_name)
            if expected_root and "value" in fields and expected_root not in fields:
                shape_mismatches.append(
                    {
                        "path": str(path),
                        "tier": tier_name,
                        "legacy_field": "value",
                        "expected_field": expected_root,
                        "item_count": len(fields["value"])
                        if isinstance(fields["value"], list)
                        else None,
                    }
                )
                if migrate_shapes:
                    fields[expected_root] = fields.pop("value")
                    write_json(path, value)
                    migrated.append(str(path))

            for field_name, field_value in fields.items():
                if isinstance(field_value, list):
                    seen: dict[str, int] = {}
                    for idx, item in enumerate(field_value):
                        key = memory_item_key(item)
                        if not key:
                            continue
                        if key in seen:
                            duplicate_keys.append(
                                {
                                    "path": str(path),
                                    "field": field_name,
                                    "key": key,
                                    "first_index": seen[key],
                                    "duplicate_index": idx,
                                }
                            )
                        else:
                            seen[key] = idx

            if tier_name == "entities":
                entity_field = fields.get("entities", fields.get("value"))
                if isinstance(entity_field, list):
                    kept: list[Any] = []
                    removed_here: list[dict[str, Any]] = []
                    noisy_removed_here: list[dict[str, Any]] = []
                    for idx, item in enumerate(entity_field):
                        reason = transient_entity_reason(item)
                        if reason:
                            entry = {
                                "path": str(path),
                                "index": idx,
                                "name": item.get("name") if isinstance(item, dict) else None,
                                "type": item.get("type") if isinstance(item, dict) else None,
                                "reason": reason,
                            }
                            transient_entities.append(entry)
                            removed_here.append(entry)
                            if prune_transient_entities:
                                continue

                        noisy_reason = noisy_entity_reason(item)
                        if noisy_reason:
                            entry = {
                                "path": str(path),
                                "index": idx,
                                "name": item.get("name") if isinstance(item, dict) else None,
                                "type": item.get("type") if isinstance(item, dict) else None,
                                "reason": noisy_reason,
                            }
                            noisy_entities.append(entry)
                            noisy_removed_here.append(entry)
                            if prune_noisy_entities:
                                continue

                        kept.append(item)
                    if (prune_transient_entities and removed_here) or (
                        prune_noisy_entities and noisy_removed_here
                    ):
                        target_field = "entities" if "entities" in fields else "value"
                        fields[target_field] = kept
                        write_json(path, value)
                        if prune_transient_entities:
                            pruned.extend(removed_here)
                        if prune_noisy_entities:
                            pruned_noisy.extend(noisy_removed_here)

    for path in root.rglob(CONSOLIDATION_AUDIT_FILE):
        try:
            with path.open() as handle:
                consolidation_audit_record_count += sum(
                    1 for line in handle if line.strip()
                )
        except OSError:
            continue

    report = {
        "scope_root": str(scope_root),
        "file_counts": dict(counts),
        "agents": dict(agents),
        "shape_mismatch_count": len(shape_mismatches),
        "shape_mismatches": shape_mismatches[:200],
        "duplicate_key_count": len(duplicate_keys),
        "duplicate_keys": duplicate_keys[:200],
        "transient_entity_count": len(transient_entities),
        "transient_entities": transient_entities[:200],
        "noisy_entity_count": len(noisy_entities),
        "noisy_entities": noisy_entities[:200],
        "episode_count": episode_count,
        "episodes_with_memory_candidates": episodes_with_memory_candidates,
        "memory_candidate_count": memory_candidate_count,
        "memory_candidate_types": dict(episode_candidate_types),
        "consolidation_audit_record_count": consolidation_audit_record_count,
        "migrated_count": len(migrated),
        "migrated": migrated,
        "pruned_transient_entity_count": len(pruned),
        "pruned_transient_entities": pruned[:200],
        "pruned_noisy_entity_count": len(pruned_noisy),
        "pruned_noisy_entities": pruned_noisy[:200],
    }
    if agent_rule_audit:
        tier_rule_report = audit_agent_memory_rules(scope_root, root)
        report["agent_rule_audit"] = tier_rule_report
        report["agent_rule_audit_issue_count"] = tier_rule_report.get("issue_count", 0)
    return report


def memory_item_key(value: Any) -> str | None:
    if not isinstance(value, dict):
        return None
    for field in ("key", "environment_key", "pattern", "insight", "name", "source_id", "id"):
        raw = value.get(field)
        if isinstance(raw, str) and raw.strip():
            return f"{field}:{' '.join(raw.lower().split())}"
    return None


def transient_entity_reason(value: Any) -> str | None:
    if not isinstance(value, dict):
        return None
    name = str(value.get("name") or "").strip()
    if not name:
        return None
    name_lower = name.lower()

    if TASK_OR_EXEC_ID_RE.search(name) or BARE_HEX_TASK_RE.search(name):
        return "task_or_execution_identifier"
    if "outputs/out_" in name_lower or "executions/exec_" in name_lower:
        return "execution_artifact_path"
    if "localhost:5173/tests/sota-tests/" in name_lower:
        return "local_sota_test_url"
    if name_lower.startswith(("/tests/sota-tests/", "tests/sota-tests/")):
        return "local_sota_test_path"
    if name_lower.startswith("debug: navigate") and (
        "localhost" in name_lower or "sota-tests" in name_lower
    ):
        return "debug_task_title"

    attributes = value.get("attributes")
    if isinstance(attributes, dict):
        for key in ("task_id", "goal_key", "execution_id"):
            raw = attributes.get(key)
            if isinstance(raw, str) and TASK_OR_EXEC_ID_RE.search(raw):
                return "task_or_execution_attribute"
        for raw in attributes.values():
            if isinstance(raw, str) and (
                "localhost:5173/tests/sota-tests/" in raw.lower()
                or TASK_OR_EXEC_ID_RE.search(raw)
            ):
                return "transient_attribute_reference"
    return None


LOCAL_BROWSER_TEST_MARKERS = (
    "localhost:5173",
    "/tests/sota-tests/",
    "tests/sota-tests",
    "sota test",
    "sota-tests",
    "window.testrunner",
    "testrunner",
    "test case",
    "test page",
    "test suite",
    "test runner",
    "pass/fail",
    "pass radio",
    "fail radio",
    "status radio",
    "manual pass",
    "manual pass/fail",
    "pending cases",
    "summary counts",
    "visible test",
    "browser test",
    "debug/test",
)

TEST_OR_BROWSER_RUNTIME_MARKERS = (
    "browser:evaluate",
    "browser:click_coords",
    "setteststatus",
    "setresult",
    "setcheckstate",
    "patch dom",
    "directly mark tests passed",
    "execution failed",
    "execution cancelled",
    "execution was cancelled",
    "goal_achieved",
    "goal achieved",
    "goal_reached",
    "outcome_kind",
    "outcome_type",
    "max_iterations_reached",
    "no tab with given id",
    "schema validation error",
)

LOCAL_TEST_FILE_RE = re.compile(r"\b\d{2}-[a-z0-9-]+\.html\b", re.IGNORECASE)
TEST_ID_RE = re.compile(r"\btest-\d+\b", re.IGNORECASE)
BROWSER_REF_RE = re.compile(r"^@e\d+$", re.IGNORECASE)

DURABLE_PROJECT_TERMS = (
    "flight",
    "bengaluru",
    "bhubaneswar",
    "blr",
    "bbi",
    "bbs",
    "us-iran",
)

DURABLE_WORKFLOW_TERMS = (
    "flight",
    "linkedin",
    "google sign-in",
    "imessage",
    "web",
)

NOISY_ENVIRONMENT_NAMES = {
    "/tmp/sota23-runtime/",
    "browser session",
    "card number field",
    "codesandbox iframe",
    "contenteditable",
    "cvc field",
    "devtools browser proxy",
    "default workspace",
    "expiry field",
    "focus ring",
    "harbor depot",
    "headed browser session",
    "important-config.json",
    "local host 5173",
    "localhost development server",
    "localhost vite test server",
    "open browser session",
    "pay $99.00 button",
    "payment form iframe",
    "payment iframe",
    "results summary",
    "review lane",
    "rick astley - never gonna give you up",
    "shadow dom",
    "sota23-runtime",
    "temp-min-handle",
    "test results",
    "textarea",
    "transit hub",
}

NOISY_ORGANIZATION_NAMES = {
    "247checkers.com",
    "codesandbox",
    "youtube",
    "www.247checkers.com",
}

NOISY_PREFERENCE_NAMES = {
    "headed",
    "keep browser session alive",
    "keep_browser_session_alive",
    "lowest medium setting",
    "page-owned visible verification",
    "pass",
    "pass-all-successful-test-cases",
    "red",
}

DURABLE_TOOL_NAMES = {
    "agent-browser",
    "dashboard",
    "wikipedia",
}


def noisy_entity_reason(value: Any) -> str | None:
    """Classify broader non-durable entity-tier noise.

    These are intentionally scoped to entity records that look like local
    browser-test harness state, execution status pseudo-entities, DOM handles,
    or sample data. Durable facts from user preferences, people, real
    organizations, Metabase, flights, and research topics should not match.
    """

    if not isinstance(value, dict):
        return None

    entity_type = str(value.get("type") or "").strip().lower()
    name = str(value.get("name") or "").strip()
    if not name:
        return None
    name_lower = name.lower()
    attributes = value.get("attributes")
    attributes_text = json.dumps(attributes, sort_keys=True).lower()
    combined = f"{name_lower} {attributes_text}"

    if BROWSER_REF_RE.match(name):
        return "ephemeral_browser_element_ref"

    if entity_type in {"account", "person", "channel"}:
        if (
            "target.user@example.com" in name_lower
            or "deep.target@example.com" in name_lower
            or "4242 4242" in name_lower
            or ("test" in combined and name_lower in {"alice", "codesandbox editor"})
            or name_lower in {"dom", "console", "network", "youtube iframe"}
        ):
            return "sample_or_runtime_test_fixture"

    if entity_type in {"environment", "project", "workflow", "pattern", "tool", "channel"}:
        if any(marker in combined for marker in LOCAL_BROWSER_TEST_MARKERS):
            return "local_browser_test_harness_entity"
        if any(marker in combined for marker in TEST_OR_BROWSER_RUNTIME_MARKERS):
            return "execution_status_or_browser_runtime_entity"
        if LOCAL_TEST_FILE_RE.search(combined) or TEST_ID_RE.search(combined):
            return "local_test_case_identifier"

    if entity_type in {"environment", "tool"}:
        if name_lower in {
            "execution_terminal",
            "v3_memory_episode/v1",
            "magician_data_v3",
            "anonymous/default/general",
            "ui_thread_id: general",
            "workspace:default",
            "default",
            "local environment",
            "local development server",
            "localhost:5173",
            "http://localhost:5173",
        }:
            return "local_runtime_environment_metadata"

    if entity_type == "account" and name_lower in {
        "anonymous",
        "anonymous principal",
        "principal:anonymous",
    }:
        return "local_runtime_identity_metadata"

    if entity_type == "channel" and name_lower in {
        "general",
        "general ui thread",
        "ui_thread general",
        "ui_thread:general",
    }:
        return "local_runtime_channel_metadata"

    if entity_type == "environment":
        if name_lower in NOISY_ENVIRONMENT_NAMES:
            return "local_fixture_environment_entity"
        if any(token in combined for token in ("iframe", "canvas", "localhost", "sota")):
            return "local_fixture_environment_entity"

    if entity_type == "organization" and name_lower in NOISY_ORGANIZATION_NAMES:
        return "sample_or_runtime_test_fixture"

    if entity_type == "person" and name_lower == "personal assistant":
        return "agent_identity_entity"

    if entity_type == "preference" and name_lower in NOISY_PREFERENCE_NAMES:
        return "local_browser_test_preference_entity"

    if entity_type == "pattern":
        if "flight" not in combined and "cheapest valid flight combination" not in name_lower:
            return "non_durable_pattern_entity"

    if entity_type == "project":
        if not any(term in combined for term in DURABLE_PROJECT_TERMS):
            return "non_durable_project_entity"

    if entity_type == "tool":
        if name_lower not in DURABLE_TOOL_NAMES:
            return "non_durable_tool_entity"

    if entity_type == "workflow":
        if not any(term in combined for term in DURABLE_WORKFLOW_TERMS):
            return "non_durable_workflow_entity"

    return None


def audit_agent_memory_rules(scope_root: Path, root: Path) -> dict[str, Any]:
    """Compare scoped agent memory definitions against persisted memory files."""

    if yaml is None or LooseYamlLoader is None:
        return {
            "available": False,
            "issue_count": 1,
            "issues": [
                {
                    "kind": "yaml_unavailable",
                    "message": "Install PyYAML to enable agent definition tier/rule audit.",
                }
            ],
            "agents": [],
        }

    definitions_root = scope_root / "agent_runtime" / "agents"
    definition_paths = sorted(definitions_root.glob(f"*/{AGENT_DEFINITION_FILE}"))
    memory_agents_root = root / "agents"
    memory_agents = (
        {
            path.name
            for path in memory_agents_root.iterdir()
            if path.is_dir() and path.name not in NON_AGENT_MEMORY_DIRS
        }
        if memory_agents_root.exists()
        else set()
    )

    agents: list[dict[str, Any]] = []
    issues: list[dict[str, Any]] = []
    defined_agent_ids: set[str] = set()

    for path in definition_paths:
        data, parse_error = load_agent_definition_yaml(path)
        agent_id = path.parent.name
        if isinstance(data, dict):
            raw_agent_id = data.get("agent_id")
            if isinstance(raw_agent_id, str) and raw_agent_id.strip():
                agent_id = raw_agent_id.strip()
        defined_agent_ids.add(agent_id)

        agent_issues: list[dict[str, Any]] = []
        if parse_error:
            issue = {
                "kind": "definition_parse_error",
                "agent_id": agent_id,
                "path": str(path),
                "message": parse_error,
            }
            agent_issues.append(issue)
            issues.append(issue)
            agents.append(
                {
                    "agent_id": agent_id,
                    "definition_path": str(path),
                    "tier_count": 0,
                    "rule_count": 0,
                    "persisted_tiers": persisted_agent_tiers(root, agent_id),
                    "tiers": [],
                    "rules": [],
                    "issues": agent_issues,
                }
            )
            continue

        tiers = data.get("memory_tiers") if isinstance(data, dict) else None
        rules = data.get("memory_consolidation") if isinstance(data, dict) else None
        prompt_pipeline = data.get("prompt_pipeline") if isinstance(data, dict) else None
        tiers = tiers if isinstance(tiers, list) else []
        rules = rules if isinstance(rules, list) else []
        tier_names = {
            str(tier.get("name")).strip()
            for tier in tiers
            if isinstance(tier, dict) and str(tier.get("name") or "").strip()
        }
        tiers_by_name = {
            str(tier.get("name")).strip(): tier
            for tier in tiers
            if isinstance(tier, dict) and str(tier.get("name") or "").strip()
        }
        persisted_tiers = persisted_agent_tiers(root, agent_id)
        writer_roots: set[str] = set()
        source_roots: set[str] = set()
        prompt_tier_refs = extract_prompt_tier_refs(prompt_pipeline)

        tier_summaries = []
        for tier in tiers:
            if not isinstance(tier, dict):
                continue
            name = str(tier.get("name") or "").strip()
            render = tier.get("render") if isinstance(tier.get("render"), dict) else {}
            tier_summaries.append(
                {
                    "name": name,
                    "scope": tier.get("scope"),
                    "description": tier.get("description"),
                    "has_schema": isinstance(tier.get("schema"), dict),
                    "has_render_template": bool(render.get("template")),
                    "referenced_by_prompt_pipeline": name in prompt_tier_refs,
                    "persisted": name in persisted_tiers,
                }
            )

        rule_summaries = []
        for rule in rules:
            if not isinstance(rule, dict):
                continue
            name = str(rule.get("name") or "").strip()
            source = str(rule.get("source") or "").strip()
            target = str(rule.get("target") or "").strip()
            transform = rule.get("transform") if isinstance(rule.get("transform"), dict) else {}
            transform_type = transform.get("type")
            target_root = memory_target_root(target)
            if target_root:
                if target_root in tier_names:
                    writer_roots.add(target_root)
                else:
                    issue = {
                        "kind": "target_unknown_tier",
                        "agent_id": agent_id,
                        "rule": name,
                        "target": target,
                        "target_root": target_root,
                    }
                    agent_issues.append(issue)
                    issues.append(issue)
            for source_root in source_tier_refs(source):
                if source_root in tier_names:
                    source_roots.add(source_root)
                else:
                    issue = {
                        "kind": "source_unknown_tier",
                        "agent_id": agent_id,
                        "rule": name,
                        "source": source,
                        "source_root": source_root,
                    }
                    agent_issues.append(issue)
                    issues.append(issue)
            rule_summaries.append(
                {
                    "name": name,
                    "trigger": rule.get("trigger"),
                    "source": source,
                    "target": target,
                    "target_root": target_root,
                    "transform_type": transform_type,
                    "builtin": transform.get("builtin"),
                    "prompt": transform.get("prompt"),
                    "system_prompt": transform.get("system_prompt"),
                    "merge": transform.get("merge"),
                    "operation": transform.get("operation"),
                }
            )
            if (
                transform_type == "llm"
                and not transform.get("operation")
                and not transform.get("system_prompt")
            ):
                issue = {
                    "kind": "llm_transform_operation_inferred",
                    "agent_id": agent_id,
                    "rule": name,
                    "target": target,
                    "message": (
                        "set transform.operation or a managed system_prompt with an "
                        "operation tag; runtime target inference is compatibility-only"
                    ),
                }
                agent_issues.append(issue)
                issues.append(issue)

        for tier_file in sorted((root / "agents" / agent_id / "tiers").glob("*.json")):
            tier_name = tier_name_from_path(tier_file)
            tier_definition = tiers_by_name.get(tier_name or "")
            if not tier_definition:
                continue
            persisted = load_json(tier_file)
            fields = persisted.get("fields") if isinstance(persisted, dict) else None
            schema = tier_definition.get("schema")
            for schema_issue in validate_persisted_tier_schema(fields, schema):
                issue = {
                    "kind": "persisted_tier_schema_mismatch",
                    "agent_id": agent_id,
                    "tier": tier_name,
                    "path": str(tier_file),
                    **schema_issue,
                }
                agent_issues.append(issue)
                issues.append(issue)

        for tier_name in persisted_tiers:
            if tier_name not in tier_names:
                issue = {
                    "kind": "orphan_persisted_tier",
                    "agent_id": agent_id,
                    "tier": tier_name,
                }
                agent_issues.append(issue)
                issues.append(issue)

        for tier_name in sorted(tier_names):
            if tier_name == "episode":
                continue
            if tier_name not in writer_roots and tier_name not in persisted_tiers:
                issue = {
                    "kind": "tier_has_no_writer_or_persisted_state",
                    "agent_id": agent_id,
                    "tier": tier_name,
                }
                agent_issues.append(issue)
                issues.append(issue)

        agents.append(
            {
                "agent_id": agent_id,
                "definition_path": str(path),
                "tier_count": len(tier_summaries),
                "rule_count": len(rule_summaries),
                "persisted_tiers": persisted_tiers,
                "writer_tiers": sorted(writer_roots),
                "source_tiers": sorted(source_roots),
                "prompt_tier_refs": sorted(prompt_tier_refs),
                "tiers": tier_summaries,
                "rules": rule_summaries,
                "issues": agent_issues,
            }
        )

    for agent_id in sorted(memory_agents - defined_agent_ids):
        issue = {
            "kind": "memory_agent_without_definition",
            "agent_id": agent_id,
        }
        issues.append(issue)

    return {
        "available": True,
        "definition_count": len(definition_paths),
        "memory_agent_count": len(memory_agents),
        "issue_count": len(issues),
        "issues": issues[:500],
        "agents": agents,
    }


def load_agent_definition_yaml(path: Path) -> tuple[Any | None, str | None]:
    try:
        return yaml.load(path.read_text(), Loader=LooseYamlLoader), None
    except Exception as exc:
        return None, str(exc)


def validate_persisted_tier_schema(fields: Any, schema: Any) -> list[dict[str, Any]]:
    """Validate every persisted tier against its owning declarative schema."""

    if not isinstance(fields, dict):
        return [{"field": "fields", "message": "tier fields must be an object"}]
    if not isinstance(schema, dict):
        return []
    issues: list[dict[str, Any]] = []
    for field in sorted(fields):
        if field not in schema:
            issues.append({"field": field, "message": "off-schema root field"})
            continue
        issues.extend(validate_schema_value(fields[field], schema[field], field))
    return issues


def validate_schema_value(value: Any, schema: Any, path: str) -> list[dict[str, Any]]:
    if value is None or not isinstance(schema, dict):
        return []
    field_type = schema.get("type")
    if field_type == "collection":
        if not isinstance(value, list):
            return [{"field": path, "message": "collection must be an array"}]
        max_items = schema.get("max_items")
        issues: list[dict[str, Any]] = []
        if isinstance(max_items, int) and len(value) > max_items:
            issues.append(
                {
                    "field": path,
                    "message": f"collection exceeds max_items ({len(value)} > {max_items})",
                }
            )
        item_schema = schema.get("item_schema")
        if isinstance(item_schema, dict):
            for index, item in enumerate(value):
                if not isinstance(item, dict):
                    issues.append(
                        {"field": f"{path}[{index}]", "message": "item must be an object"}
                    )
                    continue
                for item_field in sorted(item):
                    item_path = f"{path}[{index}].{item_field}"
                    if item_field not in item_schema:
                        issues.append(
                            {"field": item_path, "message": "off-schema item field"}
                        )
                    else:
                        issues.extend(
                            validate_schema_value(
                                item[item_field], item_schema[item_field], item_path
                            )
                        )
        return issues
    if field_type == "key_value_list" and not isinstance(value, (dict, list)):
        return [{"field": path, "message": "key_value_list must be an object or array"}]
    if field_type in {"text", "date_time"} and isinstance(value, (dict, list)):
        return [{"field": path, "message": f"{field_type} must be a scalar"}]
    return []


def persisted_agent_tiers(root: Path, agent_id: str) -> list[str]:
    tier_root = root / "agents" / agent_id / "tiers"
    if not tier_root.exists():
        return []
    names = {
        tier_name_from_path(path) or path.stem
        for path in tier_root.glob("*.json")
        if not path.name.startswith(".")
    }
    return sorted(name for name in names if name)


SOURCE_TIER_RE = re.compile(r"tiers\(([^)]*)\)")
PROMPT_TIER_RE = re.compile(r"memory\.tier\[([^\]]+)\]")


def source_tier_refs(source: str) -> list[str]:
    refs: list[str] = []
    for match in SOURCE_TIER_RE.finditer(source):
        refs.extend(
            item.strip()
            for item in match.group(1).split(",")
            if item.strip()
        )
    return refs


def memory_target_root(target: str) -> str | None:
    if not target:
        return None
    lowered = target.lower()
    if lowered == "user" or lowered.startswith("user."):
        return None
    if lowered.startswith(("report:", "reports.", "artifact:", "artifacts.", "none")):
        return None
    root = target.split(".", 1)[0].split(":", 1)[0].strip()
    return root or None


def extract_prompt_tier_refs(prompt_pipeline: Any) -> set[str]:
    refs: set[str] = set()
    if not isinstance(prompt_pipeline, dict):
        return refs
    sections = prompt_pipeline.get("sections")
    if not isinstance(sections, list):
        return refs
    for section in sections:
        if not isinstance(section, dict):
            continue
        source = section.get("source")
        if isinstance(source, str):
            refs.update(match.group(1).strip() for match in PROMPT_TIER_RE.finditer(source))
    return {ref for ref in refs if ref}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--scope-root",
        default="magician_data_v3/scopes/anonymous/default",
        help="Scoped workspace root containing memory/",
    )
    parser.add_argument(
        "--migrate-shapes",
        action="store_true",
        help="Rewrite known legacy fields.value collection roots in place.",
    )
    parser.add_argument(
        "--prune-transient-entities",
        action="store_true",
        help="Remove conservative task/exec/url artifact entities from entities tiers.",
    )
    parser.add_argument(
        "--prune-noisy-entities",
        action="store_true",
        help="Remove broader local browser-test/runtime fixture noise from entities tiers.",
    )
    parser.add_argument(
        "--agent-rule-audit",
        action="store_true",
        help="Inspect scoped agent memory tiers/rules and report orphan tiers or unknown rule targets.",
    )
    args = parser.parse_args()

    report = audit(
        Path(args.scope_root),
        args.migrate_shapes,
        args.prune_transient_entities,
        args.prune_noisy_entities,
        args.agent_rule_audit,
    )
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
