#!/usr/bin/env python3
"""Run the non-executing Phase 0E2 live-agentic catalog baseline.

The evaluator sends the active agentic decision prompts and real checked skill
schemas to the configured OpenAI Responses profile. It records provider tool
calls but never dispatches them, resolves credentials, opens auth sessions, or
executes side effects. The decisive gate is five runs for every frozen Phase
0E1 representative contract.
"""

from __future__ import annotations

import argparse
import hashlib
import html
import importlib.util
import json
import os
import re
import statistics
import struct
import sys
import tempfile
from concurrent.futures import ThreadPoolExecutor
from dataclasses import asdict, dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

import yaml
from jsonschema import Draft202012Validator
from yaml.tokens import (
    AliasToken,
    AnchorToken,
    BlockEndToken,
    BlockMappingStartToken,
    BlockSequenceStartToken,
    FlowMappingEndToken,
    FlowMappingStartToken,
    FlowSequenceEndToken,
    FlowSequenceStartToken,
)


MANIFEST_SCHEMA = "tool-runtime.phase0-live-agentic-manifest.v1"
OFFLINE_SCHEMA = "tool-runtime.phase0-offline-baseline.v1"
MAX_INPUT_BYTES = 8 * 1024 * 1024
MAX_YAML_DEPTH = 64
MAX_YAML_TOKENS = 250_000
MAX_YAML_REFERENCES = 2_048
MAX_TASKS = 64
MAX_EXPECTATIONS = 64
CORE_TOOL_NAMES = {"tool_search", "yield", "need_user_input"}
KNOWN_PROFILE_BINDINGS = {
    "none",
    "argument",
    "runtime_bound",
    "runtime_fixed",
    "runtime_implicit",
}
KNOWN_OPERATORS = {
    "equals",
    "contains",
    "contains_terms",
    "list_equals",
    "json_subset",
}


@dataclass(frozen=True)
class Task:
    id: str
    goal: str
    success_criteria: str
    state: str
    expected_action_id: str
    expected_tool: str
    catalog_skills: tuple[str, ...]
    profile_binding: dict[str, Any]
    argument_expectations: tuple[dict[str, Any], ...]
    approval_argument_expectations: tuple[dict[str, Any], ...]
    auth_strategies: tuple[str, ...]
    auth_requirement: str
    profile_policy: str
    approval_class: str
    runtime_owner: str


@dataclass
class Result:
    task: str
    run_index: int
    profile: str
    model: str
    status_code: int
    response_status: str | None
    response_id: str | None
    total_ms: int
    first_output_ms: int | None
    tool_decision_ms: int | None
    retries: int
    input_tokens: int | None
    cached_tokens: int | None
    output_tokens: int | None
    reasoning_tokens: int | None
    cost_usd: float | None
    catalog_tools: int
    catalog_schema_bytes: int
    auth_strategies: list[str]
    auth_requirement: str
    profile_policy: str
    profile_binding_source: str
    approval_class: str
    runtime_owner: str
    selected_tools: list[str]
    tool_calls: list[dict[str, Any]]
    http_success: bool
    catalog_confined: bool
    exact_first_action_pass: bool
    exact_action_selection_pass: bool
    argument_semantics_pass: bool
    schema_validity_pass: bool
    auth_profile_correctness_pass: bool
    approval_correctness_pass: bool
    decision_success: bool
    error: str | None


def load_module(path: Path, name: str) -> Any:
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load helper module from {path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def read_bounded(path: Path, label: str) -> bytes:
    if path.is_symlink() or not path.is_file():
        raise ValueError(f"{label} is not a regular non-symlink file: {path}")
    with path.open("rb") as handle:
        value = handle.read(MAX_INPUT_BYTES + 1)
    if len(value) > MAX_INPUT_BYTES:
        raise ValueError(f"{label} exceeds the {MAX_INPUT_BYTES}-byte limit")
    return value


def read_utf8(path: Path, label: str) -> str:
    return read_bounded(path, label).decode("utf-8")


def load_json(path: Path, label: str) -> Any:
    return json.loads(read_utf8(path, label))


def bounded_yaml(source: str, label: str) -> Any:
    depth = 0
    tokens = 0
    references = 0
    starts = (
        BlockMappingStartToken,
        BlockSequenceStartToken,
        FlowMappingStartToken,
        FlowSequenceStartToken,
    )
    ends = (BlockEndToken, FlowMappingEndToken, FlowSequenceEndToken)
    for token in yaml.scan(source):
        tokens += 1
        if tokens > MAX_YAML_TOKENS:
            raise ValueError(f"{label} exceeds the YAML token limit")
        if isinstance(token, starts):
            depth += 1
            if depth > MAX_YAML_DEPTH:
                raise ValueError(f"{label} exceeds the YAML depth limit")
        elif isinstance(token, ends):
            depth = max(0, depth - 1)
        elif isinstance(token, (AliasToken, AnchorToken)):
            references += 1
            if references > MAX_YAML_REFERENCES:
                raise ValueError(f"{label} exceeds the YAML reference limit")
    return yaml.safe_load(source)


def load_yaml(path: Path, label: str) -> Any:
    return bounded_yaml(read_utf8(path, label), label)


def exact_keys(value: dict[str, Any], expected: set[str], label: str) -> None:
    unknown = set(value) - expected
    if unknown:
        raise ValueError(f"{label} contains unknown fields: {sorted(unknown)}")


def validate_identifier(value: Any, label: str) -> str:
    if not isinstance(value, str) or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,127}", value):
        raise ValueError(f"{label} is not a portable identifier")
    return value


def action_tool_name(action_id: str) -> str:
    parts = action_id.split("::")
    if len(parts) != 2:
        raise ValueError(f"invalid action id: {action_id}")
    validate_identifier(parts[0], "skill id")
    validate_identifier(parts[1], "action id")
    return f"{parts[0]}__{parts[1]}"


def validate_expectations(raw: Any, label: str) -> tuple[dict[str, Any], ...]:
    if raw is None:
        return ()
    if not isinstance(raw, list) or len(raw) > MAX_EXPECTATIONS:
        raise ValueError(f"{label} must be a bounded list")
    result: list[dict[str, Any]] = []
    for index, item in enumerate(raw):
        if not isinstance(item, dict):
            raise ValueError(f"{label}[{index}] is not a mapping")
        exact_keys(item, {"path", "operator", "value"}, f"{label}[{index}]")
        path = item.get("path")
        operator = item.get("operator")
        if not isinstance(path, str) or not path or len(path) > 128:
            raise ValueError(f"{label}[{index}].path is invalid")
        if operator not in KNOWN_OPERATORS:
            raise ValueError(f"{label}[{index}] uses an unknown operator")
        expected = item.get("value")
        if operator == "contains_terms" and (
            not isinstance(expected, list)
            or not expected
            or len(expected) > 32
            or any(
                not isinstance(term, str) or not term or len(term) > 128
                for term in expected
            )
        ):
            raise ValueError(
                f"{label}[{index}].value must be a bounded non-empty string list"
            )
        result.append(dict(item))
    return tuple(result)


