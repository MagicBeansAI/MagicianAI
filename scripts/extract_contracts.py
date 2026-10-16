#!/usr/bin/env python3
"""
Extract behavioral contracts from the Magician Rust codebase.

Produces `contracts.json` — a structured file capturing:
  - Validation rules (what each subsystem accepts/rejects)
  - Kind-gating (where code branches on enum variants)
  - Constants (limits, whitelists, approved values)
  - Type shapes (struct fields with types for key interfaces)

YAML / JSON config files are intentionally NOT parsed — even
project-marker JSON (`package.json`, `manifest.json`) and TOML
(`Cargo.toml`, `pyproject.toml`) only have their `name` and
`version` metadata fields read. Arbitrary YAML content is excluded
to avoid leaking secrets, hostnames, default credentials, and other
config-bearing values into the committed graph artifact.

This fills the gap between the structural codegraph (graph.json) and
the actual source code.  The codegraph tells you WHAT exists and HOW
things connect; contracts.json tells you WHAT each thing requires,
rejects, and produces.

Usage:
    python3 scripts/extract_contracts.py
    python3 scripts/extract_contracts.py --out docs/codegraph/contracts.json
"""

import argparse
import json
import os
import re
import sys
from dataclasses import dataclass, field, asdict
from pathlib import Path
from typing import Optional

WORKSPACE_ROOT = Path(__file__).resolve().parent.parent


# ── Extraction primitives ────────────────────────────────────────────


def read_file(path: Path) -> str:
    return path.read_text(encoding="utf-8", errors="replace")


def relative_path(path: Path) -> str:
    try:
        return str(path.relative_to(WORKSPACE_ROOT))
    except ValueError:
        return str(path)


# ── 1. Constants extraction ──────────────────────────────────────────

# Matches: const NAME: TYPE = VALUE;
# Captures multi-token values including arrays and string slices.
RE_CONST = re.compile(
    r"(?:pub\s+)?const\s+([A-Z_][A-Z_0-9]*)\s*:\s*(.+?)\s*=\s*(.+?)\s*;",
    re.DOTALL,
)

# Matches: &["item1", "item2", ...]
RE_STR_ARRAY = re.compile(r'&\[\s*("(?:[^"\\]|\\.)*"(?:\s*,\s*"(?:[^"\\]|\\.)*")*)\s*\]')
RE_STR_ITEM = re.compile(r'"((?:[^"\\]|\\.)*)"')


def extract_constants(source: str, file_path: Path) -> list[dict]:
    """Extract const declarations with their values."""
    results = []
    for m in RE_CONST.finditer(source):
        name, typ, raw_value = m.group(1), m.group(2).strip(), m.group(3).strip()
        # Skip very long or uninteresting constants
        if len(raw_value) > 500:
            continue

        value = raw_value
        # Try to parse string array constants into a list
        arr_match = RE_STR_ARRAY.search(raw_value)
        if arr_match:
            value = RE_STR_ITEM.findall(arr_match.group(0))

        # Try to parse numeric constants
        if isinstance(value, str):
            try:
                value = int(value.replace("_", ""))
            except ValueError:
                pass

        line = source[: m.start()].count("\n") + 1
        results.append(
            {
                "name": name,
                "type": typ,
                "value": value,
                "file": relative_path(file_path),
                "line": line,
            }
        )
    return results


# ── 2. Validation rules extraction ───────────────────────────────────

# Matches: errors.push("..." or errors.push(format!("...
RE_VALIDATION_ERROR = re.compile(
    r'errors\.push\(\s*(?:'
    r'"([^"]+)"'                          # string literal
    r'|format!\(\s*"([^"]+)"'             # format! macro
    r")\s*\.to_string\(\)\s*\)",
)

# Matches: return Err(SomeError::Variant(...)) or Err(SomeError { ... })
RE_RETURN_ERR = re.compile(
    r"return\s+Err\(\s*(\w+Error::(\w+))\s*[\({]"
)


