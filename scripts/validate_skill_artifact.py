#!/usr/bin/env python3
"""Validate scoped skill artifacts used by Skill Evolution proposals."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any

try:
    import yaml
except ImportError:
    print("ERROR: PyYAML required (pip install pyyaml or apt install python3-yaml)", file=sys.stderr)
    sys.exit(2)


def validate_tool_schema(path: Path) -> list[str]:
    errors: list[str] = []
    if not path.exists():
        return [f"{path}: file does not exist"]
    if not path.is_file():
        return [f"{path}: not a file"]

    try:
        document: Any = yaml.safe_load(path.read_text(encoding="utf-8"))
    except yaml.YAMLError as exc:
        return [f"{path}: YAML parse error: {exc}"]

    if not isinstance(document, dict):
        return [f"{path}: root must be a mapping"]

    name = document.get("name")
    if not isinstance(name, str) or not name.strip():
        errors.append(f"{path}: 'name' must be a non-empty string")

    description = document.get("description")
    if description is not None and not isinstance(description, str):
        errors.append(f"{path}: 'description' must be a string when present")

    parameters = document.get("parameters", [])
    if parameters is None:
        parameters = []
    if not isinstance(parameters, list):
        errors.append(f"{path}: 'parameters' must be a list when present")
    else:
        for index, parameter in enumerate(parameters):
            if not isinstance(parameter, dict):
                errors.append(f"{path}: parameters[{index}] must be a mapping")
                continue
            parameter_name = parameter.get("name")
            if not isinstance(parameter_name, str) or not parameter_name.strip():
                errors.append(f"{path}: parameters[{index}].name must be a non-empty string")
            parameter_description = parameter.get("description")
            if parameter_description is not None and not isinstance(parameter_description, str):
                errors.append(f"{path}: parameters[{index}].description must be a string when present")

    return errors


def sample_value_for_parameter(parameter: dict[str, Any]) -> Any:
    if "default" in parameter:
        return parameter["default"]
    enum_values = parameter.get("enum_values")
    if isinstance(enum_values, list) and enum_values:
        return enum_values[0]

    name = str(parameter.get("name") or "input").strip()
    normalized_name = name.lower().replace("-", "_")
    type_hint = str(parameter.get("param_type") or parameter.get("type") or "string").lower()
    if type_hint in {"integer", "int"}:
        return 1
    if type_hint in {"number", "float", "double"}:
        return 1.0
    if type_hint in {"boolean", "bool"}:
        return True
    if type_hint in {"array", "list"}:
        return ["sample"]
    if type_hint in {"object", "map"}:
        return {"sample": "value"}
    if "query" in normalized_name or "search" in normalized_name:
        return "skill evolution regression fixture"
    if normalized_name == "target" or normalized_name.endswith("_target") or "path" in normalized_name:
        return "."
    if "url" in normalized_name:
        return "https://example.invalid/skill-evolution-fixture"
    return f"sample_{normalized_name}"


def fixture_inputs_from_schema(document: dict[str, Any]) -> dict[str, Any]:
    inputs: dict[str, Any] = {}
    parameter_names: list[str] = []
    parameters = document.get("parameters") or []
    if not isinstance(parameters, list):
        return {"parameter_names": [], "sample_inputs": {}}

    for parameter in parameters[:16]:
        if not isinstance(parameter, dict):
            continue
        name = parameter.get("name")
        if not isinstance(name, str) or not name.strip():
            continue
        name = name.strip()
        parameter_names.append(name)
        required = parameter.get("required") is True or str(parameter.get("required")).lower() in {
            "true",
            "yes",
            "1",
        }
        if required or "default" in parameter:
            inputs[name] = sample_value_for_parameter(parameter)

    return {
        "input_source": "tool_schema_required_and_default_parameters",
        "parameter_names": parameter_names,
        "sample_inputs": inputs,
    }


def env_from_inputs(inputs: dict[str, Any]) -> dict[str, str]:
    env: dict[str, str] = {}
    for key, value in inputs.items():
        env_key = "_TOOL_" + "".join(ch.upper() if ch.isalnum() else "_" for ch in key)
        if isinstance(value, str):
            env[env_key] = value
        elif isinstance(value, (bool, int, float)):
            env[env_key] = str(value).lower() if isinstance(value, bool) else str(value)
        else:
            env[env_key] = json.dumps(value, sort_keys=True)
    return env


def build_wrapper_fixture(tool_schema: Path, wrapper: Path) -> tuple[dict[str, Any] | None, list[str]]:
    errors: list[str] = []
    if not tool_schema.exists() or not tool_schema.is_file():
        errors.append(f"{tool_schema}: tool schema file does not exist")
    if not wrapper.exists() or not wrapper.is_file():
        errors.append(f"{wrapper}: wrapper fixture target does not exist")
    if errors:
        return None, errors

    try:
        document = yaml.safe_load(tool_schema.read_text(encoding="utf-8"))
    except yaml.YAMLError as exc:
        return None, [f"{tool_schema}: YAML parse error: {exc}"]
    if not isinstance(document, dict):
        return None, [f"{tool_schema}: root must be a mapping"]

    fixture_inputs = fixture_inputs_from_schema(document)
    sample_inputs = fixture_inputs.get("sample_inputs") or {}
    if not isinstance(sample_inputs, dict):
        sample_inputs = {}
    fixture = {
        "tool_schema": str(tool_schema),
        "wrapper": str(wrapper),
        "tool_name": document.get("name"),
        "parameter_names": fixture_inputs.get("parameter_names") or [],
        "sample_inputs": sample_inputs,
        "env": env_from_inputs(sample_inputs),
        "execution_mode": "plan_only",
        "note": "Fixture validates schema-shaped sample inputs for review/meta-harness execution; it does not run the wrapper.",
    }
    return fixture, []


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tool-schema", action="append", default=[], help="tool_schema.yaml path to validate")
    parser.add_argument(
        "--wrapper-fixture",
        action="append",
        default=[],
        help="Wrapper script path to pair with --tool-schema for a generated regression fixture",
    )
    args = parser.parse_args()

    targets = [Path(value) for value in args.tool_schema]
    if not targets:
        parser.error("at least one --tool-schema path is required")

    errors: list[str] = []
    for target in targets:
        errors.extend(validate_tool_schema(target))

    fixtures: list[dict[str, Any]] = []
    if args.wrapper_fixture:
        if len(targets) != 1:
            errors.append("--wrapper-fixture requires exactly one --tool-schema path")
        else:
            for wrapper_value in args.wrapper_fixture:
                fixture, fixture_errors = build_wrapper_fixture(targets[0], Path(wrapper_value))
                errors.extend(fixture_errors)
                if fixture is not None:
                    fixtures.append(fixture)

    if errors:
        print("\n".join(errors), file=sys.stderr)
        print(f"\nFAIL: {len(errors)} validation error(s)", file=sys.stderr)
        return 1

    if fixtures:
        print(json.dumps({"fixtures": fixtures}, sort_keys=True))
    print(f"OK: {len(targets)} tool schema file(s) validated", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