def load_contract(manifest_path: Path, baseline_path: Path) -> tuple[dict[str, Any], list[Task]]:
    manifest = load_yaml(manifest_path, "Phase 0E2 manifest")
    baseline = load_json(baseline_path, "Phase 0E1 baseline")
    if not isinstance(manifest, dict) or not isinstance(baseline, dict):
        raise ValueError("Phase 0 contracts must be mappings")
    exact_keys(manifest, {"schema_version", "offline_baseline", "surface", "gates", "tasks"}, "manifest")
    if manifest.get("schema_version") != MANIFEST_SCHEMA:
        raise ValueError("Phase 0E2 manifest schema has drifted")
    if baseline.get("schema_version") != OFFLINE_SCHEMA:
        raise ValueError("Phase 0E1 baseline schema has drifted")

    offline = manifest.get("offline_baseline")
    surface = manifest.get("surface")
    gates = manifest.get("gates")
    raw_tasks = manifest.get("tasks")
    if not all(isinstance(value, dict) for value in (offline, surface, gates)):
        raise ValueError("manifest contract sections must be mappings")
    if not isinstance(raw_tasks, list) or not raw_tasks or len(raw_tasks) > MAX_TASKS:
        raise ValueError("manifest tasks must be a bounded non-empty list")
    exact_keys(offline, {"schema_version", "catalog_digest"}, "offline_baseline")
    exact_keys(
        surface,
        {
            "decision_prompt_version",
            "system_prompt_version",
            "catalog_mode",
            "execution_mode",
            "tool_choice",
            "max_output_tokens",
            "max_tools_per_request",
        },
        "surface",
    )
    exact_keys(
        gates,
        {
            "minimum_runs_per_task",
            "http_success_rate",
            "catalog_confinement_rate",
            "schema_validity_rate",
            "auth_profile_correctness_rate",
            "approval_correctness_rate",
            "exact_first_action_rate",
            "decision_success_rate",
            "minimum_successes_per_task",
            "require_provider_usage",
            "require_priced_cost",
        },
        "gates",
    )
    if offline.get("schema_version") != baseline.get("schema_version"):
        raise ValueError("manifest points at a different offline baseline schema")
    catalog = baseline.get("catalog") or {}
    if offline.get("catalog_digest") != catalog.get("digest"):
        raise ValueError("manifest points at a different offline catalog digest")
    if surface.get("catalog_mode") != "loaded_family_with_cross_family_distractors_v1" or surface.get("execution_mode") != "provider_decision_only" or surface.get("tool_choice") != "required":
        raise ValueError("Phase 0E2 surface contract has drifted")
    if not 1 <= int(surface.get("max_tools_per_request", 0)) <= 128:
        raise ValueError("max_tools_per_request is outside the supported bound")
    if int(gates.get("minimum_runs_per_task", 0)) < 5:
        raise ValueError("decisive live evidence requires at least five runs per task")

    offline_tasks = {item["id"]: item for item in baseline.get("representative_tasks") or []}
    if len(offline_tasks) != len(baseline.get("representative_tasks") or []):
        raise ValueError("offline representative task ids are duplicated")
    tasks: list[Task] = []
    seen: set[str] = set()
    task_keys = {
        "id",
        "goal",
        "success_criteria",
        "state",
        "expected_action",
        "catalog_skills",
        "profile_binding",
        "argument_expectations",
        "approval_argument_expectations",
    }
    for index, raw in enumerate(raw_tasks):
        if not isinstance(raw, dict):
            raise ValueError(f"tasks[{index}] is not a mapping")
        exact_keys(raw, task_keys, f"tasks[{index}]")
        task_id = validate_identifier(raw.get("id"), "task id")
        if task_id in seen:
            raise ValueError(f"duplicate task id: {task_id}")
        seen.add(task_id)
        offline_task = offline_tasks.get(task_id)
        if offline_task is None:
            raise ValueError(f"task {task_id} is absent from the offline corpus")
        expected_action = str(raw.get("expected_action") or "")
        if expected_action not in offline_task.get("action_ids", []):
            raise ValueError(f"task {task_id} expected action is outside its offline contract")
        catalog_skills_raw = raw.get("catalog_skills")
        if not isinstance(catalog_skills_raw, list) or not catalog_skills_raw:
            raise ValueError(f"task {task_id} has no catalog skills")
        catalog_skills = tuple(validate_identifier(value, "catalog skill") for value in catalog_skills_raw)
        if len(set(catalog_skills)) != len(catalog_skills):
            raise ValueError(f"task {task_id} repeats a catalog skill")
        expected_skill = expected_action.split("::", 1)[0]
        if expected_skill not in catalog_skills:
            raise ValueError(f"task {task_id} catalog omits its expected skill")
        binding = raw.get("profile_binding")
        if not isinstance(binding, dict):
            raise ValueError(f"task {task_id} profile binding is not a mapping")
        exact_keys(binding, {"source", "argument", "value"}, f"task {task_id} profile binding")
        if binding.get("source") not in KNOWN_PROFILE_BINDINGS:
            raise ValueError(f"task {task_id} has an unknown profile binding source")
        if binding.get("source") == "argument" and (not binding.get("argument") or "value" not in binding):
            raise ValueError(f"task {task_id} argument binding is incomplete")
        for field in ("goal", "success_criteria", "state"):
            if not isinstance(raw.get(field), str) or not raw[field].strip() or len(raw[field]) > 4_096:
                raise ValueError(f"task {task_id} has invalid {field}")
        tasks.append(
            Task(
                id=task_id,
                goal=raw["goal"].strip(),
                success_criteria=raw["success_criteria"].strip(),
                state=raw["state"].strip(),
                expected_action_id=expected_action,
                expected_tool=action_tool_name(expected_action),
                catalog_skills=catalog_skills,
                profile_binding=dict(binding),
                argument_expectations=validate_expectations(raw.get("argument_expectations"), f"task {task_id} arguments"),
                approval_argument_expectations=validate_expectations(raw.get("approval_argument_expectations"), f"task {task_id} approval arguments"),
                auth_strategies=tuple(offline_task.get("auth_strategies") or []),
                auth_requirement=str(offline_task.get("auth_requirement")),
                profile_policy=str(offline_task.get("profile_policy")),
                approval_class=str(offline_task.get("approval_class")),
                runtime_owner=str(offline_task.get("runtime_owner")),
            )
        )
    if seen != set(offline_tasks):
        raise ValueError("Phase 0E2 corpus does not exactly cover the Phase 0E1 tasks")
    tasks.sort(key=lambda item: item.id)
    return manifest, tasks