def extract_validation_rules(source: str, file_path: Path) -> list[dict]:
    """Extract validation error messages from validation functions."""
    results = []
    for m in RE_VALIDATION_ERROR.finditer(source):
        msg = m.group(1) or m.group(2)
        line = source[: m.start()].count("\n") + 1
        results.append(
            {
                "message": msg,
                "file": relative_path(file_path),
                "line": line,
            }
        )
    return results


def extract_error_variants(source: str, file_path: Path) -> list[dict]:
    """Extract error enum variants with their #[error("...")] messages."""
    results = []
    # Match #[error("message")] followed by VariantName
    for m in re.finditer(
        r'#\[error\("([^"]+)"\)\]\s*(\w+)',
        source,
    ):
        msg, variant = m.group(1), m.group(2)
        line = source[: m.start()].count("\n") + 1
        results.append(
            {
                "variant": variant,
                "message": msg,
                "file": relative_path(file_path),
                "line": line,
            }
        )
    return results


# ── 3. Kind-gating extraction ────────────────────────────────────────

# Matches: AgentKind::Worker => { ... } or if self.kind == AgentKind::Personal
RE_KIND_GATE = re.compile(
    r"(AgentKind::(\w+))\s*(?:=>|==|!=)",
)


def extract_kind_gates(source: str, file_path: Path) -> list[dict]:
    """Find where code branches on AgentKind variants."""
    results = []
    seen_lines = set()
    for m in RE_KIND_GATE.finditer(source):
        line = source[: m.start()].count("\n") + 1
        if line in seen_lines:
            continue
        seen_lines.add(line)

        # Get surrounding context (the line and a few after it)
        lines = source.splitlines()
        start = max(0, line - 1)
        end = min(len(lines), line + 5)
        context = "\n".join(lines[start:end]).strip()

        results.append(
            {
                "kind": m.group(2),
                "expression": m.group(0),
                "file": relative_path(file_path),
                "line": line,
                "context": context,
            }
        )
    return results


# ── 4. Struct shape extraction ────────────────────────────────────────

RE_STRUCT_START = re.compile(
    r"(?:#\[derive\([^\)]*\)\]\s*)*"
    r"(?:#\[serde\([^\)]*\)\]\s*)*"
    r"pub\s+struct\s+(\w+)\s*\{",
)

RE_FIELD = re.compile(
    r"(?:#\[serde\(([^\)]*)\)\]\s*)?"
    r"pub\s+(\w+)\s*:\s*(.+?)\s*,"
)


def extract_struct_shapes(source: str, file_path: Path, target_structs: set[str]) -> list[dict]:
    """Extract struct fields for specific target structs."""
    results = []
    for m in RE_STRUCT_START.finditer(source):
        name = m.group(1)
        if name not in target_structs:
            continue

        # Find matching closing brace
        start = m.end()
        depth = 1
        pos = start
        while pos < len(source) and depth > 0:
            if source[pos] == "{":
                depth += 1
            elif source[pos] == "}":
                depth -= 1
            pos += 1
        body = source[start : pos - 1]

        fields = []
        for fm in RE_FIELD.finditer(body):
            serde_attrs = fm.group(1)
            field_name = fm.group(2)
            field_type = fm.group(3).strip()
            field_info = {"name": field_name, "type": field_type}
            if serde_attrs:
                if "default" in serde_attrs:
                    field_info["has_default"] = True
                if "skip_serializing_if" in serde_attrs:
                    field_info["optional_skip"] = True
                if "deny_unknown_fields" in serde_attrs:
                    field_info["strict"] = True
            fields.append(field_info)

        line = source[: m.start()].count("\n") + 1
        results.append(
            {
                "name": name,
                "fields": fields,
                "file": relative_path(file_path),
                "line": line,
            }
        )
    return results


# ── 5. Enum variant extraction ────────────────────────────────────────

