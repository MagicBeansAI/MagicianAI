#!/usr/bin/env python3
"""Deterministic, offline Phase 5G conformance-closure guard."""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

from with_extracted_library import prepared_checkout


ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "data/tool-runtime-inventory/phase5g-conformance-v1.json"
PACKAGE_JSON = ROOT / "scripts/mcp-conformance/package.json"
PACKAGE_LOCK = ROOT / "scripts/mcp-conformance/package-lock.json"
MAKEFILE = ROOT / "Makefile"

RUNNER_PACKAGE = "@modelcontextprotocol/conformance"
RUNNER_VERSION = "0.2.0-alpha.10"
RUNNER_INTEGRITY = (
    "sha512-0V/HZDdWHcg6j0zVBzBsXcPZ571IVi6umKgTpnBhtTx/"
    "jm/LONmGF6cIWL2k4Xjyps0OiHV6B37nj2s0pUg0nQ=="
)
SDK_VERSION = "3.1.0"
SDK_COMMIT = "1f9358eddca42d3a510c70ae6446dd6548c7c856"
EXPECTED_GATES = {
    "official_protocol_auth_backcompat",
    "supported_transport_boundaries",
    "oauth_fake_provider",
    "discovery_continuation_and_subscription_adversarial",
    "official_sdk_ownership",
    "browser_profile_isolation",
    "native_permission_states",
    "delegated_grant_binding",
    "cross_strategy_adversarial_matrix",
}


def evidence_path(relative: str, roots: dict[str, Path]) -> Path:
    """Keep historical evidence labels while reading the consumed source pin.

    No network fallback, duplicate source tree, or skipped evidence is allowed.
    The Makefile prepares the checkout explicitly before this offline guard.
    """
    path = Path(relative)
    if path.is_absolute() or ".." in path.parts:
        raise ValueError("evidence path must stay within its source repository")
    if path.parts[:1] == ("tool-runtime-core",):
        if "magicrun" not in roots:
            roots["magicrun"] = prepared_checkout("magicrun")
        return roots["magicrun"] / path
    return ROOT / path


def load_json(path: Path) -> object:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ValueError(f"cannot read valid JSON from {path.relative_to(ROOT)}: {error}")


def require(condition: bool, message: str, errors: list[str]) -> None:
    if not condition:
        errors.append(message)