def frontmatter_description(path: Path) -> str:
    source = read_utf8(path, "skill markdown")
    lines = source.lstrip("\ufeff").splitlines()
    if not lines or lines[0].strip() != "---":
        return ""
    end = next((index for index, line in enumerate(lines[1:257], 1) if line.strip() == "---"), None)
    if end is None:
        return ""
    value = bounded_yaml("\n".join(lines[1:end]), "skill frontmatter") or {}
    return str(value.get("description") or "") if isinstance(value, dict) else ""


def canonical_mapping(value: Any) -> Any:
    if isinstance(value, dict):
        return {str(key): canonical_mapping(value[key]) for key in sorted(value, key=lambda item: str(item))}
    if isinstance(value, list):
        return [canonical_mapping(item) for item in value]
    return value


def compact_json(value: Any) -> bytes:
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"), allow_nan=False).encode("utf-8")


def schema_for_action(action: dict[str, Any]) -> dict[str, Any]:
    parameters = action.get("parameters") or []
    required = action.get("required") or []
    overrides = action.get("parameter_overrides") or {}
    if not isinstance(parameters, list) or not isinstance(required, list) or not isinstance(overrides, dict):
        raise ValueError("tool action has malformed parameter declarations")
    properties: dict[str, Any] = {}
    for name in parameters:
        validate_identifier(name, "parameter")
        if name in properties:
            raise ValueError(f"duplicate tool parameter: {name}")
        override = overrides.get(name, {"type": "string"})
        if not isinstance(override, dict):
            raise ValueError(f"parameter override for {name} is not a mapping")
        properties[name] = canonical_mapping(override)
    if len(set(required)) != len(required) or any(name not in properties for name in required):
        raise ValueError("tool action has invalid required parameters")
    return canonical_mapping(
        {
            "type": "object",
            "properties": properties,
            "required": list(required),
            "additionalProperties": False,
        }
    )


def build_catalog(skill_root: Path, baseline: dict[str, Any]) -> tuple[dict[str, dict[str, Any]], int, str]:
    if skill_root.is_symlink() or not skill_root.is_dir():
        raise ValueError("skill root is not a regular non-symlink directory")
    expected_skills = [item["skill_id"] for item in baseline.get("catalog", {}).get("skills") or []]
    tools: dict[str, dict[str, Any]] = {}
    entries: list[tuple[str, str, dict[str, Any], str]] = []
    for skill_id in expected_skills:
        validate_identifier(skill_id, "skill id")
        directory = skill_root / skill_id
        schema_path = directory / "tool_schema.yaml"
        if directory.is_symlink() or not directory.is_dir():
            raise ValueError(f"skill directory is not regular: {directory}")
        raw = load_yaml(schema_path, f"schema for {skill_id}")
        if not isinstance(raw, dict) or raw.get("name") != skill_id:
            raise ValueError(f"skill schema name mismatch: {skill_id}")
        actions = raw.get("native_action_schemas")
        if not isinstance(actions, dict):
            raise ValueError(f"skill has no native action mapping: {skill_id}")
        pack_description = str(raw.get("description") or "")
        if not pack_description:
            pack_description = frontmatter_description(directory / "SKILL.md")
        for action_name, action in actions.items():
            validate_identifier(action_name, "action name")
            if not isinstance(action, dict):
                raise ValueError(f"action {skill_id}::{action_name} is not a mapping")
            name = f"{skill_id}__{action_name}"
            parameters = schema_for_action(action)
            description = str(action.get("description") or pack_description)
            tool = {
                "type": "function",
                "name": name,
                "description": description,
                "parameters": parameters,
            }
            if name in tools:
                raise ValueError(f"duplicate provider tool name: {name}")
            tools[name] = tool
            entries.append((name, description, parameters, skill_id))
    entries.sort(key=lambda item: item[0])
    digest = hashlib.sha256()
    schema_bytes = 0
    for name, description, parameters, _skill in entries:
        parameter_bytes = compact_json(parameters)
        schema_bytes += len(name.encode()) + len(description.encode()) + len(parameter_bytes)
        entry = {
            "name": name,
            "description": description,
            "parameters": parameters,
        }
        encoded = compact_json(entry)
        digest.update(struct.pack(">Q", len(encoded)))
        digest.update(encoded)
    expected_catalog = baseline.get("catalog") or {}
    digest_hex = digest.hexdigest()
    if schema_bytes != expected_catalog.get("schema_bytes") or digest_hex != expected_catalog.get("digest"):
        raise ValueError(
            "live evaluator catalog differs from the checked Phase 0E1 baseline "
            f"(bytes={schema_bytes}, digest={digest_hex})"
        )
    return tools, schema_bytes, digest_hex


def core_tools() -> list[dict[str, Any]]:
    return [
        {
            "type": "function",
            "name": "tool_search",
            "description": "Search or select a deferred provider tool only when the required loaded leaf is absent.",
            "parameters": {
                "type": "object",
                "properties": {"query": {"type": "string"}},
                "required": ["query"],
                "additionalProperties": False,
            },
        },
        {
            "type": "function",
            "name": "yield",
            "description": "Return a terminal outcome only when no concrete work action remains.",
            "parameters": {
                "type": "object",
                "properties": {"summary": {"type": "string"}},
                "required": ["summary"],
                "additionalProperties": False,
            },
        },
        {
            "type": "function",
            "name": "need_user_input",
            "description": "Request input only when the task cannot proceed without the user.",
            "parameters": {
                "type": "object",
                "properties": {"question": {"type": "string"}, "input_type": {"type": "string"}},
                "required": ["question", "input_type"],
                "additionalProperties": False,
            },
        },
    ]