RE_ENUM_START = re.compile(
    r"(?:#\[derive\([^\)]*\)\]\s*)*"
    r"(?:#\[serde\([^\)]*\)\]\s*)*"
    r"(?:pub\s+)?enum\s+(\w+)\s*\{",
)

RE_VARIANT = re.compile(r"^\s*(\w+)(?:\s*\{|\s*\(|\s*,|\s*$)", re.MULTILINE)


def extract_enum_variants(source: str, file_path: Path, target_enums: set[str]) -> list[dict]:
    """Extract enum variants for specific target enums."""
    results = []
    for m in RE_ENUM_START.finditer(source):
        name = m.group(1)
        if name not in target_enums:
            continue

        start = m.end()
        depth = 1
        pos = start
        while pos < len(source) and depth > 0:
            if source[pos] == "{":
                depth += 1
            elif source[pos] == "}":
                depth -= 1
            pos += 1
        body = source[start : pos - 1]

        variants = []
        for vm in RE_VARIANT.finditer(body):
            v = vm.group(1)
            # Skip common noise
            if v in ("pub", "fn", "let", "use", "type", "self", "impl", "where"):
                continue
            variants.append(v)

        line = source[: m.start()].count("\n") + 1
        results.append(
            {
                "name": name,
                "variants": variants,
                "file": relative_path(file_path),
                "line": line,
            }
        )
    return results


# ── 6. Default injection extraction ──────────────────────────────────

RE_EARLY_RETURN = re.compile(
    r"if\s+self\.kind\s*!=\s*AgentKind::(\w+)\s*\{\s*return\s*;?\s*\}"
)


def extract_default_guards(source: str, file_path: Path) -> list[dict]:
    """Find early-return guards based on agent kind."""
    results = []
    for m in RE_EARLY_RETURN.finditer(source):
        line = source[: m.start()].count("\n") + 1
        # Find enclosing function name
        before = source[: m.start()]
        fn_match = list(re.finditer(r"(?:pub\s+)?fn\s+(\w+)", before))
        fn_name = fn_match[-1].group(1) if fn_match else "<unknown>"

        results.append(
            {
                "function": fn_name,
                "guard_kind": m.group(1),
                "effect": f"Only AgentKind::{m.group(1)} agents pass through; others return early",
                "file": relative_path(file_path),
                "line": line,
            }
        )
    return results


# YAML value extraction was removed deliberately — even when opt-in,
# parsing arbitrary YAML to lift `param.default`, `description`,
# `enum_values`, etc. into the committed `contracts.json` is a
# secrets-leakage risk (config files routinely carry tokens, keys,
# example endpoint URLs, internal hostnames). The engine intentionally
# extracts NO YAML data anywhere. Project markers (`Cargo.toml`,
# `pyproject.toml`) only have their `name` / `version` metadata read.
# If consumers need to inspect a YAML's contents, they should read
# the file directly rather than reach for the graph.


# ── 8. Match-arm extraction for specific enums ───────────────────────

RE_MATCH_ARM = re.compile(
    r"(\w+)::(\w+)\s*=>\s*\{",
)


def extract_match_arms(source: str, file_path: Path, target_enum: str) -> list[dict]:
    """Extract match arms for a specific enum and capture the body."""
    results = []
    for m in RE_MATCH_ARM.finditer(source):
        if m.group(1) != target_enum:
            continue
        variant = m.group(2)
        line = source[: m.start()].count("\n") + 1

        # Capture the arm body (find matching brace)
        start = m.end()
        depth = 1
        pos = start
        while pos < len(source) and depth > 0:
            if source[pos] == "{":
                depth += 1
            elif source[pos] == "}":
                depth -= 1
            pos += 1
        body = source[start : pos - 1].strip()

        # Extract key behaviors from the body
        behaviors = []
        for err_m in re.finditer(r'errors\.push\(\s*"([^"]+)"', body):
            behaviors.append({"type": "rejects", "rule": err_m.group(1)})
        for err_m in re.finditer(r'errors\.push\(\s*format!\(\s*"([^"]+)"', body):
            behaviors.append({"type": "rejects", "rule": err_m.group(1)})
        if "return Err" in body:
            behaviors.append({"type": "returns_error"})
        if "return Ok" in body or "return;" in body:
            behaviors.append({"type": "allows"})

        results.append(
            {
                "enum": target_enum,
                "variant": variant,
                "behaviors": behaviors,
                "file": relative_path(file_path),
                "line": line,
            }
        )
    return results