def main() -> int:
    errors: list[str] = []
    try:
        manifest = load_json(MANIFEST)
        package = load_json(PACKAGE_JSON)
        lock = load_json(PACKAGE_LOCK)
    except ValueError as error:
        return report([str(error)])
    require(isinstance(manifest, dict), "manifest root must be an object", errors)
    require(isinstance(package, dict), "package.json root must be an object", errors)
    require(isinstance(lock, dict), "package-lock.json root must be an object", errors)
    if errors:
        return report(errors)
    assert isinstance(manifest, dict)
    assert isinstance(package, dict)
    assert isinstance(lock, dict)

    require(
        manifest.get("schema_version") == "tool-runtime.phase5g-conformance.v1",
        "unexpected Phase 5G manifest schema",
        errors,
    )
    require(
        manifest.get("production_routing_enabled") is False,
        "Phase 5G must not enable production routing",
        errors,
    )
    component_versions = manifest.get("component_versions")
    require(
        component_versions
        == {
            "magician_mcp_client": "0.1.20",
            "tool_runtime_core": "0.1.59",
            "magician": "0.6.1146",
        },
        "Phase 5G component-version matrix drift",
        errors,
    )

    runner = manifest.get("official_runner")
    sdk = manifest.get("official_sdk")
    require(isinstance(runner, dict), "official_runner must be an object", errors)
    require(isinstance(sdk, dict), "official_sdk must be an object", errors)
    if isinstance(runner, dict):
        require(runner.get("package") == RUNNER_PACKAGE, "runner package drift", errors)
        require(runner.get("version") == RUNNER_VERSION, "runner version drift", errors)
        require(runner.get("integrity") == RUNNER_INTEGRITY, "runner integrity drift", errors)
        require(runner.get("expected_failures") == [], "expected failures are forbidden", errors)
    if isinstance(sdk, dict):
        require(sdk.get("crate") == "rmcp", "official SDK crate drift", errors)
        require(sdk.get("crate_version") == SDK_VERSION, "official SDK version drift", errors)
        require(sdk.get("git_commit") == SDK_COMMIT, "official SDK commit drift", errors)

    runs = manifest.get("official_client_runs")
    require(isinstance(runs, list) and len(runs) == 2, "exactly two official runs are required", errors)
    if isinstance(runs, list):
        run_keys = {
            (run.get("spec_version"), run.get("mode"), run.get("suite"), run.get("transport"))
            for run in runs
            if isinstance(run, dict)
        }
        require(
            run_keys
            == {
                ("2025-11-25", "client", "all", "streamable_http"),
                ("2026-07-28", "client", "all", "streamable_http"),
            },
            "official client run matrix drift",
            errors,
        )

    dependencies = package.get("dependencies")
    require(
        isinstance(dependencies, dict) and dependencies.get(RUNNER_PACKAGE) == RUNNER_VERSION,
        "package.json must exact-pin the official runner",
        errors,
    )
    packages = lock.get("packages")
    runner_lock = (
        packages.get(f"node_modules/{RUNNER_PACKAGE}") if isinstance(packages, dict) else None
    )
    require(isinstance(runner_lock, dict), "official runner is absent from package-lock.json", errors)
    if isinstance(runner_lock, dict):
        require(runner_lock.get("version") == RUNNER_VERSION, "locked runner version drift", errors)
        require(runner_lock.get("integrity") == RUNNER_INTEGRITY, "locked runner integrity drift", errors)

    gates = manifest.get("gates")
    require(isinstance(gates, list), "gates must be a list", errors)
    gate_ids: list[str] = []
    evidence_roots: dict[str, Path] = {}
    if isinstance(gates, list):
        for index, gate in enumerate(gates):
            if not isinstance(gate, dict):
                errors.append(f"gate {index} must be an object")
                continue
            gate_id = gate.get("id")
            if not isinstance(gate_id, str):
                errors.append(f"gate {index} has no string id")
                continue
            gate_ids.append(gate_id)
            evidence = gate.get("evidence")
            if not isinstance(evidence, list) or not evidence:
                errors.append(f"gate {gate_id} has no evidence")
                continue
            for item in evidence:
                if not isinstance(item, dict):
                    errors.append(f"gate {gate_id} has malformed evidence")
                    continue
                relative = item.get("path")
                symbols = item.get("symbols")
                if not isinstance(relative, str) or not isinstance(symbols, list) or not symbols:
                    errors.append(f"gate {gate_id} has incomplete evidence")
                    continue
                try:
                    path = evidence_path(relative, evidence_roots)
                    source = path.read_text(encoding="utf-8")
                except (OSError, ValueError, subprocess.CalledProcessError) as error:
                    errors.append(f"gate {gate_id} cannot read {relative}: {error}")
                    continue
                for symbol in symbols:
                    if not isinstance(symbol, str) or symbol not in source:
                        errors.append(f"gate {gate_id} is missing evidence {symbol!r} in {relative}")
    require(len(gate_ids) == len(set(gate_ids)), "gate ids must be unique", errors)
    require(set(gate_ids) == EXPECTED_GATES, "Phase 5G gate set is incomplete or has drifted", errors)

    makefile = MAKEFILE.read_text(encoding="utf-8")
    for target in (
        "check-phase5g-conformance:",
        "test-phase5g:",
        "test-mcp-official-conformance:",
    ):
        require(target in makefile, f"Makefile is missing {target}", errors)
    check_all_line = next(
        (line for line in makefile.splitlines() if line.startswith("check-all:")),
        "",
    )
    check_all_dependencies = set(check_all_line.partition(":")[2].split())
    require(
        {"check-service-name-boundary", "check-phase5g-conformance"}
        <= check_all_dependencies,
        "check-all must include the offline Phase 5G and service-name guards",
        errors,
    )
    require(
        'rmcp = { version = "=3.1.0", default-features = false }'
        in (ROOT / "Cargo.toml").read_text(encoding="utf-8"),
        "workspace rmcp exact pin drift",
        errors,
    )
    # Working-tree crate versions are deliberately not pinned.
    #
    # This guard freezes the qualified MCP surface: runner package / version /
    # integrity, rmcp version and commit, gate evidence symbols, and
    # production_routing_enabled. Those are the claims Phase 5G closed.
    #
    # Crate Cargo.toml versions were a currency check on top of that.
    # magician was dropped on 2026-08-13 because it bumped on every feature
    # commit. magician-mcp-client and tool-runtime-core stayed because they
    # "essentially never move". They do move, for reasons that are not MCP
    # re-qualification: Darwin memory ceilings (0.1.68 → 0.1.69) and the apps
    # catalog (0.1.69 → 0.1.71). Each miss aborted check-all before cargo
    # check, named "component version drift", and taught the same lesson as
    # magician: a check that fires only falsely gets routed around.
    #
    # The attested component_versions block in the JSON is a different list.
    # It records the versions the contract was established against and stays
    # put unless conformance is re-run. Do not re-add a working-tree
    # Cargo.toml currency check.

    if errors:
        return report(errors)
    print("Phase 5G conformance closure guard passed.")
    return 0


def report(errors: list[str]) -> int:
    for error in errors:
        print(f"FAIL: {error}", file=sys.stderr)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