def task_catalog(task: Task, all_tools: dict[str, dict[str, Any]], max_tools: int) -> list[dict[str, Any]]:
    prefixes = tuple(f"{skill}__" for skill in task.catalog_skills)
    selected = [tool for name, tool in sorted(all_tools.items()) if name.startswith(prefixes)]
    selected.extend(core_tools())
    names = [tool["name"] for tool in selected]
    if task.expected_tool not in names:
        raise ValueError(f"task {task.id} catalog omits expected tool")
    if len(names) != len(set(names)) or len(names) > max_tools:
        raise ValueError(f"task {task.id} catalog exceeds the provider tool bound")
    return selected


def current_runtime_instruction(root: Path) -> str:
    source = read_utf8(
        root / "magician/src/magician_v2/execution/agentic/native_integration.rs",
        "native tool instruction",
    )
    match = re.search(
        r'pub const NATIVE_TOOL_INSTRUCTION: &str = "(.*?)";\s*pub const CHAT_NATIVE_TOOL_INSTRUCTION',
        source,
        re.DOTALL,
    )
    if not match:
        raise ValueError("cannot extract the production native tool instruction")
    encoded = re.sub(r"\\\n\s*", "", match.group(1))
    return json.loads('"' + encoded.replace('"', '\\"') + '"')


def render_prompt(path: Path, values: dict[str, str]) -> str:
    raw = load_json(path, "agentic prompt")
    rendered = "\n".join(raw["content"])
    variables = raw.get("variables") or []
    if variables and isinstance(variables[0], str):
        names, defaults = variables, {}
    else:
        names = [item["name"] for item in variables]
        defaults = {
            item["name"]: str(item.get("default_value") or "")
            for item in variables
            if isinstance(item, dict)
        }
    for name in names:
        rendered = rendered.replace("{" + name + "}", values.get(name, defaults.get(name, "")))
    leftovers = [name for name in names if "{" + name + "}" in rendered]
    if leftovers:
        raise ValueError(f"unrendered prompt variables: {leftovers}")
    return rendered


def build_payload(
    root: Path,
    profile: Any,
    manifest: dict[str, Any],
    task: Task,
    tools: list[dict[str, Any]],
    max_output_tokens: int,
) -> dict[str, Any]:
    surface = manifest["surface"]
    system = render_prompt(
        root / "data/magician_v2/prompts" / f"agentic_decision_system_v{surface['system_prompt_version']}.json",
        {"identity_section": "", "capabilities_section": ""},
    )
    capabilities = (
        "\n## LOADED CAPABILITY ROUTING\n"
        "The provider catalog contains one loaded target family plus authorized cross-family "
        "distractors. Tool schemas are authoritative. This is a decision-only evaluation: "
        "emit the concrete native tool call, but never claim it executed or that runtime "
        "authentication, authorization, or approval has completed.\n"
    )
    user = render_prompt(
        root / "data/magician_v2/prompts" / f"agentic_decision_v{surface['decision_prompt_version']}.json",
        {
            "goal": task.goal,
            "success_criteria": task.success_criteria,
            "state_type": "Phase 0E2 non-executing provider decision fixture",
            "state_description": task.state,
            "history_summary": "No action has been executed. Select the first concrete action now.",
            "capabilities_section": capabilities,
            "task_context": "",
        },
    )
    payload: dict[str, Any] = {
        "model": profile.model,
        "instructions": system + current_runtime_instruction(root),
        "input": [{"role": "user", "content": [{"type": "input_text", "text": user}]}],
        "tools": tools,
        "tool_choice": "required",
        "max_output_tokens": max_output_tokens,
        "stream": True,
    }
    if profile.reasoning_effort:
        payload["reasoning"] = {
            "effort": profile.reasoning_effort,
            "summary": profile.reasoning_summary or "auto",
        }
    elif profile.model.startswith("gpt-5.") and not profile.model.startswith("gpt-5-pro"):
        payload["reasoning"] = {"effort": "none"}
    if profile.verbosity:
        payload["text"] = {"verbosity": profile.verbosity}
    return payload


def get_path(value: Any, path: str) -> tuple[bool, Any]:
    current = value
    for component in path.split("."):
        if not isinstance(current, dict) or component not in current:
            return False, None
        current = current[component]
    return True, current


def is_json_subset(expected: Any, actual: Any) -> bool:
    if isinstance(expected, dict):
        return isinstance(actual, dict) and all(
            key in actual and is_json_subset(value, actual[key]) for key, value in expected.items()
        )
    if isinstance(expected, list):
        return isinstance(actual, list) and len(expected) == len(actual) and all(
            is_json_subset(left, right) for left, right in zip(expected, actual)
        )
    return expected == actual


def json_within_budget(value: Any, max_depth: int = 64, max_nodes: int = 20_000) -> bool:
    pending = [(value, 0)]
    nodes = 0
    while pending:
        current, depth = pending.pop()
        nodes += 1
        if nodes > max_nodes or depth > max_depth:
            return False
        if isinstance(current, dict):
            pending.extend((key, depth + 1) for key in current)
            pending.extend((item, depth + 1) for item in current.values())
        elif isinstance(current, list):
            pending.extend((item, depth + 1) for item in current)
        elif isinstance(current, str) and len(current) > MAX_INPUT_BYTES:
            return False
    return True


def expectation_passes(arguments: dict[str, Any], expectation: dict[str, Any]) -> bool:
    present, actual = get_path(arguments, expectation["path"])
    if not present:
        return False
    operator = expectation["operator"]
    expected = expectation.get("value")
    if operator == "equals":
        if (
            isinstance(actual, (int, float))
            and not isinstance(actual, bool)
            and isinstance(expected, (int, float))
            and not isinstance(expected, bool)
        ):
            return actual == expected
        return actual == expected and type(actual) is type(expected)
    if operator == "contains":
        return isinstance(actual, str) and str(expected).casefold() in actual.casefold()
    if operator == "contains_terms":
        if not isinstance(actual, str) or not isinstance(expected, list):
            return False
        actual_terms = {
            term.casefold() for term in re.findall(r"\w+", actual, re.UNICODE)
        }
        return all(term.casefold() in actual_terms for term in expected)
    if operator == "list_equals":
        return isinstance(actual, list) and actual == expected
    if operator == "json_subset":
        if not isinstance(actual, str):
            return False
        try:
            decoded = json.loads(actual)
        except (json.JSONDecodeError, RecursionError):
            return False
        return json_within_budget(decoded) and is_json_subset(expected, decoded)
    return False