# ── 9. Approved/whitelisted value extraction ─────────────────────────

RE_APPROVED_LIST = re.compile(
    r"(?:APPROVED|ALLOWED|SUPPORTED|VALID)_(\w+)\s*:\s*&\[&str\]\s*=\s*&\[([^\]]+)\]",
)


def extract_whitelists(source: str, file_path: Path) -> list[dict]:
    results = []
    for m in RE_APPROVED_LIST.finditer(source):
        name = m.group(1)
        raw_values = m.group(2)
        values = RE_STR_ITEM.findall(raw_values)
        line = source[: m.start()].count("\n") + 1
        results.append(
            {
                "name": f"APPROVED_{name}" if not name.startswith("APPROVED") else name,
                "values": values,
                "file": relative_path(file_path),
                "line": line,
            }
        )
    return results


# ── Main extraction pipeline ─────────────────────────────────────────

# Key structs whose shapes we want to capture
TARGET_STRUCTS = {
    "AgentDefinition",
    "AgentConstraints",
    "AutonomousConfig",
    "FocusArea",
    "MemoryTierDefinition",
    "MemoryConsolidationRule",
    "SurfacePublishParams",
    "SurfaceSpec",
    "SurfaceManifest",
    "SurfacePublishResult",
    "MuijDocument",
    "MuijComponent",
    "ArtifactMetadata",
    "OwnershipScope",
    "ProducerInfo",
    "PolicyBindings",
    "AnalyticsEventSink",
    "DuckDbPool",
    "QueryRequest",
    "QueryResponse",
    "Task",
    "CreateTaskParams",
    "TaskSchedule",
    "EpisodeRecord",
    "TierData",
}

# Key enums whose variants we want to capture
TARGET_ENUMS = {
    "AgentKind",
    "TaskStatus",
    "LifecycleState",
    "ProjectionSurface",
    "ConsolidationTrigger",
    "MergeStrategy",
    "TierScope",
    "RetentionMode",
    "SurfaceCompileError",
    "SurfacePublishError",
    "ExposureClass",
    "PhysicalLocator",
    "SurfaceSectionSpec",
    "BuiltinTransform",
}

# Key enums we want match-arm analysis for
MATCH_ARM_ENUMS = {"AgentKind", "SurfaceSectionSpec"}


def scan_rust_files(src_dir: Path | None = None) -> list[Path]:
    """Return every `.rs` file across the repo, walking each Cargo
    crate's full directory tree (not just `src/`) so contracts can be
    extracted from integration tests, examples, benches, and build
    scripts in addition to the canonical source.

    When `src_dir` is given (CLI `--src` override), scan only that
    directory — useful for narrowing extraction to one crate during
    development."""
    if src_dir is not None:
        return sorted(p for p in _walk_pruned(src_dir) if p.suffix == ".rs")
    files: list[Path] = []
    seen_crate_roots: set[Path] = set()
    for manifest in _walk_manifests(WORKSPACE_ROOT, "Cargo.toml"):
        crate_root = manifest.parent.resolve()
        if crate_root in seen_crate_roots:
            continue
        seen_crate_roots.add(crate_root)
        files.extend(p for p in _walk_pruned(crate_root) if p.suffix == ".rs")
    return sorted(files)


# ── TypeScript contract extraction ──────────────────────────────────

# Exclude list (dirs pruned during walk, files skipped at emission).
# Sourced from `scripts/codegraph_exclude.txt` so the list is shared
# with `generate_code_graph.py` and editable without touching code.
EXCLUDE_FILE: Path = Path(__file__).resolve().parent / "codegraph_exclude.txt"


