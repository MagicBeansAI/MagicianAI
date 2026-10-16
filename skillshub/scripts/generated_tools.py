#!/usr/bin/env python3
"""Record and verify the exact generated Metabase artifact used in a release."""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[1]


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def inputs(root: Path) -> dict:
    pin = json.loads((root / "metabase/generator-pin.json").read_text())
    actual_spec = digest(root / "metabase/scripts/spec.json")
    if pin["spec_sha256"] != actual_spec:
        raise ValueError("Metabase spec differs from generator-pin.json; review and update the pin before regenerating")
    recipe = (root / "Makefile").read_text().split("regen-metabase-cli: setup-printing-press\n", 1)[1].split("\nrefresh-metabase-spec:", 1)[0]
    return {**pin, "postgen_sha256": digest(root / "metabase/scripts/apply-postgen-patches.py"),
            "recipe_sha256": hashlib.sha256(recipe.encode()).hexdigest()}


def record(root: Path):
    expected = inputs(root)
    binary = root / "metabase/bin/metabase-pp-cli"
    version = subprocess.check_output([str(binary), "--version"], text=True, timeout=10).strip()
    if version != f"metabase-pp-cli {expected['cli_version']}":
        raise ValueError(f"unexpected generated CLI version: {version}")
    generator = Path.home() / "go/bin/printing-press"
    generator_version = subprocess.check_output([str(generator), "--version"], text=True, timeout=10).strip()
    if generator_version != f"printing-press {expected['generator_version'].lstrip('v')}":
        raise ValueError(f"unexpected generator version: {generator_version}")
    toolchain = Path.home() / "go/bin" / expected["go_version"]
    build_version = subprocess.check_output([str(toolchain), "version", "-m", str(binary)], text=True, timeout=10).splitlines()[0]
    if not build_version.endswith(": " + expected["go_version"]):
        raise ValueError(f"generated binary used another Go compiler: {build_version}")
    result = {"schema_version": 1, "inputs": expected, "version": version, "sha256": digest(binary),
              "go_sum_sha256": digest(root / "metabase/.pp-build/go.sum"),
              "generator_sha256": digest(generator), "compiler": expected["go_version"]}
    (root / "metabase/bin/generated.json").write_text(json.dumps(result, indent=2) + "\n")
    print(f"✓ pinned generated Metabase CLI: {result['sha256'][:16]}")


def verify(root: Path):
    result = json.loads((root / "metabase/bin/generated.json").read_text())
    if result["schema_version"] != 1 or result["inputs"] != inputs(root):
        raise ValueError("generated CLI inputs changed; run make -C skillshub regen-metabase-cli")
    if result["sha256"] != digest(root / "metabase/bin/metabase-pp-cli"):
        raise ValueError("generated Metabase CLI checksum differs from its build record")
    print(f"✓ generated Metabase CLI matches pinned inputs and SHA-256 {result['sha256'][:16]}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("inputs", "record", "verify"))
    parser.add_argument("--root", type=Path, default=ROOT)
    args = parser.parse_args()
    try:
        if args.action == "inputs":
            inputs(args.root)
        else:
            globals()[args.action](args.root)
    except (OSError, ValueError, KeyError, IndexError, subprocess.SubprocessError) as error:
        parser.exit(1, f"✗ {error}\n")


if __name__ == "__main__":
    main()