def profile_binding_passes(task: Task, arguments: dict[str, Any]) -> bool:
    source = task.profile_binding["source"]
    policy_sources = {
        "none": {"none"},
        "argument": {"selectable"},
        "runtime_bound": {"selectable"},
        "runtime_fixed": {"fixed"},
        "runtime_implicit": {"implicit"},
    }
    if task.profile_policy not in policy_sources[source]:
        return False
    if source != "argument":
        return True
    present, actual = get_path(arguments, str(task.profile_binding["argument"]))
    return present and actual == task.profile_binding.get("value")


def validate_call_schema(call: Any, tools_by_name: dict[str, dict[str, Any]]) -> bool:
    tool = tools_by_name.get(call.name)
    if (
        tool is None
        or not isinstance(call.arguments, dict)
        or not json_within_budget(call.arguments)
    ):
        return False
    try:
        return not list(Draft202012Validator(tool["parameters"]).iter_errors(call.arguments))
    except RecursionError:
        return False


def score_result(
    helpers: Any,
    task: Task,
    profile: Any,
    run_index: int,
    tools: list[dict[str, Any]],
    status_code: int,
    response: dict[str, Any] | None,
    total_ms: int,
    first_output_ms: int | None,
    tool_decision_ms: int | None,
    pricing: dict[str, Any] | None,
    error: str | None,
) -> Result:
    response = response or {}
    try:
        calls = helpers.parse_tool_calls(response)
    except (RecursionError, ValueError, TypeError):
        calls = []
        error = error or "provider tool arguments exceeded the safe JSON shape"
    names = [call.name for call in calls]
    tools_by_name = {tool["name"]: tool for tool in tools}
    catalog_confined = bool(calls) and all(name in tools_by_name for name in names)
    exact_first = bool(calls) and calls[0].name == task.expected_tool
    exact_selection = len(calls) == 1 and exact_first
    expected_arguments = calls[0].arguments if exact_first else {}
    arguments_pass = exact_first and all(
        expectation_passes(expected_arguments, expectation)
        for expectation in task.argument_expectations
    )
    schema_pass = bool(calls) and all(validate_call_schema(call, tools_by_name) for call in calls)
    auth_profile_pass = exact_first and profile_binding_passes(task, expected_arguments)
    approval_pass = exact_first and all(
        expectation_passes(expected_arguments, expectation)
        for expectation in task.approval_argument_expectations
    )
    response_status = response.get("status")
    http_success = status_code == 200 and error is None and response_status == "completed"
    decision_success = all(
        (
            http_success,
            catalog_confined,
            exact_selection,
            arguments_pass,
            schema_pass,
            auth_profile_pass,
            approval_pass,
        )
    )
    usage = response.get("usage") or {}
    input_details = usage.get("input_tokens_details") or {}
    output_details = usage.get("output_tokens_details") or {}
    input_tokens = usage.get("input_tokens")
    cached_tokens = input_details.get("cached_tokens")
    output_tokens = usage.get("output_tokens")
    reasoning_tokens = output_details.get("reasoning_tokens")
    schema_bytes = sum(
        len(tool["name"].encode())
        + len(tool.get("description", "").encode())
        + len(compact_json(tool["parameters"]))
        for tool in tools
    )
    return Result(
        task=task.id,
        run_index=run_index,
        profile=profile.name,
        model=profile.model,
        status_code=status_code,
        response_status=response_status,
        response_id=response.get("id"),
        total_ms=total_ms,
        first_output_ms=first_output_ms,
        tool_decision_ms=tool_decision_ms,
        retries=0,
        input_tokens=input_tokens,
        cached_tokens=cached_tokens,
        output_tokens=output_tokens,
        reasoning_tokens=reasoning_tokens,
        cost_usd=helpers.compute_cost(pricing, input_tokens, cached_tokens, output_tokens),
        catalog_tools=len(tools),
        catalog_schema_bytes=schema_bytes,
        auth_strategies=list(task.auth_strategies),
        auth_requirement=task.auth_requirement,
        profile_policy=task.profile_policy,
        profile_binding_source=str(task.profile_binding["source"]),
        approval_class=task.approval_class,
        runtime_owner=task.runtime_owner,
        selected_tools=names,
        tool_calls=[asdict(call) for call in calls],
        http_success=http_success,
        catalog_confined=catalog_confined,
        exact_first_action_pass=exact_first,
        exact_action_selection_pass=exact_selection,
        argument_semantics_pass=arguments_pass,
        schema_validity_pass=schema_pass,
        auth_profile_correctness_pass=auth_profile_pass,
        approval_correctness_pass=approval_pass,
        decision_success=decision_success,
        error=error,
    )


def rate(results: list[Result], field: str) -> float:
    return sum(bool(getattr(item, field)) for item in results) / len(results) if results else 0.0


def numeric(results: list[Result], field: str) -> list[float]:
    return [float(getattr(item, field)) for item in results if getattr(item, field) is not None]