def _load_exclude(path: Path) -> tuple[frozenset[str], frozenset[str]]:
    """Parse a `[dirs]` / `[files]` sectioned exclude file. One entry
    per line, `#` comments stripped, blank lines ignored. Returns
    `(dir_names, file_names)` — both matched by basename, not path."""
    section: str | None = None
    buckets: dict[str, set[str]] = {"dirs": set(), "files": set()}
    for raw_line in path.read_text(encoding="utf-8").splitlines():
        line = raw_line.split("#", 1)[0].strip()
        if not line:
            continue
        if line.startswith("[") and line.endswith("]"):
            section = line[1:-1].strip().lower()
            if section not in buckets:
                raise ValueError(
                    f"{path}: unknown section [{section}] (expected [dirs] or [files])"
                )
            continue
        if section is None:
            raise ValueError(f"{path}: entry {line!r} appears before any section header")
        buckets[section].add(line)
    return frozenset(buckets["dirs"]), frozenset(buckets["files"])


SCAN_EXCLUDE_DIRS, SCAN_EXCLUDE_FILES = _load_exclude(EXCLUDE_FILE)


def _walk_manifests(root: Path, filename: str) -> list[Path]:
    """Stack-based walk that prunes `SCAN_EXCLUDE_DIRS` (and dotdirs)
    before descending, returning every file matching `filename`."""
    hits: list[Path] = []
    stack: list[Path] = [root]
    while stack:
        cur = stack.pop()
        try:
            entries = sorted(cur.iterdir())
        except (PermissionError, OSError):
            continue
        for entry in entries:
            if entry.is_dir():
                if entry.name in SCAN_EXCLUDE_DIRS or entry.name.startswith("."):
                    continue
                stack.append(entry)
            elif entry.name == filename:
                hits.append(entry)
    return sorted(hits)


def _walk_pruned(root: Path):
    """Yield every file under `root`, pruning `SCAN_EXCLUDE_DIRS` and
    leading-dot dirs during descent, and skipping basenames in
    `SCAN_EXCLUDE_FILES`. Mirrors `generate_code_graph.py::_walk_pruned`
    so both scripts agree on what counts as repo source."""
    stack: list[Path] = [root]
    while stack:
        cur = stack.pop()
        try:
            entries = sorted(cur.iterdir())
        except (PermissionError, OSError):
            continue
        for entry in entries:
            try:
                if entry.is_dir():
                    if entry.name in SCAN_EXCLUDE_DIRS or entry.name.startswith("."):
                        continue
                    stack.append(entry)
                elif entry.is_file():
                    if entry.name in SCAN_EXCLUDE_FILES:
                        continue
                    yield entry
            except OSError:
                continue

TS_INTERFACE_START_RE = re.compile(
    r"(?m)^\s*(?:export\s+)?interface\s+([A-Za-z_]\w*)\s*(?:extends\s+[^{]+)?\{"
)
TS_TYPE_ALIAS_RE = re.compile(
    r"(?m)^\s*(?:export\s+)?type\s+([A-Za-z_]\w*)\s*=\s*(.+?);"
)
TS_FIELD_RE = re.compile(
    r"^\s*(?:readonly\s+)?([A-Za-z_]\w*)\??:\s*(.+?);\s*$", re.MULTILINE
)


def scan_ts_files() -> list[tuple[Path, str]]:
    """Scan every Node project for `.ts` files (excluding `.d.ts`
    declarations). Projects are auto-discovered via `package.json`;
    each one's entire tree is walked (not just `src/`) so config
    files, root-level helpers, etc. contribute contracts too. The
    pruned walker keeps `node_modules/`, `dist/`, `.svelte-kit/` and
    friends out."""
    results: list[tuple[Path, str]] = []
    seen_proj_roots: set[Path] = set()
    for manifest in _walk_manifests(WORKSPACE_ROOT, "package.json"):
        proj_root = manifest.parent.resolve()
        if proj_root in seen_proj_roots:
            continue
        seen_proj_roots.add(proj_root)
        for ts_file in sorted(p for p in _walk_pruned(proj_root) if p.suffix == ".ts"):
            if ts_file.name.endswith(".d.ts"):
                continue
            try:
                content = ts_file.read_text(encoding="utf-8", errors="replace")
                results.append((ts_file, content))
            except Exception:
                continue
    return results


def extract_ts_interfaces(source: str, file_path: Path) -> list[dict]:
    """Extract TypeScript interface definitions with their fields."""
    interfaces: list[dict] = []
    rel = relative_path(file_path)

    for match in TS_INTERFACE_START_RE.finditer(source):
        name = match.group(1)
        line = source[: match.start()].count("\n") + 1
        # Find matching closing brace
        brace_start = match.end() - 1  # the opening {
        depth = 1
        pos = brace_start + 1
        while pos < len(source) and depth > 0:
            if source[pos] == "{":
                depth += 1
            elif source[pos] == "}":
                depth -= 1
            pos += 1
        body = source[brace_start + 1 : pos - 1]

        fields = []
        for field_match in TS_FIELD_RE.finditer(body):
            field_name = field_match.group(1)
            field_type = field_match.group(2).strip()
            fields.append({"name": field_name, "type": field_type})

        if fields:
            interfaces.append({
                "name": name,
                "language": "typescript",
                "fields": fields,
                "file": rel,
                "line": line,
            })

    return interfaces


def extract_ts_type_aliases(source: str, file_path: Path) -> list[dict]:
    """Extract TypeScript type alias definitions."""
    aliases: list[dict] = []
    rel = relative_path(file_path)

    for match in TS_TYPE_ALIAS_RE.finditer(source):
        name = match.group(1)
        definition = match.group(2).strip()
        line = source[: match.start()].count("\n") + 1
        aliases.append({
            "name": name,
            "language": "typescript",
            "definition": definition,
            "file": rel,
            "line": line,
        })

    return aliases


# ── Python contract extraction ──────────────────────────────────────

PY_ENV_PARAM_RE = re.compile(
    r"""os\.environ\.get\(\s*['"](_TOOL_\w+)['"]\s*(?:,\s*['"]([^'"]*)['"]\s*)?\)"""
)
PY_ENV_KEY_RE = re.compile(
    r"""os\.environ\.get\(\s*['"]([A-Z_]+_(?:KEY|TOKEN|SECRET|URL|ID))['"]\s*"""
)
PY_CONST_RE = re.compile(
    r"(?m)^([A-Z_][A-Z_0-9]{2,})\s*=\s*(.+?)$"
)


def scan_python_files() -> list[tuple[Path, str]]:
    """Scan every Python project for `.py` files. Projects are
    auto-discovered by walking for `pyproject.toml` / `setup.py`
    manifests; the whole project tree is walked (no `src/`
    privileging). `test_*` files are skipped — contract extraction
    targets production sources, not test fixtures."""
    results: list[tuple[Path, str]] = []
    py_roots: set[Path] = set()
    for marker in ("pyproject.toml", "setup.py"):
        for manifest in _walk_manifests(WORKSPACE_ROOT, marker):
            py_roots.add(manifest.parent.resolve())
    for py_root in sorted(py_roots):
        for py_file in sorted(p for p in _walk_pruned(py_root) if p.suffix == ".py"):
            if py_file.name.startswith("test_"):
                continue
            try:
                content = py_file.read_text(encoding="utf-8", errors="replace")
                results.append((py_file, content))
            except Exception:
                continue
    return results