def percentile(values: list[float], fraction: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    index = min(len(ordered) - 1, max(0, int((len(ordered) - 1) * fraction + 0.999999)))
    return ordered[index]


def summarize(results: list[Result], tasks: list[Task]) -> dict[str, Any]:
    per_task: dict[str, Any] = {}
    for task in tasks:
        subset = [item for item in results if item.task == task.id]
        per_task[task.id] = {
            "calls": len(subset),
            "successes": sum(item.decision_success for item in subset),
            "exact_first_action_rate": rate(subset, "exact_first_action_pass"),
            "decision_success_rate": rate(subset, "decision_success"),
            "mean_total_ms": statistics.fmean(numeric(subset, "total_ms")) if subset else None,
            "total_cost_usd": sum(item.cost_usd or 0.0 for item in subset),
        }
    total_ms = numeric(results, "total_ms")
    decision_ms = numeric(results, "tool_decision_ms")
    return {
        "calls": len(results),
        "tasks": len({item.task for item in results}),
        "http_success_rate": rate(results, "http_success"),
        "catalog_confinement_rate": rate(results, "catalog_confined"),
        "exact_first_action_rate": rate(results, "exact_first_action_pass"),
        "exact_action_selection_rate": rate(results, "exact_action_selection_pass"),
        "argument_semantics_rate": rate(results, "argument_semantics_pass"),
        "schema_validity_rate": rate(results, "schema_validity_pass"),
        "auth_profile_correctness_rate": rate(results, "auth_profile_correctness_pass"),
        "approval_correctness_rate": rate(results, "approval_correctness_pass"),
        "decision_success_rate": rate(results, "decision_success"),
        "provider_usage_complete": all(
            item.input_tokens is not None and item.output_tokens is not None for item in results
        ) if results else False,
        "priced_cost_complete": all(item.cost_usd is not None for item in results) if results else False,
        "input_tokens": sum(item.input_tokens or 0 for item in results),
        "cached_tokens": sum(item.cached_tokens or 0 for item in results),
        "output_tokens": sum(item.output_tokens or 0 for item in results),
        "reasoning_tokens": sum(item.reasoning_tokens or 0 for item in results),
        "total_cost_usd": sum(item.cost_usd or 0.0 for item in results),
        "mean_total_ms": statistics.fmean(total_ms) if total_ms else None,
        "median_total_ms": statistics.median(total_ms) if total_ms else None,
        "p90_total_ms": percentile(total_ms, 0.90),
        "mean_tool_decision_ms": statistics.fmean(decision_ms) if decision_ms else None,
        "median_tool_decision_ms": statistics.median(decision_ms) if decision_ms else None,
        "p90_tool_decision_ms": percentile(decision_ms, 0.90),
        "retries": sum(item.retries for item in results),
        "per_task": per_task,
    }


def gate_failures(summary: dict[str, Any], manifest: dict[str, Any], tasks: list[Task], runs: int) -> list[str]:
    gates = manifest["gates"]
    failures: list[str] = []
    minimum_runs = int(gates["minimum_runs_per_task"])
    if runs < minimum_runs:
        failures.append(f"runs={runs} below decisive minimum {minimum_runs}")
    expected_calls = len(tasks) * runs
    if summary.get("calls") != expected_calls or summary.get("tasks") != len(tasks):
        failures.append("result corpus is incomplete")
    rate_gates = (
        ("http_success_rate", "http_success_rate"),
        ("catalog_confinement_rate", "catalog_confinement_rate"),
        ("schema_validity_rate", "schema_validity_rate"),
        ("auth_profile_correctness_rate", "auth_profile_correctness_rate"),
        ("approval_correctness_rate", "approval_correctness_rate"),
        ("exact_first_action_rate", "exact_first_action_rate"),
        ("decision_success_rate", "decision_success_rate"),
    )
    for summary_key, gate_key in rate_gates:
        actual = float(summary.get(summary_key, 0.0))
        minimum = float(gates[gate_key])
        if actual < minimum:
            failures.append(f"{summary_key}={actual:.1%} below {minimum:.1%}")
    if gates.get("require_provider_usage") and not summary.get("provider_usage_complete"):
        failures.append("provider token usage is incomplete")
    if gates.get("require_priced_cost") and not summary.get("priced_cost_complete"):
        failures.append("provider cost is incomplete")
    per_task_minimum = int(gates["minimum_successes_per_task"])
    for task in tasks:
        item = summary.get("per_task", {}).get(task.id) or {}
        if int(item.get("successes", 0)) < per_task_minimum:
            failures.append(
                f"task {task.id} successes={item.get('successes', 0)} below {per_task_minimum}"
            )
    return failures


def render_markdown(report: dict[str, Any]) -> str:
    summary = report["summary"]
    lines = [
        "# Tool Runtime Phase 0E2 Live-Agentic Baseline",
        "",
        "This report records provider decisions only. No selected tool was executed, no credential was resolved, and no external side effect occurred.",
        "",
        "## Summary",
        "",
        f"- Profile: `{report['profile']['name']}`",
        f"- Model: `{report['profile']['model']}`",
        f"- Runs per task: {report['runs']}",
        f"- Calls: {summary['calls']}",
        f"- Exact first-action rate: {summary['exact_first_action_rate']:.1%}",
        f"- Schema validity rate: {summary['schema_validity_rate']:.1%}",
        f"- Auth/profile correctness rate: {summary['auth_profile_correctness_rate']:.1%}",
        f"- Approval correctness rate: {summary['approval_correctness_rate']:.1%}",
        f"- Decision success rate: {summary['decision_success_rate']:.1%}",
        f"- Input/cache/output tokens: {summary['input_tokens']} / {summary['cached_tokens']} / {summary['output_tokens']}",
        f"- Cost: ${summary['total_cost_usd']:.6f}",
        f"- Median / p90 total latency: {summary['median_total_ms']:.0f} / {summary['p90_total_ms']:.0f} ms",
        f"- Evidence gate: `{'PASS' if report['evidence_gate_passed'] else 'FAIL'}`",
        "",
        "## Per task",
        "",
        "| Task | Calls | Successes | First action | Decision success | Cost |",
        "|---|---:|---:|---:|---:|---:|",
    ]
    for task, item in summary["per_task"].items():
        lines.append(
            f"| `{task}` | {item['calls']} | {item['successes']} | {item['exact_first_action_rate']:.1%} | {item['decision_success_rate']:.1%} | ${item['total_cost_usd']:.6f} |"
        )
    lines.extend(["", "## Gate failures", ""])
    if report["gate_failures"]:
        lines.extend(f"- {failure}" for failure in report["gate_failures"])
    else:
        lines.append("- None.")
    lines.extend(
        [
            "",
            "## Boundary",
            "",
            "This baseline qualifies the current typed catalog's first provider decision. It is not tool-execution, end-to-end task-completion, authentication, MCP conformance, or production-migration evidence. Those remain later-phase gates.",
            "",
        ]
    )
    return "\n".join(lines)


def render_html(report: dict[str, Any]) -> str:
    summary = report["summary"]
    rows = []
    for item in report["results"]:
        cost = "" if item["cost_usd"] is None else f"${item['cost_usd']:.6f}"
        rows.append(
            "<tr>"
            f"<td>{html.escape(item['task'])}</td><td>{item['run_index']}</td>"
            f"<td>{html.escape(', '.join(item['selected_tools']) or '<none>')}</td>"
            f"<td>{'PASS' if item['decision_success'] else 'FAIL'}</td>"
            f"<td>{item['total_ms']}</td><td>{item['input_tokens']}</td>"
            f"<td>{cost}</td>"
            f"<td>{html.escape(item['error'] or '')}</td></tr>"
        )
    failures = "".join(f"<li>{html.escape(value)}</li>" for value in report["gate_failures"]) or "<li>None</li>"
    return f"""<!doctype html><html><head><meta charset=\"utf-8\"><title>Phase 0E2 live-agentic baseline</title>
<style>body{{font:14px system-ui;margin:32px;max-width:1400px}}table{{border-collapse:collapse;width:100%}}th,td{{border:1px solid #ccc;padding:6px;text-align:left}}code{{background:#eee;padding:2px 4px}}</style></head><body>
<h1>Tool Runtime Phase 0E2 Live-Agentic Baseline</h1>
<p>Decision-only: selected tools were recorded and never executed.</p>
<p><b>Gate:</b> {'PASS' if report['evidence_gate_passed'] else 'FAIL'} · <b>Calls:</b> {summary['calls']} · <b>Success:</b> {summary['decision_success_rate']:.1%} · <b>Cost:</b> ${summary['total_cost_usd']:.6f}</p>
<h2>Gate failures</h2><ul>{failures}</ul>
<h2>Calls</h2><table><thead><tr><th>Task</th><th>Run</th><th>Selected</th><th>Decision</th><th>Total ms</th><th>Input tokens</th><th>Cost</th><th>Error</th></tr></thead><tbody>{''.join(rows)}</tbody></table>
</body></html>"""


def atomic_write(path: Path, value: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.is_symlink():
        raise ValueError(f"refusing to replace symlinked output: {path}")
    temporary = path.with_name(f".{path.name}.tmp.{os.getpid()}")
    try:
        with temporary.open("x", encoding="utf-8") as handle:
            handle.write(value)
            handle.flush()
            os.fsync(handle.fileno())
        temporary.replace(path)
    finally:
        if temporary.exists():
            temporary.unlink()


def write_report(output: Path, report: dict[str, Any]) -> None:
    atomic_write(output / "report.json", json.dumps(report, indent=2, ensure_ascii=False) + "\n")
    atomic_write(output / "report.md", render_markdown(report))
    atomic_write(output / "report.html", render_html(report))
    jsonl = "".join(json.dumps(item, ensure_ascii=False) + "\n" for item in report["results"])
    atomic_write(output / "calls.jsonl", jsonl)


def default_config(root: Path) -> Path:
    override = os.environ.get("MAGICIAN_CONFIG_PATH")
    candidates = [
        Path(override).expanduser() if override else None,
        Path.home() / "MagicianNotes/magician-config.yaml",
        root / "magician-config.yaml",
    ]
    return next(path for path in candidates if path is not None and path.is_file())


def absolute_without_resolving(path: Path) -> Path:
    expanded = path.expanduser()
    return expanded if expanded.is_absolute() else Path.cwd() / expanded


def self_test(root: Path, helpers: Any, manifest_path: Path, baseline_path: Path, skill_root: Path) -> None:
    manifest, tasks = load_contract(manifest_path, baseline_path)
    baseline = load_json(baseline_path, "Phase 0E1 baseline")
    all_tools, schema_bytes, digest = build_catalog(skill_root, baseline)
    assert schema_bytes == 268_761
    assert digest == manifest["offline_baseline"]["catalog_digest"]
    task = next(item for item in tasks if item.id == "public-paper-research")
    tools = task_catalog(task, all_tools, int(manifest["surface"]["max_tools_per_request"]))
    profile = helpers.Profile("test", "openai", "gpt-5.6-terra", "OPENAI_API_KEY", 10, 2048, None, None, None, None)
    response = {
        "id": "resp_test",
        "status": "completed",
        "output": [{"type": "function_call", "name": task.expected_tool, "arguments": '{"query":"graph neural networks for molecular property prediction","limit":4,"category":"cs.LG"}'}],
        "usage": {"input_tokens": 100, "input_tokens_details": {"cached_tokens": 20}, "output_tokens": 10, "output_tokens_details": {"reasoning_tokens": 0}},
    }
    result = score_result(helpers, task, profile, 1, tools, 200, response, 80, 20, 40, {"input_per_m": 1.0, "cache_read_per_m": 0.1, "output_per_m": 2.0}, None)
    assert result.decision_success and result.cost_usd is not None
    wrong = score_result(helpers, task, profile, 1, tools, 200, {**response, "output": [{"type": "function_call", "name": "yield", "arguments": '{"summary":"done"}'}]}, 80, 20, 40, {"input_per_m": 1.0, "cache_read_per_m": 0.1, "output_per_m": 2.0}, None)
    assert not wrong.decision_success and not wrong.exact_first_action_pass
    with tempfile.TemporaryDirectory(prefix="phase0-agentic-self-test-") as directory:
        repeated = [Result(**{**asdict(result), "task": item.id, "run_index": run}) for item in tasks for run in range(1, 6)]
        summary = summarize(repeated, tasks)
        report = {"profile": {"name": "test", "model": "test"}, "runs": 5, "summary": summary, "gate_failures": [], "evidence_gate_passed": True, "results": [asdict(item) for item in repeated]}
        write_report(Path(directory), report)
        assert (Path(directory) / "report.html").is_file()
    print("tool runtime Phase 0E2 live evaluator self-test passed")


def parse_args(root: Path) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", type=Path, default=default_config(root))
    parser.add_argument("--profile")
    parser.add_argument("--manifest", type=Path, default=root / "data/tool-runtime-inventory/phase0-live-agentic-v1.yaml")
    parser.add_argument("--baseline", type=Path, default=root / "data/tool-runtime-inventory/phase0-offline-baseline-v1.json")
    parser.add_argument("--skill-root", type=Path, default=root / "skillshub")
    parser.add_argument("--scenario", action="append")
    parser.add_argument("--runs", type=int, default=5)
    parser.add_argument("--workers", type=int, default=2)
    parser.add_argument("--max-output-tokens", type=int)
    parser.add_argument("--timeout-secs", type=int)
    parser.add_argument("--pricing-file", type=Path)
    parser.add_argument("--env-file", type=Path, action="append")
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument("--publish-json", type=Path)
    parser.add_argument("--publish-report", type=Path)
    parser.add_argument("--render-report", type=Path)
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--no-gate", action="store_true")
    return parser.parse_args()


def main() -> int:
    root = Path(__file__).resolve().parent.parent
    helpers = load_module(
        root / "scripts/eval-agentic-decision-rationale-live.py",
        "tool_runtime_phase0_live_helpers",
    )
    args = parse_args(root)
    manifest_path = absolute_without_resolving(args.manifest)
    baseline_path = absolute_without_resolving(args.baseline)
    skill_root = absolute_without_resolving(args.skill_root)
    if args.self_test:
        self_test(root, helpers, manifest_path, baseline_path, skill_root)
        return 0
    try:
        manifest, all_tasks = load_contract(manifest_path, baseline_path)
        baseline = load_json(baseline_path, "Phase 0E1 baseline")
        all_tools, catalog_schema_bytes, catalog_digest = build_catalog(skill_root, baseline)
        profile = helpers.load_profile(args.config.expanduser(), args.profile)
    except Exception as error:
        print(f"Phase 0E2 configuration error: {error}", file=sys.stderr)
        return 2
    if profile.provider.lower() != "openai":
        print(f"Phase 0E2 requires an OpenAI Responses profile; got {profile.provider}", file=sys.stderr)
        return 2
    unknown = set(args.scenario or []) - {task.id for task in all_tasks}
    if unknown:
        print(f"unknown scenarios: {sorted(unknown)}", file=sys.stderr)
        return 2
    tasks = [task for task in all_tasks if not args.scenario or task.id in args.scenario]
    if args.runs < 1 or args.workers < 1:
        print("--runs and --workers must be positive", file=sys.stderr)
        return 2
    max_tools = int(manifest["surface"]["max_tools_per_request"])
    catalogs = {task.id: task_catalog(task, all_tools, max_tools) for task in tasks}
    effective_max = min(
        profile.configured_max_output_tokens,
        args.max_output_tokens or int(manifest["surface"]["max_output_tokens"]),
    )
    jobs = [(run, task) for run in range(1, args.runs + 1) for task in tasks]
    print(
        f"Phase 0E2 live eval: profile={profile.name} model={profile.model} "
        f"tasks={len(tasks)} runs={args.runs} calls={len(jobs)} workers={args.workers}"
    )
    if args.dry_run:
        details = []
        for task in tasks:
            payload = build_payload(root, profile, manifest, task, catalogs[task.id], effective_max)
            details.append(
                {
                    "task": task.id,
                    "expected_tool": task.expected_tool,
                    "tools": len(catalogs[task.id]),
                    "schema_bytes": sum(
                        len(tool["name"].encode())
                        + len(tool.get("description", "").encode())
                        + len(compact_json(tool["parameters"]))
                        for tool in catalogs[task.id]
                    ),
                    "payload_bytes": len(compact_json(payload)),
                }
            )
        print(
            json.dumps(
                {
                    "config": str(args.config),
                    "profile": asdict(profile),
                    "catalog_schema_bytes": catalog_schema_bytes,
                    "catalog_digest": catalog_digest,
                    "projected_calls": len(jobs),
                    "decision_only": True,
                    "tasks": details,
                },
                indent=2,
            )
        )
        return 0
    if args.render_report:
        try:
            report_path = absolute_without_resolving(args.render_report)
            report = load_json(report_path, "saved Phase 0E2 report")
            results = [Result(**item) for item in report.get("results") or []]
            recorded_runs = int(report.get("runs", 0))
            summary = summarize(results, all_tasks)
            failures = gate_failures(summary, manifest, all_tasks, recorded_runs)
            report["summary"] = summary
            report["gate_failures"] = failures
            report["evidence_gate_passed"] = not failures
            output = args.output_dir or report_path.parent
            write_report(output, report)
        except Exception as error:
            print(f"saved report render failed: {error}", file=sys.stderr)
            return 2
        return 0 if not failures or args.no_gate else 1

    for env_file in args.env_file or [
        Path.home() / "MagicianNotes/.env.development",
        Path.home() / "MagicianNotes/.env",
        root / ".env.development",
        root / ".env",
    ]:
        if env_file.is_file():
            helpers.load_dotenv(env_file)
    api_key = os.environ.get(profile.api_key_env)
    if not api_key:
        print(f"{profile.api_key_env} is not set", file=sys.stderr)
        return 2
    pricing_path = args.pricing_file or Path.home() / "MagicianNotes/llm_pricing.json"
    pricing = (
        helpers.select_pricing_row(pricing_path, profile.provider, profile.model)
        if pricing_path.is_file()
        else None
    )
    endpoint = helpers.responses_url(profile)
    timeout = args.timeout_secs or profile.timeout_secs

    def execute(job: tuple[int, Task]) -> Result:
        run_index, task = job
        tools = catalogs[task.id]
        payload = build_payload(root, profile, manifest, task, tools, effective_max)
        status, response, total_ms, first_ms, decision_ms, error = helpers.run_live_request(
            api_key, endpoint, payload, timeout
        )
        result = score_result(
            helpers,
            task,
            profile,
            run_index,
            tools,
            status,
            response,
            total_ms,
            first_ms,
            decision_ms,
            pricing,
            error,
        )
        selected = " -> ".join(result.selected_tools) or "<none>"
        print(
            f"  {task.id}/run-{run_index}: HTTP {status} tools={selected} "
            f"decision={decision_ms}ms {'PASS' if result.decision_success else 'FAIL'}",
            flush=True,
        )
        return result

    with ThreadPoolExecutor(max_workers=args.workers) as pool:
        results = list(pool.map(execute, jobs))
    summary = summarize(results, all_tasks)
    failures = gate_failures(summary, manifest, all_tasks, args.runs)
    stamp = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H%M%SZ")
    output = args.output_dir or root / "coverage/evals/tool-runtime-phase0-agentic" / f"live-{stamp}"
    report = {
        "schema_version": "tool-runtime.phase0-live-agentic-evidence.v1",
        "generated_at": stamp,
        "decision_only": True,
        "tool_execution_count": 0,
        "credential_resolution_count": 0,
        "external_side_effect_count": 0,
        "manifest": str(manifest_path),
        "offline_baseline": str(baseline_path),
        "catalog_digest": catalog_digest,
        "catalog_schema_bytes": catalog_schema_bytes,
        "profile": asdict(profile),
        "runs": args.runs,
        "workers": args.workers,
        "summary": summary,
        "gate_failures": failures,
        "evidence_gate_passed": not failures,
        "results": [asdict(item) for item in results],
    }
    write_report(output, report)
    print(f"JSON report: {output / 'report.json'}")
    print(f"HTML report: {output / 'report.html'}")
    decisive = not args.scenario and args.runs >= int(manifest["gates"]["minimum_runs_per_task"])
    if (args.publish_json or args.publish_report) and (failures or not decisive):
        print("refusing to publish incomplete or failing evidence", file=sys.stderr)
        return 1
    if args.publish_json:
        atomic_write(args.publish_json, json.dumps(report, indent=2, ensure_ascii=False) + "\n")
    if args.publish_report:
        atomic_write(args.publish_report, render_markdown(report))
    if failures:
        for failure in failures:
            print(f"GATE FAIL: {failure}", file=sys.stderr)
        return 0 if args.no_gate else 1
    print("Tool runtime Phase 0E2 live-agentic gate: PASS")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