def extract_python_tool_params(source: str, file_path: Path) -> list[dict]:
    """Extract _TOOL_* environment variable parameters from Python tools."""
    params: list[dict] = []
    rel = relative_path(file_path)
    tool_name = file_path.stem

    seen: set[str] = set()
    for match in PY_ENV_PARAM_RE.finditer(source):
        env_name = match.group(1)
        default = match.group(2) if match.group(2) is not None else ""
        if env_name in seen:
            continue
        seen.add(env_name)
        # Strip _TOOL_ prefix for parameter name
        param_name = env_name.replace("_TOOL_", "", 1)
        line = source[: match.start()].count("\n") + 1
        params.append({
            "tool": tool_name,
            "param": param_name,
            "env_var": env_name,
            "default": default,
            "file": rel,
            "line": line,
        })

    return params


def extract_python_credentials(source: str, file_path: Path) -> list[dict]:
    """Extract API key / credential env vars from Python tools."""
    creds: list[dict] = []
    rel = relative_path(file_path)
    tool_name = file_path.stem

    seen: set[str] = set()
    for match in PY_ENV_KEY_RE.finditer(source):
        env_name = match.group(1)
        if env_name in seen:
            continue
        seen.add(env_name)
        line = source[: match.start()].count("\n") + 1
        creds.append({
            "tool": tool_name,
            "env_var": env_name,
            "file": rel,
            "line": line,
        })

    return creds


def extract_python_constants(source: str, file_path: Path) -> list[dict]:
    """Extract top-level UPPER_CASE constants from Python files."""
    constants: list[dict] = []
    rel = relative_path(file_path)

    for match in PY_CONST_RE.finditer(source):
        name = match.group(1)
        value = match.group(2).strip()
        # Skip if it looks like a regex variable or import
        if name.startswith("RE_") or "import" in value:
            continue
        line = source[: match.start()].count("\n") + 1
        constants.append({
            "name": name,
            "value": value[:200],  # truncate long values
            "file": rel,
            "line": line,
        })

    return constants


def run_extraction(src_dir: Path | None) -> dict:
    contracts = {
        "version": "1.0.0",
        "description": (
            "Behavioral contracts extracted from the Magician codebase. "
            "Captures validation rules, kind-gating, constants, type shapes, "
            "and capability contracts. Complements graph.json (structure) with "
            "behavioral constraints and decision boundaries."
        ),
        "constants": [],
        "validation_rules": [],
        "error_types": [],
        "kind_gates": [],
        "default_guards": [],
        "struct_shapes": [],
        "enum_shapes": [],
        "match_arms": [],
        "whitelists": [],
        "ts_interfaces": [],
        "ts_type_aliases": [],
        "python_tool_params": [],
        "python_credentials": [],
        "python_constants": [],
    }

    rust_files = scan_rust_files(src_dir)
    scope_label = str(src_dir) if src_dir else "all auto-discovered Cargo crates"
    print(f"Scanning {len(rust_files)} Rust files ({scope_label})", file=sys.stderr)

    for path in rust_files:
        source = read_file(path)

        # Constants — only from files with meaningful constants
        consts = extract_constants(source, path)
        if consts:
            contracts["constants"].extend(consts)

        # Validation rules
        rules = extract_validation_rules(source, path)
        if rules:
            contracts["validation_rules"].extend(rules)

        # Error types
        errors = extract_error_variants(source, path)
        if errors:
            contracts["error_types"].extend(errors)

        # Kind gates
        gates = extract_kind_gates(source, path)
        if gates:
            contracts["kind_gates"].extend(gates)

        # Default injection guards
        guards = extract_default_guards(source, path)
        if guards:
            contracts["default_guards"].extend(guards)

        # Struct shapes
        shapes = extract_struct_shapes(source, path, TARGET_STRUCTS)
        if shapes:
            contracts["struct_shapes"].extend(shapes)

        # Enum shapes
        enum_shapes = extract_enum_variants(source, path, TARGET_ENUMS)
        if enum_shapes:
            contracts["enum_shapes"].extend(enum_shapes)

        # Match arms for key enums
        for enum_name in MATCH_ARM_ENUMS:
            arms = extract_match_arms(source, path, enum_name)
            if arms:
                contracts["match_arms"].extend(arms)

        # Whitelists
        wl = extract_whitelists(source, path)
        if wl:
            contracts["whitelists"].extend(wl)

    # ── TypeScript contracts ──────────────────────────────────────────
    ts_files = scan_ts_files()
    if ts_files:
        print(f"Scanning {len(ts_files)} TypeScript files for contracts", file=sys.stderr)
        for path, source in ts_files:
            ifaces = extract_ts_interfaces(source, path)
            if ifaces:
                contracts["ts_interfaces"].extend(ifaces)
            aliases = extract_ts_type_aliases(source, path)
            if aliases:
                contracts["ts_type_aliases"].extend(aliases)

    # ── Python contracts ────────────────────────────────────────────
    py_files = scan_python_files()
    if py_files:
        print(f"Scanning {len(py_files)} Python tool files for contracts", file=sys.stderr)
        for path, source in py_files:
            params = extract_python_tool_params(source, path)
            if params:
                contracts["python_tool_params"].extend(params)
            creds = extract_python_credentials(source, path)
            if creds:
                contracts["python_credentials"].extend(creds)
            pyconsts = extract_python_constants(source, path)
            if pyconsts:
                contracts["python_constants"].extend(pyconsts)

    # Filter constants to interesting ones (limits, counts, sizes, routes)
    interesting_const_patterns = re.compile(
        r"MAX_|MIN_|LIMIT_|DEFAULT_|APPROVED_|ALLOWED_|SUPPORTED_|"
        r"TIMEOUT|CAPACITY|BATCH|FLUSH|SURFACE_|BOOTSTRAP_|"
        r"EVENT_TYPE|NAMESPACE|VERSION"
    )
    contracts["constants"] = [
        c for c in contracts["constants"] if interesting_const_patterns.search(c["name"])
    ]

    # Summary stats
    contracts["stats"] = {
        "rust_files_scanned": len(rust_files),
        "ts_files_scanned": len(ts_files) if ts_files else 0,
        "python_files_scanned": len(py_files) if py_files else 0,
        "constants": len(contracts["constants"]),
        "validation_rules": len(contracts["validation_rules"]),
        "error_types": len(contracts["error_types"]),
        "kind_gates": len(contracts["kind_gates"]),
        "default_guards": len(contracts["default_guards"]),
        "struct_shapes": len(contracts["struct_shapes"]),
        "enum_shapes": len(contracts["enum_shapes"]),
        "match_arms": len(contracts["match_arms"]),
        "whitelists": len(contracts["whitelists"]),
        "ts_interfaces": len(contracts["ts_interfaces"]),
        "ts_type_aliases": len(contracts["ts_type_aliases"]),
        "python_tool_params": len(contracts["python_tool_params"]),
        "python_credentials": len(contracts["python_credentials"]),
        "python_constants": len(contracts["python_constants"]),
    }

    return contracts


def main():
    parser = argparse.ArgumentParser(description="Extract behavioral contracts from the codebase")
    parser.add_argument(
        "--out",
        default="docs/codegraph/contracts.json",
        help="Output path (default: docs/codegraph/contracts.json)",
    )
    parser.add_argument(
        "--src",
        default=None,
        help=(
            "Optional Rust source directory to scan. When omitted, "
            "every `Cargo.toml` in the repo is auto-discovered and "
            "its sibling `src/` indexed."
        ),
    )
    args = parser.parse_args()

    out_path = WORKSPACE_ROOT / args.out
    src_arg = Path(args.src) if args.src else None
    contracts = run_extraction(src_arg)

    out_path.parent.mkdir(parents=True, exist_ok=True)
    with open(out_path, "w") as f:
        json.dump(contracts, f, indent=2, default=str)

    print(f"\nContracts written to {out_path}", file=sys.stderr)
    print(f"Stats: {json.dumps(contracts['stats'], indent=2)}", file=sys.stderr)


if __name__ == "__main__":
    main()
