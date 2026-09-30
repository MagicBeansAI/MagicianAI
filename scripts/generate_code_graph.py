#!/usr/bin/env python3
"""Generate a static code graph for the workspace.

Extended scope:
- crate dependency graph from `cargo metadata`
- Rust source file/module graph
- symbol definitions (fn/struct/enum/trait/type/const/static)
- lightweight references via `use <crate>::...`
- Actix endpoint extraction (endpoint -> handler -> schema)
- outbound API call extraction (Rust reqwest-style + UI fetch calls)
- payload profile generation for endpoint mock payloads
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import re
import shutil
import subprocess
import sys
import time
from collections import defaultdict
from pathlib import Path
from typing import Any, Callable

SYMBOL_PATTERNS = [
    ("function", re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)\b")),
    ("struct", re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?struct\s+([A-Za-z_][A-Za-z0-9_]*)\b")),
    ("enum", re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?enum\s+([A-Za-z_][A-Za-z0-9_]*)\b")),
    ("trait", re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?trait\s+([A-Za-z_][A-Za-z0-9_]*)\b")),
    ("type_alias", re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?type\s+([A-Za-z_][A-Za-z0-9_]*)\b")),
    ("const", re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?const\s+([A-Za-z_][A-Za-z0-9_]*)\b")),
    ("static", re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?static\s+([A-Za-z_][A-Za-z0-9_]*)\b")),
]

USE_RE = re.compile(r"^\s*use\s+([A-Za-z_][A-Za-z0-9_]*)")
USE_STMT_RE = re.compile(r"(?m)^\s*(?:pub\s+)?use\s+([^;]+);")

SCOPE_RE = re.compile(r"web::scope\(\s*\"([^\"]*)\"\s*\)", re.DOTALL)
ROUTE_RE = re.compile(
    r"\.route\(\s*\"([^\"]*)\"\s*,\s*web::(get|post|put|delete|patch)\(\)\.to\(\s*([A-Za-z_][A-Za-z0-9_:]*)\s*\)\s*\)",
    re.DOTALL,
)
RESOURCE_ROUTE_RE = re.compile(
    r"web::resource\(\s*\"([^\"]*)\"\s*\)\s*\.route\(\s*web::(get|post|put|delete|patch)\(\)\.to\(\s*([A-Za-z_][A-Za-z0-9_:]*)\s*\)\s*\)",
    re.DOTALL,
)

RUST_FN_RE = re.compile(
    r"(?ms)^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)\s*\((.*?)\)\s*(?:->\s*[^\{]+)?(?:where\s+[^\{]+)?\{"
)

RUST_STRUCT_RE = re.compile(
    r"(?ms)^\s*(?:pub(?:\([^)]*\))?\s+)?struct\s+([A-Za-z_][A-Za-z0-9_]*)\s*\{(.*?)^\s*\}"
)
RUST_ENUM_RE = re.compile(
    r"(?ms)^\s*(?:pub(?:\([^)]*\))?\s+)?enum\s+([A-Za-z_][A-Za-z0-9_]*)\s*\{(.*?)^\s*\}"
)
RUST_STRUCT_FIELD_RE = re.compile(
    r"^\s*(?:pub(?:\([^)]*\))?\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*:\s*([^,]+),?\s*$"
)

RUST_EXTRACTOR_RE = {
    "json": re.compile(r"web::Json\s*<\s*([^>]+)\s*>") ,
    "query": re.compile(r"web::Query\s*<\s*([^>]+)\s*>") ,
    "path": re.compile(r"web::Path\s*<\s*([^>]+)\s*>") ,
}

RUST_API_CALL_RE = re.compile(
    r"\b([A-Za-z_][A-Za-z0-9_\.]*)\.(get|post|put|patch|delete)\(\s*([^\)]+)\)",
    re.MULTILINE,
)

TS_FUNCTION_RE = re.compile(r"(?m)^\s*(?:export\s+)?function\s+([A-Za-z_][A-Za-z0-9_]*)\s*\(")
TS_CONST_FN_RE = re.compile(
    r"(?m)^\s*(?:export\s+)?const\s+([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(?:async\s*)?\("
)
TS_FETCH_CALL_RE = re.compile(r"fetch\(\s*([\"'`])([^\"'`]+)\1", re.MULTILINE)
TS_METHOD_RE = re.compile(r"method\s*:\s*[\"']([A-Za-z]+)[\"']", re.IGNORECASE)
TS_REQUEST_JSON_RE = re.compile(
    r"""this\.requestJson\s*(?:<[^>]+>)?\(\s*(?:`([^`]+)`|([\"'])([^\"']+)\2)""",
    re.MULTILINE,
)
TS_CLASS_RE = re.compile(r"(?m)^\s*(?:export\s+)?class\s+([A-Za-z_][A-Za-z0-9_]*)")
TS_INTERFACE_RE = re.compile(r"(?m)^\s*(?:export\s+)?interface\s+([A-Za-z_][A-Za-z0-9_]*)")
TS_TEMPLATE_EXPR_RE = re.compile(r"\$\{[^}]+\}")
RUST_PLAIN_CALL_RE = re.compile(r"(?<![.:])\b([A-Za-z_][A-Za-z0-9_]*)\s*(?:::<[^>]+>)?\s*\(")
RUST_PATH_CALL_RE = re.compile(
    r"\b(?:self|super|crate|[a-z_][A-Za-z0-9_]*)(?:::[A-Za-z_][A-Za-z0-9_]*)*::([A-Za-z_][A-Za-z0-9_]*)\s*(?:::<[^>]+>)?\s*\("
)
# Captures both the qualifier path AND the leaf name for a Rust call,
# including paths whose leading segment starts uppercase (e.g.
# `String::new()`, `MyStruct::default()`) — those associated-fn calls
# are skipped by `RUST_PATH_CALL_RE` but are crucial for dead-code
# analysis. Match leading word-or-`::` chain → leaf-name + `(`.
RUST_QUALIFIED_CALL_RE = re.compile(
    r"(?<![.:])"
    r"((?:[A-Za-z_]\w*::)+)"
    r"([A-Za-z_]\w*)\s*(?:::<[^>]+>)?\s*\("
)
# Method-style call (`x.foo()`, `self.bar()`). Captures only the
# leaf method name — the receiver type isn't known without type
# inference. For dead-code purposes that's still useful: any
# function named `foo` defined in an `impl` block is marked as
# called, which is safer than marking it dead just because we can't
# resolve which `impl` it lives on. Edge over-emission is bounded
# by `resolve_call_targets` (file → crate → global-with-qualifier).
RUST_METHOD_CALL_RE = re.compile(r"\.([A-Za-z_]\w*)\s*(?:::<[^>]+>)?\s*\(")
# Same trick for TypeScript / JavaScript (`obj.method(`, `this.x(`)
# and Python (`obj.method(`). `TS_THIS_CALL_RE` already covered
# `this.x`; this widens to any receiver.
JS_METHOD_CALL_RE = re.compile(r"\.([A-Za-z_]\w*)\s*\(")
PY_METHOD_CALL_RE = re.compile(r"\.([A-Za-z_]\w*)\s*\(")
# Visibility for a Rust `fn`. Captures `pub`, `pub(crate)`, `pub(super)`,
# `pub(in path::to::mod)`, or empty for private. The leaf name follows.
RUST_FN_VISIBILITY_RE = re.compile(
    r"^\s*(pub(?:\([^)]*\))?)?\s*(?:async\s+)?(?:const\s+)?(?:unsafe\s+)?(?:extern\s+(?:\"[^\"]*\"\s+)?)?fn\s+([A-Za-z_]\w*)"
)
# Match the `impl [Trait for] Type` opener. Captures `(trait_name, type_name)`
# with `trait_name` being `None` for inherent impls.
RUST_IMPL_RE = re.compile(
    r"^\s*impl(?:\s*<[^>]*(?:where[^>]*)?>)?\s+"
    r"(?:([A-Za-z_][\w:]*(?:\s*<[^>]*>)?)\s+for\s+)?"
    r"([A-Za-z_][\w:]*(?:\s*<[^>]*>)?)"
)
# Trait declaration opener (so trait methods can later be matched).
RUST_TRAIT_DECL_RE = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?trait\s+([A-Za-z_]\w*)\b")
TS_PLAIN_CALL_RE = re.compile(r"(?<![.$])\b([A-Za-z_][A-Za-z0-9_]*)\s*\(")
TS_THIS_CALL_RE = re.compile(r"\b(?:this|self)\.([A-Za-z_][A-Za-z0-9_]*)\s*\(")
STRING_LITERAL_RE = re.compile(r"\"(?:\\.|[^\"\\])*\"|'(?:\\.|[^'\\])*'")

# Python patterns
PY_FUNCTION_RE = re.compile(r"(?m)^\s*(?:async\s+)?def\s+([A-Za-z_]\w*)\s*\(")
PY_CLASS_RE = re.compile(r"(?m)^\s*class\s+([A-Za-z_]\w*)")
PY_PLAIN_CALL_RE = re.compile(r"(?<![.\w])\b([A-Za-z_]\w*)\s*\(")
PY_CURL_CALL_RE = re.compile(
    r"""subprocess\.run\(\s*\[.*?['"]curl['"].*?['"]?(https?://[^'"}\s,\]]+)['"]?""",
    re.DOTALL,
)
PY_CURL_METHOD_RE = re.compile(r"""['"]-X['"]\s*,\s*['"](\w+)['"]""")
PY_REQUESTS_CALL_RE = re.compile(
    r"""(?:requests|urllib\.request)\.(get|post|put|patch|delete)\(\s*['"]([^'"]+)['"]""",
    re.MULTILINE | re.IGNORECASE,
)


def run(cmd: list[str], cwd: Path) -> str:
    proc = subprocess.run(cmd, cwd=str(cwd), capture_output=True, text=True)
    if proc.returncode != 0:
        raise RuntimeError(f"Command failed: {' '.join(cmd)}\\n{proc.stderr.strip()}")
    return proc.stdout


def relpath(path: Path, root: Path) -> str:
    return path.resolve().relative_to(root.resolve()).as_posix()


def derive_module_path(crate: str, file_rel_to_crate_root: Path) -> str:
    """Compute a Rust-style module path for a file inside a crate.
    Accepts the file's path relative to the crate root (not `src/`),
    so it's stable regardless of where the source actually lives.

    Conventions preserved:
        - leading `src/` is dropped (Rust idiom: crate root lives in
          `src/lib.rs` or `src/main.rs`, not `crate::src::lib`)
        - `lib.rs` / `main.rs` at crate root → `crate`
        - `mod.rs` collapses to its parent directory name
        - other dirs (`tests/`, `benches/`, `examples/`) DO appear in
          the path since they represent distinct compilation units
    """
    parts = list(file_rel_to_crate_root.parts)
    if parts and parts[0] == "src":
        parts = parts[1:]
    if not parts:
        return crate
    if parts[-1] in {"lib.rs", "main.rs"}:
        parts = parts[:-1]
        return f"{crate}::{'::'.join(parts)}" if parts else crate
    if parts[-1] == "mod.rs":
        parts = parts[:-1]
    else:
        parts[-1] = Path(parts[-1]).stem
    suffix = "::".join(parts)
    return f"{crate}::{suffix}" if suffix else crate


def derive_ui_module_path(crate: str, file_rel_to_project_root: Path) -> str:
    """Compute a module path for non-Rust source files. Strips a
    leading conventional source directory (`src` for Node/Python,
    `Sources` for SwiftPM) so module IDs match what users expect to
    type into `cgraph_search`, then joins the remaining path parts
    with `::` (file extension dropped on the leaf)."""
    parts = list(file_rel_to_project_root.parts)
    if parts and parts[0] in {"src", "Sources"}:
        parts = parts[1:]
    if not parts:
        return crate
    parts[-1] = Path(parts[-1]).stem
    suffix = "::".join(parts)
    return f"{crate}::{suffix}" if suffix else crate


# Swift patterns for SwiftPM and XcodeGen project scanning. Lightweight —
# we extract declarations (func / class / struct / enum / protocol / actor /
# extension / init) so cgraph_search can find them; we do NOT attempt full type
# resolution. Modifiers (`public`, `final`,
# `override`, `static`, `mutating`, `@MainActor`, etc.) are stripped
# from the leading run before the keyword.
SWIFT_FUNC_RE = re.compile(
    r"^\s*(?:@\w+(?:\([^)]*\))?\s+)*"
    r"(?:(?:public|private|internal|fileprivate|open|final|static|class|"
    r"override|mutating|nonisolated|isolated|convenience|required|"
    r"weak|unowned)\s+)*"
    r"func\s+([A-Za-z_][A-Za-z0-9_]*)",
    re.MULTILINE,
)
SWIFT_TYPE_RE = re.compile(
    r"^\s*(?:@\w+(?:\([^)]*\))?\s+)*"
    r"(?:(?:public|private|internal|fileprivate|open|final)\s+)*"
    r"(class|struct|enum|protocol|actor|extension)\s+([A-Za-z_][A-Za-z0-9_]*)",
    re.MULTILINE,
)
SWIFT_INIT_RE = re.compile(
    r"^\s*(?:(?:public|private|internal|fileprivate|open|"
    r"required|convenience|override)\s+)*"
    r"(init|deinit)\b",
    re.MULTILINE,
)


# Exclude list (dirs pruned during walk, files skipped at emission).
# Sourced from sibling `codegraph_exclude.txt` so the same config is
# edited without touching code (also consumed by `extract_contracts.py`).
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


DISCOVERY_EXCLUDE_DIRS, DISCOVERY_EXCLUDE_FILES = _load_exclude(EXCLUDE_FILE)


# Source-file suffixes counted upfront to give the progress bar a
# stable denominator. Mirrors what each language phase actually walks
# (Rust / Python / Swift / Chrome-ext JS / Node sources). Metadata
# files (`.md` / `.yaml` / `.toml` / `.json`) are skipped here — they
# tick too fast to add useful resolution and the orphan walker is the
# only late phase that visits them.
PROGRESS_SOURCE_EXTENSIONS: frozenset[str] = frozenset({
    ".rs", ".py", ".swift",
    ".ts", ".tsx", ".jsx", ".js", ".mjs", ".cjs",
    ".svelte", ".vue", ".html", ".css", ".scss",
})


def count_source_files(root: Path) -> int:
    """Walk `root` once to estimate the total source-file workload
    used by the progress bar. Same pruning as the per-phase scanners
    (`DISCOVERY_EXCLUDE_DIRS` + leading-dot dirs, file basenames in
    `DISCOVERY_EXCLUDE_FILES`) so the count matches the work actually
    done in `generate()`. Stack-based DFS keeps it allocation-light."""
    total = 0
    stack: list[Path] = [root]
    while stack:
        cur = stack.pop()
        try:
            entries = cur.iterdir()
        except (PermissionError, OSError):
            continue
        for entry in entries:
            try:
                if entry.is_dir():
                    if entry.name in DISCOVERY_EXCLUDE_DIRS or entry.name.startswith("."):
                        continue
                    stack.append(entry)
                elif entry.is_file():
                    if entry.name in DISCOVERY_EXCLUDE_FILES:
                        continue
                    if entry.suffix in PROGRESS_SOURCE_EXTENSIONS:
                        total += 1
            except OSError:
                continue
    return total


class Progress:
    """Stderr progress reporter — two-line on TTY (bar + current file
    below), single-line per phase when piped (so log files stay
    readable). On TTY it also prints a hero banner at startup.

    Color and cursor-control are gated independently: cursor-control
    requires a TTY; color additionally requires `NO_COLOR` to be unset
    (see https://no-color.org/). Bar updates throttle to ~10 fps."""

    BAR_WIDTH = 30
    RENDER_INTERVAL_SEC = 0.1

    # ANSI escape sequences. Disabled in __init__ (replaced with empty
    # strings) when stderr is not a TTY, or NO_COLOR is set.
    RESET = "\x1b[0m"
    BOLD = "\x1b[1m"
    DIM = "\x1b[2m"
    FG_GREEN = "\x1b[32m"
    FG_CYAN = "\x1b[36m"
    FG_MAGENTA = "\x1b[35m"
    FG_YELLOW = "\x1b[33m"
    BRIGHT_GREEN = "\x1b[92m"
    BRIGHT_CYAN = "\x1b[96m"
    # Cursor controls — disabled (empty) when not on a TTY.
    CURSOR_UP = "\x1b[1A"
    CLEAR_LINE = "\x1b[2K"
    HOME = "\r"

    def __init__(self, total: int) -> None:
        self.total = max(total, 1)
        self.current = 0
        self.phase = ""
        self.current_file = ""
        self._is_tty = sys.stderr.isatty()
        self._term_width = shutil.get_terminal_size((80, 20)).columns
        self._last_render = 0.0
        self._started = time.monotonic()
        self._lines_drawn = 0  # 0 → first render, 2 → 2-line layout drawn

        use_color = self._is_tty and "NO_COLOR" not in os.environ
        if not use_color:
            for attr in ("RESET", "BOLD", "DIM", "FG_GREEN", "FG_CYAN",
                         "FG_MAGENTA", "FG_YELLOW", "BRIGHT_GREEN", "BRIGHT_CYAN"):
                setattr(self, attr, "")
        if not self._is_tty:
            for attr in ("CURSOR_UP", "CLEAR_LINE", "HOME"):
                setattr(self, attr, "")
            self.HOME = "\r"  # `\r` is harmless on non-TTYs too

        if self._is_tty:
            self._print_banner()

    # ── Banner ───────────────────────────────────────────────────────

    def _print_banner(self) -> None:
        """Hero banner printed once per TTY run. Compact, ASCII-safe
        (one emoji + box-drawing chars), self-aligning thanks to a
        rule-only top/bottom (no right border to keep in sync)."""
        rule = f"{self.FG_CYAN}{'━' * 58}{self.RESET}"
        title = f"{self.BOLD}{self.FG_MAGENTA}CODEGRAPH{self.RESET}"
        sub = f"{self.DIM}repo-wide structural index{self.RESET}"
        spark = f"{self.FG_YELLOW}⚡{self.RESET}"
        sys.stderr.write("\n")
        sys.stderr.write(f"  {rule}\n")
        sys.stderr.write(f"     {spark}  {title}  {self.DIM}·{self.RESET}  {sub}\n")
        sys.stderr.write(f"  {rule}\n")
        sys.stderr.write("\n")
        sys.stderr.flush()

    # ── Public API ───────────────────────────────────────────────────

    def set_phase(self, label: str) -> None:
        self.phase = label
        self.current_file = ""
        if self._is_tty:
            self._render()
        else:
            print(f"  · {label}", file=sys.stderr, flush=True)

    def tick(self, n: int = 1, file: str | None = None) -> None:
        self.current += n
        if file is not None:
            self.current_file = file
        if not self._is_tty:
            return
        now = time.monotonic()
        if now - self._last_render >= self.RENDER_INTERVAL_SEC or self.current >= self.total:
            self._render()
            self._last_render = now

    def complete_file_scan(self) -> None:
        """Snap progress to 100% once all source-file phases are done.
        Keeps the bar from sitting at <100% during finalize when the
        upfront estimate slightly exceeds the per-phase tick count."""
        self.current = self.total
        if self._is_tty:
            self._render()

    def finish(self) -> None:
        if not self._is_tty:
            return
        if self._lines_drawn > 0:
            sys.stderr.write(
                self.CLEAR_LINE + "\n"
                + self.CLEAR_LINE
                + self.CURSOR_UP + self.HOME
            )
        sys.stderr.flush()

    # ── Render ───────────────────────────────────────────────────────

    def _bar_line(self) -> str:
        ratio = min(1.0, self.current / self.total)
        filled = int(ratio * self.BAR_WIDTH)
        fill_color = self.BRIGHT_GREEN if ratio >= 1.0 else self.FG_GREEN
        bar = (
            f"{fill_color}{'█' * filled}{self.RESET}"
            f"{self.DIM}{'░' * (self.BAR_WIDTH - filled)}{self.RESET}"
        )
        elapsed = time.monotonic() - self._started
        pct_color = self.BRIGHT_GREEN if ratio >= 1.0 else self.FG_YELLOW
        return (
            f"  [{bar}] "
            f"{self.BOLD}{pct_color}{ratio * 100:5.1f}%{self.RESET} "
            f"{self.current}/{self.total} "
            f"{self.DIM}{elapsed:5.1f}s{self.RESET}  "
            f"{self.BRIGHT_CYAN}{self.phase}{self.RESET}"
        )

    def _file_line(self) -> str:
        """Current file on a line of its own — keeps the bar untruncated
        even when paths are long. Head-ellipsis truncate so the
        filename tail (most informative) stays visible."""
        if not self.current_file:
            return f"  {self.DIM}·{self.RESET}"
        budget = self._term_width - 6
        shown = self.current_file
        if budget > 0 and len(shown) > budget:
            shown = "…" + shown[-(budget - 1):]
        return f"  {self.DIM}· {shown}{self.RESET}"

    def _render(self) -> None:
        if not self._is_tty:
            return
        line1 = self._bar_line()
        line2 = self._file_line()
        if self._lines_drawn == 0:
            # First render: write both lines, then move cursor back up
            # to the start of line 1 ready for in-place updates.
            sys.stderr.write(line1 + "\n" + line2 + self.CURSOR_UP + self.HOME)
        else:
            # Subsequent: cursor sits at line 1 col 0. Clear + redraw both.
            sys.stderr.write(
                self.CLEAR_LINE + line1 + "\n"
                + self.CLEAR_LINE + line2
                + self.CURSOR_UP + self.HOME
            )
        self._lines_drawn = 2
        sys.stderr.flush()


def _walk_for_manifests(root: Path, filename: str) -> list[Path]:
    """Walk `root` for files named `filename`, skipping `DISCOVERY_EXCLUDE_DIRS`.
    Used for both `Cargo.toml` (Rust) and `Package.swift` (Swift)
    discovery. Yields absolute paths in deterministic order."""
    hits: list[Path] = []
    # Stack-based DFS so we can prune directories before descending.
    stack: list[Path] = [root]
    while stack:
        cur = stack.pop()
        try:
            entries = sorted(cur.iterdir())
        except (PermissionError, OSError):
            continue
        for entry in entries:
            if entry.is_dir():
                if entry.name in DISCOVERY_EXCLUDE_DIRS or entry.name.startswith("."):
                    continue
                stack.append(entry)
            elif entry.name == filename:
                hits.append(entry)
    hits.sort()
    return hits


# Minimal TOML peek for the `[package]` table. Avoids pulling in a
# real TOML parser (which is available in 3.11+ via `tomllib`, but
# we keep this resilient to older interpreters and don't need full
# parsing — only `name` + `version` from `[package]`).
_TOML_PACKAGE_TABLE_RE = re.compile(r"^\[package\]\s*$", re.MULTILINE)
_TOML_NAME_RE = re.compile(r"^name\s*=\s*\"([^\"]+)\"\s*$", re.MULTILINE)
_TOML_VERSION_RE = re.compile(r"^version\s*=\s*\"([^\"]+)\"\s*$", re.MULTILINE)


def _parse_cargo_package(manifest_path: Path) -> tuple[str, str] | None:
    """Return `(name, version)` from a Cargo.toml's `[package]` table,
    or `None` if either field is missing (e.g. workspace-only manifests)."""
    try:
        content = manifest_path.read_text(encoding="utf-8", errors="ignore")
    except OSError:
        return None
    table_match = _TOML_PACKAGE_TABLE_RE.search(content)
    if not table_match:
        return None
    # Limit name/version search to the [package] table body — every
    # table ends at the next `^[ ... ]` header or EOF.
    body_start = table_match.end()
    next_table = re.search(r"^\[", content[body_start:], re.MULTILINE)
    body = content[body_start : body_start + (next_table.start() if next_table else len(content))]
    name_match = _TOML_NAME_RE.search(body)
    version_match = _TOML_VERSION_RE.search(body)
    if not name_match or not version_match:
        return None
    return name_match.group(1), version_match.group(1)


def discover_out_of_workspace_rust_crates(
    root: Path,
    existing_packages: list[dict],
) -> list[dict]:
    """Walk the repo for `Cargo.toml` manifests, parse `[package]`, and
    return pseudo-`cargo metadata` entries for crates not already in
    the main workspace. Crates outside the workspace (e.g.
    `desktop/src-tauri` with its own Cargo.lock) get picked up here
    automatically — no allow-list to maintain."""
    seen_names = {pkg["name"] for pkg in existing_packages}
    seen_manifests = {Path(pkg["manifest_path"]).resolve() for pkg in existing_packages}
    out: list[dict] = []
    for manifest in _walk_for_manifests(root, "Cargo.toml"):
        if manifest.resolve() in seen_manifests:
            continue
        parsed = _parse_cargo_package(manifest)
        if parsed is None:
            # Workspace-only Cargo.toml (no `[package]`) — skip.
            continue
        name, version = parsed
        if name in seen_names:
            continue
        if not (manifest.parent / "src").exists():
            # No `src/` dir means nothing for the Rust scan loop to do;
            # likely a virtual workspace member shell.
            continue
        seen_names.add(name)
        out.append({
            "name": name,
            "version": version,
            "manifest_path": str(manifest),
            "id": f"path+file://{manifest.parent}#{name}@{version}",
            "dependencies": [],
        })
    return out


def discover_swift_projects(root: Path) -> list[tuple[str, Path]]:
    """Discover SwiftPM packages and XcodeGen application projects.

    SwiftPM projects are rooted by ``Package.swift``. XcodeGen projects are
    rooted by a ``project.yml`` containing both ``name:`` and ``targets:`` and
    at least one Swift source below it. The latter is the canonical manifest
    for Magios; treating it as a first-class project keeps app, extension,
    widget, unit-test, and UI-test targets in one graph crate instead of
    degrading their files into symbol-less ``repo-root`` orphans.
    """
    projects: dict[Path, tuple[str, Path]] = {}
    for manifest in _walk_for_manifests(root, "Package.swift"):
        pkg_dir = manifest.parent
        projects[pkg_dir.resolve()] = (pkg_dir.name, pkg_dir)

    name_re = re.compile(r"(?m)^\s*name\s*:\s*['\"]?([^'\"#\n]+)")
    targets_re = re.compile(r"(?m)^\s*targets\s*:")
    for manifest in _walk_for_manifests(root, "project.yml"):
        project_dir = manifest.parent
        if project_dir.resolve() in projects:
            continue
        try:
            content = manifest.read_text(encoding="utf-8", errors="ignore")
        except OSError:
            continue
        name_match = name_re.search(content)
        if name_match is None or targets_re.search(content) is None:
            continue
        if not any(path.suffix == ".swift" for path in _walk_pruned(project_dir)):
            continue
        project_name = name_match.group(1).strip()
        if not project_name:
            continue
        projects[project_dir.resolve()] = (project_name, project_dir)

    return sorted(projects.values(), key=lambda item: str(item[1]))


def _normalize_npm_name(name: str | None, fallback_dir: Path) -> str:
    """Return a clean project name. For npm packages, strips the
    `@scope/` prefix (npm convention) so `@org/some-lib` becomes
    `some-lib`. Falls back to the directory name when no usable name
    is present."""
    if not name:
        return fallback_dir.name
    if name.startswith("@") and "/" in name:
        return name.split("/", 1)[1]
    return name


def _name_from_path(project_root: Path, repo_root: Path) -> str:
    """Disambiguate two projects with the same directory name by
    including a parent-directory hint. `foo/bar/extension` →
    `bar-extension`, `baz/extension` → `baz-extension` instead of
    both colliding on `extension`. For projects at the repo root,
    use the directory name as-is."""
    rel = project_root.relative_to(repo_root)
    parts = rel.parts
    if len(parts) <= 1:
        return parts[-1] if parts else "root"
    return f"{parts[-2]}-{parts[-1]}"


def discover_node_projects(root: Path) -> list[tuple[str, Path, Path]]:
    """Walk for `package.json` manifests and return
    `(project_name, project_root, scan_root)` tuples.

    `scan_root` is now the project root itself — earlier we restricted
    it to `src/`, which missed top-level `index.html`, `vite.config.ts`,
    `*.mjs` scripts, config files, etc. that are real first-party code.
    Pruning of `node_modules/`, `dist/`, `build/`, `.svelte-kit/` and
    friends is the walker's job (see `_walk_pruned`).

    Workspace-shell `package.json` files with no sources at all still
    get a crate node (so they show up in the graph) but yield zero
    files — the source loop just no-ops on them.

    Name precedence: `package.json::name` (stripping `@scope/`),
    falling back to the directory name.
    """
    out: list[tuple[str, Path, Path]] = []
    for manifest in _walk_for_manifests(root, "package.json"):
        pkg_dir = manifest.parent
        try:
            data = json.loads(manifest.read_text(encoding="utf-8", errors="ignore"))
        except (ValueError, OSError):
            data = {}
        name = _normalize_npm_name(data.get("name"), pkg_dir)
        out.append((name, pkg_dir, pkg_dir))
    return out


def discover_chrome_extensions(root: Path) -> list[tuple[str, Path]]:
    """Walk for `manifest.json` files carrying a `manifest_version`
    field (the Chrome / MV3 extension marker). Returns
    `(project_name, extension_root)` tuples. The project name uses
    `<parent>-<dirname>` so generic dir names like `extension`,
    `ext`, `addon` don't collide across different products."""
    out: list[tuple[str, Path]] = []
    for manifest in _walk_for_manifests(root, "manifest.json"):
        try:
            data = json.loads(manifest.read_text(encoding="utf-8", errors="ignore"))
        except (ValueError, OSError):
            continue
        if not isinstance(data, dict) or "manifest_version" not in data:
            continue
        ext_dir = manifest.parent
        if ext_dir.name in {"extension", "ext", "addon"} and ext_dir.parent != root:
            crate_name = f"{ext_dir.parent.name}-{ext_dir.name}"
        else:
            crate_name = ext_dir.name
        out.append((crate_name, ext_dir))
    return out


def discover_python_projects(root: Path) -> list[tuple[str, Path]]:
    """Walk for Python project markers (`pyproject.toml` or
    `setup.py`) and return `(project_name, project_root)` tuples.
    The whole project tree is scanned recursively — no privileging
    of `src/` (that's a layout choice, not a scan rule). Module
    paths are derived later via `derive_ui_module_path`, which
    strips a leading `src` so both flat and src-layout projects
    yield identical module IDs.

    Project name: `pyproject.toml::[project].name` if parseable,
    else `[tool.poetry].name`, else the directory name."""
    out: list[tuple[str, Path]] = []
    seen_roots: set[Path] = set()
    for marker_name in ("pyproject.toml", "setup.py"):
        for manifest in _walk_for_manifests(root, marker_name):
            proj_root = manifest.parent
            if proj_root in seen_roots:
                continue
            seen_roots.add(proj_root)
            name = proj_root.name
            if marker_name == "pyproject.toml":
                try:
                    content = manifest.read_text(encoding="utf-8", errors="ignore")
                    nm = _TOML_NAME_RE.search(content)
                    if nm:
                        name = nm.group(1)
                except OSError:
                    pass
            out.append((name, proj_root))
    return out


# File extensions the generic Node scan picks up. Covers TypeScript
# (.ts), React/TSX (.tsx, .jsx), plain JavaScript (.js, .mjs, .cjs),
# Svelte components (.svelte), Vue SFCs (.vue), and static surface
# files (.html, .css, .scss) — the last group lands as file-only
# nodes (no symbol extraction; their value is navigation /
# "what files exist under this project"). `.d.ts` declarations are
# filtered out to avoid duplicating the symbols they re-export.
NODE_SOURCE_EXTENSIONS: tuple[str, ...] = (
    ".ts", ".tsx", ".jsx", ".js", ".mjs", ".cjs",
    ".svelte", ".vue",
    ".html", ".css", ".scss",
)
NODE_SYMBOL_EXTENSIONS: frozenset[str] = frozenset({
    ".ts", ".tsx", ".jsx", ".js", ".mjs", ".cjs", ".svelte", ".vue",
})


# ── Safe metadata extraction from YAML / TOML / JSON / Markdown ─────
#
# The engine pulls a SMALL set of identifying metadata from non-code
# files (project name, schema kind, document title, etc.) so they're
# navigable from `cgraph_search` — but it never lifts arbitrary
# values. Three guardrails:
#
#  1. **Key allowlist** (`SAFE_METADATA_KEYS`) — only fields whose name
#     identifies the file ("what is this?") rather than configures
#     behaviour ("how does this run?"). `name`, `title`, `version`,
#     `kind`, `description`, `tags`, etc.
#
#  2. **Unsafe key pattern** (`UNSAFE_KEY_RE`) — even within the
#     allowlist, reject any key whose name HINTS at a secret or
#     environment-coupled config. Covers `*secret*`, `*token*`,
#     `*password*`, `*credential*`, `*api_key*`, `*endpoint*`,
#     `*host*`, `*port*`, etc.
#
#  3. **Secret-shaped values** (`SECRET_VALUE_PATTERNS`) — high-
#     entropy strings that LOOK like API keys / JWTs / AWS keys /
#     hex blobs / base64 are skipped on output even if the key
#     itself passes the allowlist.
#
# Filename allowlist gate (`SECRET_FILE_RE`) skips entire files
# whose name signals secrets (`.env`, `*credentials*`,
# `*-secret*.yaml`, etc.) so we never even read them.
SAFE_METADATA_KEYS: frozenset[str] = frozenset({
    "name", "title", "id", "kind", "type", "category", "categories",
    "tags", "version", "description", "summary", "author", "license",
    "agent_id", "agent", "tool", "tool_id", "skill", "skill_id",
    "package", "module", "namespace", "language",
})
UNSAFE_KEY_RE = re.compile(
    r"(?i)(secret|token|password|credential|auth|api[_-]?key|"
    r"private[_-]?key|access[_-]?key|certificate|webhook|"
    r"endpoint|url|host|port|address|connection|dsn|database|"
    r"bearer|cookie|session)"
)
SECRET_VALUE_PATTERNS: tuple[re.Pattern, ...] = (
    re.compile(r"^sk-[A-Za-z0-9_\-]{20,}$"),         # OpenAI-style
    re.compile(r"^eyJ[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+"),  # JWT
    re.compile(r"^AKIA[0-9A-Z]{16}$"),                # AWS access key
    re.compile(r"^ghp_[A-Za-z0-9]{20,}$"),            # GitHub PAT
    re.compile(r"^xoxb-[A-Za-z0-9\-]{20,}$"),         # Slack bot token
    re.compile(r"^[A-Fa-f0-9]{40,}$"),                # hex blob ≥40 chars
    re.compile(r"^[A-Za-z0-9+/]{50,}={0,2}$"),        # base64 blob ≥50 chars
)
SECRET_FILE_RE = re.compile(
    r"(?i)(^|/)(\.env(\..*)?|secrets?\.(?:ya?ml|json|toml)|"
    r"credentials?\.(?:json|ya?ml|toml)|"
    r".*[._-]secret[._-]?.*\.(?:ya?ml|json|toml)|"
    r"keyring|tokens?\.json)$"
)


def _value_looks_safe(key: str, value: object) -> bool:
    """True iff a `(key, value)` pair is safe to include in extracted
    metadata. Keys hinting at secrets are rejected outright; string
    values matching secret patterns are dropped; large strings (>200
    chars) are dropped as likely-descriptive-config not navigable
    metadata."""
    if UNSAFE_KEY_RE.search(key):
        return False
    if isinstance(value, bool):
        return True  # bool first — bool is a subclass of int
    if isinstance(value, (int, float)):
        return True
    if isinstance(value, str):
        if len(value) > 200:
            return False
        for pat in SECRET_VALUE_PATTERNS:
            if pat.match(value):
                return False
        return True
    if isinstance(value, list):
        # Allow flat list of short scalars only — nested structures
        # are out of scope; their fields would need their own
        # allowlist pass.
        if not value:
            return True
        return all(
            isinstance(item, str) and len(item) <= 100
            for item in value
        )
    return False


def extract_safe_metadata(data: object) -> dict[str, object]:
    """Pluck `SAFE_METADATA_KEYS` from a top-level dict, filtered by
    `_value_looks_safe`. Returns `{}` for non-dict inputs so the
    caller doesn't need to defend against shape surprises."""
    if not isinstance(data, dict):
        return {}
    out: dict[str, object] = {}
    for key, value in data.items():
        if not isinstance(key, str):
            continue
        if key.lower() not in SAFE_METADATA_KEYS:
            continue
        if not _value_looks_safe(key.lower(), value):
            continue
        out[key.lower()] = value
    return out


# Headings in Markdown: `^# Title`, `^## Section`, etc. We pull h1/h2
# only — deeper levels (h3+) clutter the graph without adding much
# navigation value. Heading text becomes a `section` symbol so
# `cgraph_search "Restart durability"` lands on the right .md anchor.
MD_HEADING_RE = re.compile(r"^(#{1,2})\s+(.+?)\s*$", re.MULTILINE)
# Optional YAML frontmatter at the top of a Markdown file
# (Jekyll / Hugo / docusaurus convention): `^---\n...key: value...\n---`.
MD_FRONTMATTER_RE = re.compile(r"\A---\n(.*?)\n---\n", re.DOTALL)


def extract_markdown_metadata(content: str) -> tuple[dict[str, object], list[dict]]:
    """Return `(frontmatter_metadata, headings)` for a Markdown file.
    Frontmatter is parsed via `yaml.safe_load` (if available) and
    filtered through `extract_safe_metadata`. Headings extract h1+h2
    only with the title text and line number."""
    metadata: dict[str, object] = {}
    fm_match = MD_FRONTMATTER_RE.match(content)
    if fm_match:
        try:
            import yaml  # noqa: WPS433 — optional dep, gracefully skipped
            data = yaml.safe_load(fm_match.group(1))
            metadata = extract_safe_metadata(data)
        except Exception:
            metadata = {}
    headings: list[dict] = []
    for m in MD_HEADING_RE.finditer(content):
        title = m.group(2).strip()
        if not title or len(title) > 200:
            continue
        level = len(m.group(1))
        line = content.count("\n", 0, m.start()) + 1
        headings.append({"level": level, "title": title, "line": line})
    return metadata, headings


def safe_parse_metadata_file(file_path: Path) -> dict[str, object]:
    """Read + parse a single YAML/TOML/JSON file and return ONLY the
    allowlisted metadata fields. Returns `{}` for parse failures or
    unsupported extensions. NEVER raises."""
    suffix = file_path.suffix.lower()
    try:
        raw = file_path.read_text(encoding="utf-8", errors="ignore")
    except (OSError, UnicodeDecodeError):
        return {}
    try:
        if suffix in {".yaml", ".yml"}:
            try:
                import yaml  # noqa: WPS433 — optional dep
            except ImportError:
                return {}
            data = yaml.safe_load(raw)
        elif suffix == ".toml":
            try:
                import tomllib  # Python 3.11+
                data = tomllib.loads(raw)
            except (ImportError, Exception):
                return {}
        elif suffix == ".json":
            data = json.loads(raw)
        else:
            return {}
    except Exception:
        return {}
    return extract_safe_metadata(data)


# File extensions the metadata scan picks up. NEVER read for content
# beyond the allowlist; never indexed as source code.
METADATA_FILE_EXTENSIONS: tuple[str, ...] = (".yaml", ".yml", ".toml", ".json", ".md")


def _walk_pruned(root: Path, *, skip_subroots: frozenset[Path] = frozenset()):
    """Yield every file under `root`, pruning `DISCOVERY_EXCLUDE_DIRS`
    and leading-dot dirs during descent, and skipping any file whose
    basename appears in `DISCOVERY_EXCLUDE_FILES`.

    `skip_subroots` lets a per-project scanner avoid descending into
    nested projects (e.g., a Node project containing a Chrome-ext
    manifest). Each entry should be a directory path; when descent
    hits an entry whose resolved path matches, it's pruned. The
    scanner's own project root is never skipped, so callers can pass
    "all project roots" wholesale without filtering."""
    root_resolved = root.resolve()
    skip_resolved: set[Path] = {
        p.resolve() for p in skip_subroots if p.resolve() != root_resolved
    }
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
                    if entry.name in DISCOVERY_EXCLUDE_DIRS or entry.name.startswith("."):
                        continue
                    if skip_resolved and entry.resolve() in skip_resolved:
                        continue
                    stack.append(entry)
                elif entry.is_file():
                    if entry.name in DISCOVERY_EXCLUDE_FILES:
                        continue
                    yield entry
            except OSError:
                continue


def emit_metadata_files_for_project(
    project_root: Path,
    repo_root: Path,
    crate_id: str,
    crate_name: str,
    add_node: callable,
    add_edge: callable,
) -> None:
    """Walk `project_root` for YAML/TOML/JSON/MD files (skipping
    excluded build dirs and known-secret filenames), emit a `file`
    node for each (with allowlisted metadata as the `metadata`
    attribute), and emit `section` symbol nodes for Markdown
    headings. Safe to call once per discovered project."""
    for ext in METADATA_FILE_EXTENSIONS:
        for file_path in sorted(project_root.rglob(f"*{ext}")):
            rel = relpath(file_path, repo_root)
            # Defense in depth: prune build-output and secret-leaning
            # paths even though the project walk already excluded
            # most of these — sub-dirs deep inside a project can
            # still be `target/`, `node_modules/`, etc. Also skip
            # leading-dot dirs (`.svelte-kit/`, `.claude/`, etc.) which
            # are conventionally tool-private state, mirroring the
            # orphan walker's rule.
            if any(part in DISCOVERY_EXCLUDE_DIRS or part.startswith(".")
                   for part in file_path.relative_to(project_root).parts):
                continue
            if file_path.name in DISCOVERY_EXCLUDE_FILES:
                continue
            if SECRET_FILE_RE.search(rel):
                continue
            file_id = f"file::{rel}"
            module_path = derive_ui_module_path(crate_name, file_path.relative_to(project_root))
            module_id = f"module::{module_path}"
            kwargs: dict[str, object] = {"path": rel, "crate": crate_name, "module": module_path}
            headings: list[dict] = []
            if ext == ".md":
                try:
                    raw = file_path.read_text(encoding="utf-8", errors="ignore")
                except (OSError, UnicodeDecodeError):
                    raw = ""
                if raw:
                    md_meta, headings = extract_markdown_metadata(raw)
                    if md_meta:
                        kwargs["metadata"] = md_meta
            else:
                md_meta = safe_parse_metadata_file(file_path)
                if md_meta:
                    kwargs["metadata"] = md_meta
            add_node(file_id, "file", file_path.name, **kwargs)
            add_node(module_id, "module", module_path, path=rel, crate=crate_name, module=module_path)
            add_edge(crate_id, file_id, "contains")
            add_edge(crate_id, module_id, "contains")
            add_edge(module_id, file_id, "contains")
            # Markdown h1/h2 → section symbols for in-doc navigation.
            for h in headings:
                title = h["title"]
                # Strip markdown link/emphasis wrappers for cleaner labels.
                clean = re.sub(r"[`*_\[\]]", "", title).strip()
                if not clean:
                    continue
                sym_id = f"symbol::{rel}::section::{clean}::{h['line']}"
                add_node(
                    sym_id, "section", clean,
                    path=rel, crate=crate_name, module=module_path,
                    line=h["line"], level=h["level"],
                )
                add_edge(file_id, sym_id, "defines")
                add_edge(module_id, sym_id, "contains")


def collect_node_source_files(
    scan_root: Path,
    *,
    skip_subroots: frozenset[Path] = frozenset(),
) -> list[Path]:
    """Return every file under `scan_root` whose suffix is in
    `NODE_SOURCE_EXTENSIONS`, skipping TypeScript declaration files
    (`.d.ts`). Uses `_walk_pruned` so descent skips `node_modules/`,
    `dist/`, `build/`, `.svelte-kit/`, `.next/`, etc. — required now
    that `scan_root` is the project root rather than `src/`.
    `skip_subroots` is forwarded so nested project trees aren't
    double-walked when one scanner's tree contains another's root."""
    valid = set(NODE_SOURCE_EXTENSIONS)
    files: list[Path] = []
    for path in _walk_pruned(scan_root, skip_subroots=skip_subroots):
        if path.suffix not in valid:
            continue
        if path.name.endswith(".d.ts"):
            continue
        files.append(path)
    files.sort()
    return files


def extract_swift_symbols(content: str) -> tuple[list[dict], list[dict]]:
    """Return (functions, types) lists for a Swift source. Each entry
    has `name` and `line` (1-indexed). Modeled after `extract_ts_*` so
    the existing add-node loop can consume the output directly."""
    funcs: list[dict] = []
    types: list[dict] = []
    # Map char offset → line number for O(1) line lookup per match.
    line_starts = [0]
    for idx, ch in enumerate(content):
        if ch == "\n":
            line_starts.append(idx + 1)

    def line_for(pos: int) -> int:
        # Binary-ish search via bisect — but list is small enough that
        # linear from-end is fine; Swift files are < 5k lines.
        for i in range(len(line_starts) - 1, -1, -1):
            if line_starts[i] <= pos:
                return i + 1
        return 1

    for m in SWIFT_FUNC_RE.finditer(content):
        funcs.append({"name": m.group(1), "line": line_for(m.start())})
    for m in SWIFT_INIT_RE.finditer(content):
        funcs.append({"name": m.group(1), "line": line_for(m.start())})
    for m in SWIFT_TYPE_RE.finditer(content):
        kind = m.group(1)
        types.append({"name": m.group(2), "kind": kind, "line": line_for(m.start())})
    funcs.sort(key=lambda item: int(item["line"]))
    for index, func in enumerate(funcs):
        start_line = int(func["line"])
        if index + 1 < len(funcs):
            next_line = int(funcs[index + 1]["line"])
            func["line_end"] = max(start_line, next_line - 1)
        else:
            func["line_end"] = start_line + 2000
    return funcs, types


# Regex used by `extract_svelte_script` — matches every `<script ...>...</script>`
# block in a Svelte source file. We extract the contents of `<script>` and
# `<script context="module">` blocks; everything else (template / style) is
# replaced with newlines so the line numbers reported by the downstream TS
# extractors line up with the original `.svelte` source.
SVELTE_SCRIPT_RE = re.compile(r"<script\b[^>]*>(.*?)</script>", re.DOTALL | re.IGNORECASE)


def extract_svelte_script(content: str) -> str:
    """Return the concatenated TS/JS body of every `<script>` block in a
    Svelte source, with non-script regions replaced by newlines so the
    line numbers of extracted symbols match the original `.svelte` file.
    Returns an empty string when no `<script>` block is present (pure-markup
    components — render-only, nothing for the graph to index)."""
    pieces: list[str] = []
    last_end = 0
    for match in SVELTE_SCRIPT_RE.finditer(content):
        prelude = content[last_end : match.start(1)]
        # Keep only newlines from the prelude so the captured script body
        # starts at the same line number it occupies in the source file.
        pieces.append(re.sub(r"[^\n]", "", prelude))
        pieces.append(match.group(1))
        last_end = match.end(1)
    tail = content[last_end:]
    pieces.append(re.sub(r"[^\n]", "", tail))
    return "".join(pieces)


def validate_graph_object(obj: dict[str, Any]) -> list[str]:
    errors: list[str] = []
    required_top = {"version", "generated_at", "commit", "workspace", "nodes", "edges"}
    missing = sorted(required_top - set(obj.keys()))
    if missing:
        errors.append(f"missing top-level keys: {', '.join(missing)}")

    if not isinstance(obj.get("nodes", []), list):
        errors.append("nodes must be a list")
    if not isinstance(obj.get("edges", []), list):
        errors.append("edges must be a list")

    for i, node in enumerate(obj.get("nodes", [])):
        if not isinstance(node, dict):
            errors.append(f"node[{i}] must be an object")
            continue
        for key in ("id", "kind", "label"):
            if key not in node:
                errors.append(f"node[{i}] missing key: {key}")

    for i, edge in enumerate(obj.get("edges", [])):
        if not isinstance(edge, dict):
            errors.append(f"edge[{i}] must be an object")
            continue
        for key in ("from", "to", "kind"):
            if key not in edge:
                errors.append(f"edge[{i}] missing key: {key}")

    return errors


def _count_by(items: list[dict[str, Any]], key: str) -> dict[str, int]:
    counts: dict[str, int] = defaultdict(int)
    for item in items:
        value = str(item.get(key, "unknown"))
        counts[value] += 1
    return counts


def _line_no(content: str, position: int) -> int:
    return content.count("\n", 0, position) + 1


def nearest_scope_prefix(scopes: list[tuple[int, str]], position: int) -> str:
    nearest = ""
    for scope_pos, scope_value in scopes:
        if scope_pos > position:
            break
        if position - scope_pos > 4000:
            continue
        nearest = scope_value
    return nearest


def join_scope_and_route(scope: str, route: str) -> str:
    scope_part = str(scope or "").strip()
    route_part = str(route or "").strip()

    if scope_part and not scope_part.startswith("/"):
        scope_part = "/" + scope_part
    if route_part and not route_part.startswith("/"):
        route_part = "/" + route_part

    if not scope_part and not route_part:
        return "/"
    if not route_part:
        return scope_part or "/"
    if not scope_part:
        return route_part

    joined = f"{scope_part.rstrip('/')}{route_part}"
    return joined or "/"


API_PREFIX_RE = re.compile(r"^/api/magician/v\d+")


def normalize_api_path(raw_path: str) -> str:
    text = str(raw_path or "").strip()
    if not text:
        return ""

    text = re.sub(r"^[A-Za-z][A-Za-z0-9+.-]*://[^/]+", "", text)
    text = text.split("?", 1)[0].split("#", 1)[0]
    if not text:
        return "/"
    if not text.startswith("/"):
        if text.startswith("api/"):
            text = "/" + text
        else:
            return ""
    text = re.sub(r"/+", "/", text)
    # Strip common API version prefix (e.g., /api/magician/v2/chat/enroll -> /chat/enroll)
    text = API_PREFIX_RE.sub("", text)
    if len(text) > 1:
        text = text.rstrip("/")
    return text or "/"


def endpoint_path_regex(path: str) -> re.Pattern[str]:
    pattern_parts: list[str] = []
    idx = 0
    while idx < len(path):
        if path[idx] == "{":
            end = path.find("}", idx + 1)
            if end != -1:
                pattern_parts.append("[^/]+")
                idx = end + 1
                continue
        pattern_parts.append(re.escape(path[idx]))
        idx += 1
    return re.compile("^" + "".join(pattern_parts) + "$")


def base_type_name(raw_type: str) -> str:
    text = str(raw_type or "").strip()
    text = text.replace("&", " ").replace("mut ", "")
    text = re.sub(r"\s+", " ", text).strip()
    if not text:
        return ""

    wrappers = {
        "Option",
        "Vec",
        "HashMap",
        "BTreeMap",
        "Arc",
        "Box",
        "Json",
        "Query",
        "Path",
        "web::Json",
        "web::Query",
        "web::Path",
    }

    while "<" in text and text.endswith(">"):
        outer = text.split("<", 1)[0].strip()
        inner = text[text.find("<") + 1 : -1].strip()
        outer_base = outer.split("::")[-1]
        if outer in wrappers or outer_base in wrappers:
            if not inner:
                break
            if "," in inner:
                inner = inner.split(",", 1)[0].strip()
            text = inner
            continue
        break

    text = text.strip()
    if not text:
        return ""
    if text.startswith("(") and text.endswith(")"):
        return "tuple"
    if "::" in text:
        text = text.split("::")[-1]
    return text.strip()


def split_top_level_arguments(raw_text: str) -> list[str]:
    parts: list[str] = []
    current: list[str] = []
    depth_angle = 0
    depth_paren = 0
    depth_bracket = 0
    depth_brace = 0

    for ch in str(raw_text or ""):
        if ch == "<":
            depth_angle += 1
        elif ch == ">" and depth_angle > 0:
            depth_angle -= 1
        elif ch == "(":
            depth_paren += 1
        elif ch == ")" and depth_paren > 0:
            depth_paren -= 1
        elif ch == "[":
            depth_bracket += 1
        elif ch == "]" and depth_bracket > 0:
            depth_bracket -= 1
        elif ch == "{":
            depth_brace += 1
        elif ch == "}" and depth_brace > 0:
            depth_brace -= 1

        if ch == "," and depth_angle == 0 and depth_paren == 0 and depth_bracket == 0 and depth_brace == 0:
            value = "".join(current).strip()
            if value:
                parts.append(value)
            current = []
            continue
        current.append(ch)

    trailing = "".join(current).strip()
    if trailing:
        parts.append(trailing)
    return parts


def parse_outer_generic_type(raw_type: str) -> tuple[str, list[str]]:
    text = str(raw_type or "").strip()
    if "<" not in text or not text.endswith(">"):
        return text, []
    lt_index = text.find("<")
    if lt_index <= 0:
        return text, []
    outer = text[:lt_index].strip()
    inner = text[lt_index + 1 : -1].strip()
    if not outer or not inner:
        return text, []
    return outer, split_top_level_arguments(inner)


def sample_value_for_type(
    raw_type: str,
    depth: int = 0,
    template_resolver: Callable[[str], Any] | None = None,
) -> Any:
    if depth > 4:
        return "value"

    text = str(raw_type or "").strip()
    if not text:
        return "value"
    text = text.replace("&", " ").replace("mut ", "")
    text = re.sub(r"\s+", " ", text).strip()

    if text.startswith("(") and text.endswith(")"):
        members = split_top_level_arguments(text[1:-1].strip())
        if not members:
            return ["value"]
        return [
            sample_value_for_type(member, depth + 1, template_resolver=template_resolver)
            for member in members
        ]

    outer, inner_args = parse_outer_generic_type(text)
    outer_base = outer.split("::")[-1]
    if inner_args:
        pass_through_wrappers = {
            "Option",
            "Arc",
            "Box",
            "Mutex",
            "RwLock",
            "Cow",
            "Pin",
            "Json",
            "Query",
            "Path",
        }
        list_wrappers = {
            "Vec",
            "VecDeque",
            "LinkedList",
            "BinaryHeap",
            "HashSet",
            "BTreeSet",
        }
        map_wrappers = {
            "HashMap",
            "BTreeMap",
            "IndexMap",
        }
        if outer_base in pass_through_wrappers and inner_args:
            return sample_value_for_type(inner_args[0], depth + 1, template_resolver=template_resolver)
        if outer_base in list_wrappers and inner_args:
            return [sample_value_for_type(inner_args[0], depth + 1, template_resolver=template_resolver)]
        if outer_base in map_wrappers and len(inner_args) >= 2:
            value = sample_value_for_type(inner_args[1], depth + 1, template_resolver=template_resolver)
            return {"key": value}
        if outer_base == "Result" and inner_args:
            return sample_value_for_type(inner_args[0], depth + 1, template_resolver=template_resolver)

    base = base_type_name(text)
    if not base:
        return "value"

    lowered = base.lower()
    if lowered in {"string", "str"}:
        return "text"
    if lowered in {
        "bool",
    }:
        return True
    if lowered in {
        "i8",
        "i16",
        "i32",
        "i64",
        "i128",
        "isize",
        "u8",
        "u16",
        "u32",
        "u64",
        "u128",
        "usize",
    }:
        return 1
    if lowered in {"f32", "f64"}:
        return 1.0
    if lowered == "uuid":
        return "00000000-0000-0000-0000-000000000000"
    if lowered == "tuple":
        return ["value"]
    if lowered == "value":
        return {}

    if template_resolver is not None:
        try:
            resolved = template_resolver(base)
        except Exception:
            resolved = None
        if resolved is not None:
            if isinstance(resolved, (dict, list)):
                return json.loads(json.dumps(resolved))
            return resolved

    return {"$ref": base}


def build_struct_template(
    fields: list[dict[str, str]],
    template_resolver: Callable[[str], Any] | None = None,
) -> dict[str, Any]:
    payload: dict[str, Any] = {}
    for field in fields:
        payload[field["name"]] = sample_value_for_type(
            field["type"],
            template_resolver=template_resolver,
        )
    return payload


def extract_enum_definitions(content: str) -> dict[str, list[dict[str, Any]]]:
    result: dict[str, list[dict[str, Any]]] = {}
    for enum_match in RUST_ENUM_RE.finditer(content):
        enum_name = enum_match.group(1)
        body = enum_match.group(2)
        variants: list[dict[str, Any]] = []
        for raw_line in body.splitlines():
            line = raw_line.split("//", 1)[0].strip()
            if not line or line.startswith("#["):
                continue
            match = re.match(
                r"^([A-Za-z_][A-Za-z0-9_]*)\s*(?:\(([^)]*)\)|\{([^}]*)\})?\s*,?$",
                line,
            )
            if not match:
                continue
            variant_name = match.group(1)
            tuple_fields = match.group(2)
            struct_fields = match.group(3)
            if tuple_fields is not None:
                tuple_types = split_top_level_arguments(tuple_fields)
                variants.append(
                    {
                        "name": variant_name,
                        "kind": "tuple",
                        "types": tuple_types,
                    }
                )
                continue
            if struct_fields is not None:
                fields: list[dict[str, str]] = []
                for field_text in split_top_level_arguments(struct_fields):
                    field_match = re.match(
                        r"^\s*([A-Za-z_][A-Za-z0-9_]*)\s*:\s*(.+)\s*$",
                        field_text,
                    )
                    if not field_match:
                        continue
                    fields.append(
                        {
                            "name": field_match.group(1),
                            "type": field_match.group(2).strip(),
                        }
                    )
                variants.append(
                    {
                        "name": variant_name,
                        "kind": "struct",
                        "fields": fields,
                    }
                )
                continue
            variants.append({"name": variant_name, "kind": "unit"})
        if variants:
            result[enum_name] = variants
    return result


def build_enum_template(
    variants: list[dict[str, Any]],
    template_resolver: Callable[[str], Any] | None = None,
) -> dict[str, Any]:
    if not variants:
        return {"variant": "Unknown"}
    first = variants[0]
    kind = str(first.get("kind", "unit"))
    name = str(first.get("name", "Unknown"))
    payload: dict[str, Any] = {"variant": name}
    if kind == "tuple":
        types = list(first.get("types", []))
        values = [
            sample_value_for_type(type_name, template_resolver=template_resolver)
            for type_name in types
        ]
        payload["value"] = values[0] if len(values) == 1 else values
    elif kind == "struct":
        fields = list(first.get("fields", []))
        payload["value"] = build_struct_template(fields, template_resolver=template_resolver)
    return payload


def build_path_param_template(path: str) -> dict[str, Any]:
    template: dict[str, Any] = {}
    for param in re.findall(r"\{([^}]+)\}", path):
        key = str(param).split(":", 1)[0].strip()
        lowered = key.lower()
        if "id" in lowered:
            value: Any = "sample-id"
        elif "page" in lowered or "offset" in lowered or "limit" in lowered:
            value = 1
        else:
            value = "value"
        template[key] = value
    return template


def extract_rust_functions(content: str) -> list[dict[str, Any]]:
    functions: list[dict[str, Any]] = []
    for match in RUST_FN_RE.finditer(content):
        line = _line_no(content, match.start())
        functions.append(
            {
                "name": match.group(1),
                "params": match.group(2),
                "line": line,
            }
        )

    for index, func in enumerate(functions):
        start_line = int(func["line"])
        if index + 1 < len(functions):
            next_line = int(functions[index + 1]["line"])
            func["line_end"] = max(start_line, next_line - 1)
        else:
            func["line_end"] = start_line + 1200
    return functions


def find_function_for_line(functions: list[dict[str, Any]], line_no: int) -> dict[str, Any] | None:
    for func in functions:
        if int(func["line"]) <= line_no <= int(func.get("line_end", func["line"])):
            return func
    return None


def extract_struct_definitions(content: str) -> dict[str, list[dict[str, str]]]:
    result: dict[str, list[dict[str, str]]] = {}
    for struct_match in RUST_STRUCT_RE.finditer(content):
        struct_name = struct_match.group(1)
        body = struct_match.group(2)
        fields: list[dict[str, str]] = []
        for line in body.splitlines():
            field_match = RUST_STRUCT_FIELD_RE.match(line)
            if not field_match:
                continue
            fields.append(
                {
                    "name": field_match.group(1),
                    "type": field_match.group(2).strip(),
                }
            )
        if fields:
            result[struct_name] = fields
    return result


def extract_payload_types_from_params(params: str) -> dict[str, list[str]]:
    result: dict[str, list[str]] = {"json": [], "query": [], "path": []}
    for channel, regex in RUST_EXTRACTOR_RE.items():
        for match in regex.finditer(params or ""):
            value = match.group(1).strip()
            if value:
                result[channel].append(value)
    return result


def extract_actix_endpoints(content: str) -> list[dict[str, Any]]:
    scopes = [(match.start(), match.group(1).strip()) for match in SCOPE_RE.finditer(content)]
    scopes.sort(key=lambda item: item[0])

    endpoints: list[dict[str, Any]] = []

    for route_match in ROUTE_RE.finditer(content):
        route_path = route_match.group(1).strip()
        method = route_match.group(2).upper()
        handler = route_match.group(3).strip()
        line = _line_no(content, route_match.start())
        scope_prefix = nearest_scope_prefix(scopes, route_match.start())
        full_path = join_scope_and_route(scope_prefix, route_path)
        endpoints.append(
            {
                "method": method,
                "path": full_path,
                "handler": handler,
                "line": line,
            }
        )

    for resource_match in RESOURCE_ROUTE_RE.finditer(content):
        route_path = resource_match.group(1).strip()
        method = resource_match.group(2).upper()
        handler = resource_match.group(3).strip()
        line = _line_no(content, resource_match.start())
        full_path = route_path if route_path.startswith("/") else f"/{route_path}"
        endpoints.append(
            {
                "method": method,
                "path": full_path,
                "handler": handler,
                "line": line,
            }
        )

    return endpoints


def extract_rust_api_calls(content: str) -> list[dict[str, Any]]:
    calls: list[dict[str, Any]] = []
    for match in RUST_API_CALL_RE.finditer(content):
        caller = match.group(1).strip()
        method = match.group(2).upper().strip()
        argument = match.group(3).strip()

        caller_lower = caller.lower()
        if "client" not in caller_lower and "http" not in caller_lower:
            continue

        literal_match = re.search(r"[\"']([^\"']+)[\"']", argument)
        target = literal_match.group(1).strip() if literal_match else ""
        line = _line_no(content, match.start())
        calls.append(
            {
                "method": method,
                "target": target,
                "caller": caller,
                "line": line,
            }
        )
    return calls


def extract_ts_functions(content: str) -> list[dict[str, Any]]:
    functions: list[dict[str, Any]] = []
    seen: set[tuple[str, int]] = set()

    for regex in (TS_FUNCTION_RE, TS_CONST_FN_RE):
        for match in regex.finditer(content):
            name = match.group(1)
            line = _line_no(content, match.start())
            key = (name, line)
            if key in seen:
                continue
            seen.add(key)
            # Capture `export` prefix so callers can mark the function
            # as part of the module's public API surface — the JS/TS
            # analogue of Rust's `pub fn`.
            decl_text = match.group(0)
            is_exported = decl_text.lstrip().startswith("export")
            functions.append({"name": name, "line": line, "public": is_exported})

    functions.sort(key=lambda item: int(item["line"]))
    for index, func in enumerate(functions):
        start_line = int(func["line"])
        if index + 1 < len(functions):
            next_line = int(functions[index + 1]["line"])
            func["line_end"] = max(start_line, next_line - 1)
        else:
            func["line_end"] = start_line + 2000

    return functions


def extract_ts_fetch_calls(content: str) -> list[dict[str, Any]]:
    calls: list[dict[str, Any]] = []

    for match in TS_FETCH_CALL_RE.finditer(content):
        path_hint = match.group(2).strip()
        if not path_hint:
            continue

        line = _line_no(content, match.start())
        tail = content[match.end() : match.end() + 320]
        method_match = TS_METHOD_RE.search(tail)
        method = method_match.group(1).upper().strip() if method_match else "GET"

        calls.append(
            {
                "method": method,
                "target": path_hint,
                "caller": "fetch",
                "line": line,
            }
        )

    return calls


def extract_ts_requestjson_calls(content: str) -> list[dict[str, Any]]:
    """Extract this.requestJson<T>("/path", {method: "POST"}) calls from TS SDK."""
    calls: list[dict[str, Any]] = []

    for match in TS_REQUEST_JSON_RE.finditer(content):
        # group(1) = template literal content, group(3) = quoted string content
        path_hint = (match.group(1) or match.group(3) or "").strip()
        if not path_hint:
            continue

        # Normalize template expressions: ${encodeURIComponent(sessionId)} -> {param}
        path_hint = TS_TEMPLATE_EXPR_RE.sub("{param}", path_hint)
        # Strip query string (e.g., ?principal=...)
        if "?" in path_hint:
            path_hint = path_hint.split("?", 1)[0]

        line = _line_no(content, match.start())
        tail = content[match.end() : match.end() + 320]
        method_match = TS_METHOD_RE.search(tail)
        method = method_match.group(1).upper().strip() if method_match else "GET"

        calls.append(
            {
                "method": method,
                "target": path_hint,
                "caller": "requestJson",
                "line": line,
            }
        )

    return calls


def extract_ts_classes(content: str) -> list[dict[str, Any]]:
    """Extract class definitions from TypeScript files."""
    classes: list[dict[str, Any]] = []
    for match in TS_CLASS_RE.finditer(content):
        name = match.group(1)
        line = _line_no(content, match.start())
        classes.append({"name": name, "line": line})
    return classes


def extract_python_functions(content: str) -> list[dict[str, Any]]:
    """Extract function definitions from Python files."""
    functions: list[dict[str, Any]] = []
    seen: set[tuple[str, int]] = set()

    for match in PY_FUNCTION_RE.finditer(content):
        name = match.group(1)
        line = _line_no(content, match.start())
        key = (name, line)
        if key in seen:
            continue
        seen.add(key)
        functions.append({"name": name, "line": line})

    functions.sort(key=lambda item: int(item["line"]))
    for index, func in enumerate(functions):
        start_line = int(func["line"])
        if index + 1 < len(functions):
            next_line = int(functions[index + 1]["line"])
            func["line_end"] = max(start_line, next_line - 1)
        else:
            func["line_end"] = start_line + 2000

    return functions


def extract_python_classes(content: str) -> list[dict[str, Any]]:
    """Extract class definitions from Python files."""
    classes: list[dict[str, Any]] = []
    for match in PY_CLASS_RE.finditer(content):
        name = match.group(1)
        line = _line_no(content, match.start())
        classes.append({"name": name, "line": line})
    return classes


def extract_python_curl_calls(content: str) -> list[dict[str, Any]]:
    """Extract subprocess.run(['curl', ...]) calls from Python tools."""
    calls: list[dict[str, Any]] = []

    for match in PY_CURL_CALL_RE.finditer(content):
        target = match.group(1).strip()
        if not target:
            continue

        line = _line_no(content, match.start())
        # Look for -X METHOD in the full match text
        match_text = match.group(0)
        method_match = PY_CURL_METHOD_RE.search(match_text)
        method = method_match.group(1).upper() if method_match else "GET"

        calls.append(
            {
                "method": method,
                "target": target,
                "caller": "curl",
                "line": line,
            }
        )

    # Also extract requests.get/post/etc. calls
    for match in PY_REQUESTS_CALL_RE.finditer(content):
        method = match.group(1).upper()
        target = match.group(2).strip()
        if not target:
            continue
        line = _line_no(content, match.start())
        calls.append(
            {
                "method": method,
                "target": target,
                "caller": "requests",
                "line": line,
            }
        )

    return calls


def parse_use_symbol_module_hints(content: str) -> dict[str, str]:
    hints: dict[str, str] = {}
    for match in USE_STMT_RE.finditer(content):
        expr = str(match.group(1) or "").strip()
        if not expr:
            continue

        if "::{" in expr and expr.endswith("}"):
            base, raw_items = expr.split("::{", 1)
            items = raw_items[:-1]
            base = base.strip()
            if not base:
                continue
            for raw_item in items.split(","):
                item = raw_item.strip()
                if not item or item in {"self", "*"}:
                    continue
                if " as " in item:
                    _orig, alias = item.rsplit(" as ", 1)
                    symbol_name = alias.strip()
                else:
                    symbol_name = item.split("::")[-1].strip()
                if symbol_name and symbol_name not in {"self", "*"}:
                    hints[symbol_name] = base
            continue

        symbol_name = ""
        module_hint = ""
        if " as " in expr:
            origin, alias = expr.rsplit(" as ", 1)
            symbol_name = alias.strip()
            origin = origin.strip()
            if "::" in origin:
                module_hint = origin.rsplit("::", 1)[0].strip()
        else:
            symbol_name = expr.split("::")[-1].strip()
            if "::" in expr:
                module_hint = expr.rsplit("::", 1)[0].strip()

        if symbol_name and symbol_name not in {"self", "*"}:
            hints[symbol_name] = module_hint

    return hints


def is_test_path(rel_path: str) -> bool:
    """True if the file's repo-relative path follows a test convention.
    Pure path-based — used as a structural classifier (no heuristics on
    function NAMES). Languages covered:
        Rust    — `/tests/`, `/benches/`, `/examples/` dirs are first-class
                  Cargo test/bench/example dirs.
        TS/JS   — Jest/Vitest convention: `*.test.{ts,tsx,js,jsx,mjs,cjs}`
                  and `*.spec.*`. Also `__tests__/` or `__mocks__/` dirs.
        Python  — pytest convention: `test_*.py` or `*_test.py`.
        Swift   — XCTest convention: `*Tests.swift`, plus complete
                  `*Tests` / `*UITests` target directories.
    """
    if not rel_path:
        return False
    # Cargo first-class test/bench/example dirs (any language nesting).
    for marker in ("/tests/", "/benches/", "/examples/",
                   "/__tests__/", "/__mocks__/"):
        if marker in rel_path:
            return True
    name = rel_path.rsplit("/", 1)[-1]
    # TS/JS suffixes.
    for suf in (".test.ts", ".spec.ts", ".test.tsx", ".spec.tsx",
                ".test.js", ".spec.js", ".test.mjs", ".spec.mjs",
                ".test.jsx", ".spec.jsx", ".test.cjs", ".spec.cjs"):
        if name.endswith(suf):
            return True
    # Python pytest convention.
    if name.endswith(".py") and (name.startswith("test_") or name.endswith("_test.py")):
        return True
    # Swift XCTest convention.
    if name.endswith("Tests.swift"):
        return True
    path_parts = rel_path.replace("\\", "/").split("/")[:-1]
    if any(part.endswith("Tests") or part.endswith("UITests") for part in path_parts):
        return True
    return False


def find_rust_test_function_lines(content: str) -> set[int]:
    """Return the 1-indexed lines of Rust functions that should be
    classified as tests for dead-code purposes. Covers two patterns:

    1. `#[test]` / `#[tokio::test]` / `#[actix_web::test]` annotations
       immediately preceding the `fn` declaration (skipping over other
       attributes and doc comments).
    2. Any `fn` declared inside a `#[cfg(test)] mod <name> { … }` block
       — those are test helpers used by sibling `#[test]` fns and
       would otherwise look uncalled.

    Brace-counted with simple per-character scan — accurate for clean
    Rust code; doc/string-literal braces can confuse it pathologically,
    but the worst case is over-filtering (a few non-test fns flagged),
    which is the safe direction for dead-code analysis."""
    lines = content.split("\n")
    test_lines: set[int] = set()

    # Pass 1: per-fn `#[test]`-style annotations.
    test_attr = re.compile(r"#\[(?:[a-z_]+::)*test\b")
    fn_start = re.compile(r"(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:const\s+)?(?:unsafe\s+)?fn\s+")
    for i, line in enumerate(lines):
        if not test_attr.search(line):
            continue
        for j in range(i + 1, min(i + 12, len(lines))):
            stripped = lines[j].lstrip()
            if not stripped:
                continue
            if stripped.startswith("#") or stripped.startswith("//"):
                continue
            if fn_start.match(stripped):
                test_lines.add(j + 1)
            break

    # Pass 2: `#[cfg(test)] mod <name>` blocks — all fns within count.
    cfg_test = re.compile(r"#\[cfg\(test\)\]")
    mod_open = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+[A-Za-z_]\w*\s*\{?")
    i = 0
    while i < len(lines):
        if cfg_test.search(lines[i]):
            j = i + 1
            while j < len(lines) and j - i < 6 and not mod_open.match(lines[j]):
                if cfg_test.search(lines[j]):
                    break
                j += 1
            if j < len(lines) and mod_open.match(lines[j]):
                # Brace-count from `j` to find the close.
                depth = 0
                opened = False
                k = j
                for k in range(j, len(lines)):
                    for ch in lines[k]:
                        if ch == "{":
                            depth += 1
                            opened = True
                        elif ch == "}":
                            depth -= 1
                            if opened and depth == 0:
                                break
                    if opened and depth == 0:
                        break
                if opened:
                    for m in range(j + 1, k):
                        if fn_start.search(lines[m].lstrip()):
                            test_lines.add(m + 1)
                    i = k + 1
                    continue
        i += 1

    return test_lines


def extract_rust_visibility(line: str) -> str:
    """Return the visibility prefix of a Rust `fn` declaration:
    `"pub"` for unrestricted, `"pub(crate)"` / `"pub(super)"` /
    `"pub(in path)"` for restricted, `""` for private. Used so
    dead-code queries can filter out `pub fn` (which may be called
    from outside the crate and thus look uncalled in the graph)."""
    m = RUST_FN_VISIBILITY_RE.match(line)
    return (m.group(1) or "") if m else ""


def extract_rust_impl_blocks(content: str) -> list[dict]:
    """Find every `impl [Trait for] Type { … }` block in a Rust file.
    Returns `[{"trait": str|None, "type": str, "start_line": int,
    "end_line": int, "methods": [{"name", "line"}]}]` — 1-indexed.

    Uses brace counting on a per-character sweep; reasonably accurate
    for clean Rust source. Doc-comment / string-literal braces are
    NOT stripped, so pathologically nested cases may misalign — for
    our purposes (linking impl methods to their type/trait) the
    occasional miss is acceptable."""
    lines = content.split("\n")
    blocks: list[dict] = []
    i = 0
    while i < len(lines):
        m = RUST_IMPL_RE.match(lines[i])
        if not m:
            i += 1
            continue
        trait_raw, type_raw = m.group(1), m.group(2)
        trait_name = trait_raw.split("<")[0].strip() if trait_raw else None
        type_name = type_raw.split("<")[0].strip()
        # Walk forward, counting braces. The first `{` opens the block.
        depth = 0
        opened = False
        end = i
        for j in range(i, len(lines)):
            for ch in lines[j]:
                if ch == "{":
                    depth += 1
                    opened = True
                elif ch == "}":
                    depth -= 1
                    if opened and depth == 0:
                        break
            if opened and depth == 0:
                end = j
                break
        if not opened:
            i += 1
            continue
        methods: list[dict] = []
        for k in range(i + 1, end):
            fnm = RUST_FN_VISIBILITY_RE.match(lines[k])
            if fnm:
                methods.append({"name": fnm.group(2), "line": k + 1})
        blocks.append({
            "trait": trait_name,
            "type": type_name,
            "start_line": i + 1,
            "end_line": end + 1,
            "methods": methods,
        })
        i = end + 1
    return blocks


def extract_call_sites(
    content: str,
    functions: list[dict[str, Any]],
    language: str,
) -> list[dict[str, Any]]:
    """Workspace-aware version of `extract_local_function_calls`. Emits
    every syntactic call site (`name(...)`, `path::name(...)`, plus
    Rust associated-fn calls like `Type::new(...)` via
    `RUST_QUALIFIED_CALL_RE`) WITHOUT filtering against a file-local
    function-name set.

    Cross-file resolution happens in a second pass after every file's
    symbols have been added to the workspace indices. The caller is
    responsible for routing each site's `(callee, qualifier)` through
    `resolve_function_symbol_id` (or equivalent) to attach `calls`
    edges only when resolution succeeds."""
    sites: list[dict[str, Any]] = []
    if not functions:
        return sites

    excluded = _CALL_SITE_EXCLUDED_TOKENS

    seen: set[tuple[int, str, str]] = set()
    for line_no, line in enumerate(content.splitlines(), start=1):
        owner_fn = find_function_for_line(functions, line_no)
        if owner_fn is None:
            continue

        stripped = line.strip()
        if not stripped or stripped.startswith("//"):
            continue

        if line_no == int(owner_fn.get("line", 0)):
            if language == "rust" and "fn " in stripped:
                continue
            if language == "ts" and (
                "function " in stripped
                or re.match(r"^(?:export\s+)?const\s+[A-Za-z_][A-Za-z0-9_]*\s*=", stripped)
            ):
                continue
            if language == "python" and "def " in stripped:
                continue
            if language == "swift" and re.search(r"\bfunc\s+", stripped):
                continue

        sanitized = sanitize_line_for_call_scan(line, language)
        if not sanitized.strip():
            continue

        if language == "rust":
            matchers = [
                (RUST_QUALIFIED_CALL_RE, True),    # `path::leaf(`
                (RUST_PLAIN_CALL_RE, False),       # `leaf(`
                (RUST_METHOD_CALL_RE, False),      # `recv.method(`
            ]
        elif language == "python":
            matchers = [
                (PY_PLAIN_CALL_RE, False),         # `name(`
                (PY_METHOD_CALL_RE, False),        # `obj.method(`
            ]
        else:
            matchers = [
                (TS_PLAIN_CALL_RE, False),         # `name(`
                (TS_THIS_CALL_RE, False),          # `this.name(`
                (JS_METHOD_CALL_RE, False),        # `obj.name(`
            ]

        owner_line = int(owner_fn.get("line", 0))
        for pattern, has_qualifier in matchers:
            for match in pattern.finditer(sanitized):
                if has_qualifier:
                    qualifier = match.group(1).rstrip(":")
                    callee = match.group(2)
                else:
                    qualifier = ""
                    callee = match.group(1)
                if not callee or callee in excluded:
                    continue
                key = (line_no, qualifier, callee)
                if key in seen:
                    continue
                seen.add(key)
                sites.append({
                    "owner_line": owner_line,
                    "callee": callee,
                    "qualifier": qualifier,
                    "line": line_no,
                })

    return sites


def sanitize_line_for_call_scan(line: str, language: str) -> str:
    value = str(line or "")
    if not value:
        return ""
    if language == "python":
        # Strip Python # comments
        comment_pos = value.find("#")
        if comment_pos >= 0:
            value = value[:comment_pos]
    else:
        value = value.split("//", 1)[0]
    if language == "ts":
        value = re.sub(r"/\*.*?\*/", " ", value)
    value = STRING_LITERAL_RE.sub('""', value)
    return value


_CALL_SITE_EXCLUDED_TOKENS: frozenset[str] = frozenset({
    # Rust + JS/TS keywords commonly followed by `(` that aren't calls.
    "if", "else", "for", "while", "loop", "match", "return", "let",
    "where", "fn", "async", "await", "switch", "catch", "new",
    "typeof", "instanceof", "in", "of", "yield",
    # Python keywords + a handful of stdlib names that always
    # generate false-positive matches against the loose `name(` regex.
    "def", "class", "import", "from", "with", "as", "try", "except",
    "finally", "raise", "not", "and", "or", "is", "lambda", "del",
    "pass", "break", "continue", "elif", "assert",
    "print", "len", "str", "int", "dict", "list", "set", "type",
    "bool", "range", "enumerate", "isinstance", "hasattr", "getattr",
    "setattr", "super",
})


def extract_local_function_calls(
    content: str,
    functions: list[dict[str, Any]],
    language: str,
) -> list[dict[str, Any]]:
    calls: list[dict[str, Any]] = []
    if not functions:
        return calls

    function_names = {str(func.get("name")) for func in functions if func.get("name")}
    if not function_names:
        return calls

    excluded_tokens = _CALL_SITE_EXCLUDED_TOKENS

    seen: set[tuple[int, str, int]] = set()
    for line_no, line in enumerate(content.splitlines(), start=1):
        owner_fn = find_function_for_line(functions, line_no)
        if owner_fn is None:
            continue

        stripped = line.strip()
        if not stripped or stripped.startswith("//"):
            continue

        if line_no == int(owner_fn.get("line", 0)):
            if language == "rust" and "fn " in stripped:
                continue
            if language == "ts" and (
                "function " in stripped
                or re.match(r"^(?:export\s+)?const\s+[A-Za-z_][A-Za-z0-9_]*\s*=", stripped)
            ):
                continue
            if language == "python" and "def " in stripped:
                continue

        sanitized = sanitize_line_for_call_scan(line, language)
        if not sanitized.strip():
            continue

        if language == "rust":
            match_iterables = (RUST_PLAIN_CALL_RE.finditer(sanitized), RUST_PATH_CALL_RE.finditer(sanitized))
        elif language == "python":
            match_iterables = (PY_PLAIN_CALL_RE.finditer(sanitized),)
        else:
            match_iterables = (TS_PLAIN_CALL_RE.finditer(sanitized), TS_THIS_CALL_RE.finditer(sanitized))

        owner_name = str(owner_fn.get("name", ""))
        owner_line = int(owner_fn.get("line", 0))
        for matcher in match_iterables:
            for match in matcher:
                callee = match.group(1)
                if not callee or callee in excluded_tokens:
                    continue
                if callee not in function_names:
                    continue

                key = (owner_line, callee, line_no)
                if key in seen:
                    continue
                seen.add(key)
                calls.append(
                    {
                        "owner": owner_name,
                        "owner_line": owner_line,
                        "callee": callee,
                        "line": line_no,
                    }
                )

    return calls


def match_endpoint_ids(
    method: str,
    call_path: str,
    endpoint_index: dict[str, list[dict[str, Any]]],
) -> list[str]:
    normalized = normalize_api_path(call_path)
    if not normalized:
        return []

    candidates = endpoint_index.get(method.upper(), [])
    exact: list[str] = []
    wildcard: list[str] = []

    for item in candidates:
        endpoint_path = item.get("path", "")
        endpoint_id = item.get("id", "")
        pattern = item.get("pattern")
        if not endpoint_id or not endpoint_path:
            continue
        if endpoint_path == normalized:
            exact.append(endpoint_id)
            continue
        if isinstance(pattern, re.Pattern) and pattern.match(normalized):
            wildcard.append(endpoint_id)

    if exact:
        return exact[:3]
    return wildcard[:3]


def resolve_schema_template(
    type_name: str,
    crate_name: str,
    struct_templates_by_crate_name: dict[tuple[str, str], dict[str, Any]],
    struct_templates_by_name: dict[str, dict[str, Any]],
    struct_nodes_by_crate_name: dict[tuple[str, str], list[str]],
    struct_nodes_by_name: dict[str, list[str]],
    enum_templates_by_crate_name: dict[tuple[str, str], dict[str, Any]],
    enum_templates_by_name: dict[str, dict[str, Any]],
    enum_nodes_by_crate_name: dict[tuple[str, str], list[str]],
    enum_nodes_by_name: dict[str, list[str]],
) -> tuple[dict[str, Any], str | None, str]:
    base = base_type_name(type_name)
    if not base:
        return {}, None, ""

    primitive_like = {
        "String",
        "str",
        "bool",
        "i8",
        "i16",
        "i32",
        "i64",
        "i128",
        "isize",
        "u8",
        "u16",
        "u32",
        "u64",
        "u128",
        "usize",
        "f32",
        "f64",
        "Value",
        "tuple",
    }

    if base in primitive_like:
        return {"value": sample_value_for_type(type_name)}, None, base

    template = struct_templates_by_crate_name.get((crate_name, base))
    if template is None:
        template = struct_templates_by_name.get(base)
    if template is None:
        template = enum_templates_by_crate_name.get((crate_name, base))
    if template is None:
        template = enum_templates_by_name.get(base)

    node_id = None
    nodes_by_name = struct_nodes_by_crate_name.get((crate_name, base), [])
    if nodes_by_name:
        node_id = nodes_by_name[0]
    elif struct_nodes_by_name.get(base):
        node_id = struct_nodes_by_name[base][0]
    elif enum_nodes_by_crate_name.get((crate_name, base)):
        node_id = enum_nodes_by_crate_name[(crate_name, base)][0]
    elif enum_nodes_by_name.get(base):
        node_id = enum_nodes_by_name[base][0]

    if template is None:
        def resolver(type_hint: str) -> Any:
            lookup = base_type_name(type_hint)
            if not lookup:
                return None
            local_struct = struct_templates_by_crate_name.get((crate_name, lookup))
            if local_struct is not None:
                return local_struct
            shared_struct = struct_templates_by_name.get(lookup)
            if shared_struct is not None:
                return shared_struct
            local_enum = enum_templates_by_crate_name.get((crate_name, lookup))
            if local_enum is not None:
                return local_enum
            return enum_templates_by_name.get(lookup)

        return {"value": sample_value_for_type(type_name, template_resolver=resolver)}, node_id, base

    return dict(template), node_id, base


def generate(root: Path) -> tuple[dict[str, Any], dict[str, Any], dict[str, Any]]:
    prog = Progress(total=count_source_files(root))
    prog.set_phase("loading cargo metadata")

    # Pre-compute the set of every project root in the repo so each
    # per-project scanner can prune subtrees that belong to a nested
    # project — otherwise the same file is walked (and ticked) by
    # multiple scanners (e.g. a Node project containing a Chrome-ext
    # manifest, a Rust crate containing a nested Cargo workspace).
    # `add_node` is idempotent so this didn't create duplicate graph
    # nodes, but it inflated the progress denominator past 100%.
    all_project_roots: set[Path] = set()
    swift_projects = discover_swift_projects(root)
    for manifest_name in ("Cargo.toml", "package.json", "Package.swift",
                          "project.yml", "pyproject.toml", "setup.py"):
        for manifest in _walk_for_manifests(root, manifest_name):
            all_project_roots.add(manifest.parent.resolve())
    for manifest in _walk_for_manifests(root, "manifest.json"):
        try:
            data = json.loads(manifest.read_text(encoding="utf-8", errors="ignore"))
        except (ValueError, OSError):
            continue
        if isinstance(data, dict) and "manifest_version" in data:
            all_project_roots.add(manifest.parent.resolve())
    all_project_roots_frozen: frozenset[Path] = frozenset(all_project_roots)

    metadata_raw = run(["cargo", "metadata", "--format-version", "1", "--no-deps"], cwd=root)
    metadata = json.loads(metadata_raw)

    try:
        commit = run(["git", "rev-parse", "HEAD"], cwd=root).strip()
    except Exception:
        commit = "unknown"

    packages = metadata.get("packages", [])
    workspace_members = set(metadata.get("workspace_members", []))
    workspace_packages = [pkg for pkg in packages if pkg.get("id") in workspace_members]

    # `cargo metadata` only enumerates the main workspace. Crates that
    # live under sibling Cargo workspaces (e.g. `desktop/src-tauri`,
    # which keeps its own Cargo.lock for tauri-builder isolation) are
    # invisible to it. Auto-discover them by walking the tree for any
    # `Cargo.toml` that's NOT already in the workspace, parsing
    # `[package].name` + `version`, and injecting a pseudo-package
    # entry. The existing Rust scan loop picks them up uniformly.
    #
    # Discovery is preferred over a hardcoded allow-list so that a new
    # sibling workspace gets indexed automatically — no per-crate
    # plumbing here. `discover_out_of_workspace_rust_crates` excludes
    # build/dep directories so vendored deps and tauri build outputs
    # don't pollute the graph.
    for pkg in discover_out_of_workspace_rust_crates(root, workspace_packages):
        workspace_packages.append(pkg)

    workspace_crates = {pkg["name"] for pkg in workspace_packages}

    nodes: dict[str, dict[str, Any]] = {}
    edge_counts: dict[tuple[str, str, str], int] = defaultdict(int)

    struct_nodes_by_crate_name: dict[tuple[str, str], list[str]] = defaultdict(list)
    struct_nodes_by_name: dict[str, list[str]] = defaultdict(list)
    enum_nodes_by_crate_name: dict[tuple[str, str], list[str]] = defaultdict(list)
    enum_nodes_by_name: dict[str, list[str]] = defaultdict(list)
    function_nodes_by_file_name: dict[tuple[str, str], list[str]] = defaultdict(list)
    function_nodes_by_file_line: dict[tuple[str, int], str] = {}
    function_nodes_by_crate_name: dict[tuple[str, str], list[str]] = defaultdict(list)
    function_nodes_by_name: dict[str, list[str]] = defaultdict(list)
    function_params_by_symbol_id: dict[str, str] = {}
    rust_functions_by_file: dict[str, list[dict[str, Any]]] = {}
    struct_templates_by_crate_name: dict[tuple[str, str], dict[str, Any]] = {}
    struct_templates_by_name: dict[str, dict[str, Any]] = {}
    enum_templates_by_crate_name: dict[tuple[str, str], dict[str, Any]] = {}
    enum_templates_by_name: dict[str, dict[str, Any]] = {}
    endpoint_index_by_method: dict[str, list[dict[str, Any]]] = defaultdict(list)
    payload_profiles: list[dict[str, Any]] = []
    payload_profile_by_source_id: dict[str, dict[str, Any]] = {}
    unresolved_handlers: list[dict[str, str]] = []
    # Cross-file call resolution is deferred until every file's
    # symbols have been added to the workspace indices. Each pending
    # entry carries the caller's symbol id + the raw callee name and
    # any qualifier (path prefix or use-statement hint) so
    # `resolve_function_symbol_id` can search file → crate → global.
    pending_rust_calls: list[dict[str, Any]] = []
    pending_ts_calls: list[dict[str, Any]] = []
    pending_js_calls: list[dict[str, Any]] = []
    pending_swift_calls: list[dict[str, Any]] = []
    # Impl-block resolution: each entry is `{method_id, trait_name,
    # method_name, crate}` — at the end of the run we look up the
    # trait in the workspace and emit `implements` edges. Also drives
    # `defines` edges from struct/enum nodes to their method symbols.
    pending_impl_methods: list[dict[str, Any]] = []

    def add_node(node_id: str, kind: str, label: str, **attrs: Any) -> None:
        if node_id in nodes:
            existing = nodes[node_id]
            for key, value in attrs.items():
                if value is None:
                    continue
                if key not in existing:
                    existing[key] = value
            return
        node = {"id": node_id, "kind": kind, "label": label}
        node.update({k: v for k, v in attrs.items() if v is not None})
        nodes[node_id] = node

    def add_edge(src: str, dst: str, kind: str) -> None:
        if src == dst:
            return
        edge_counts[(src, dst, kind)] += 1

    def unique_symbol_id(candidates: list[str]) -> str | None:
        deduped = list(dict.fromkeys(candidates))
        if len(deduped) == 1:
            return deduped[0]
        return None

    def normalize_module_hint(raw_hint: str) -> str:
        hint = str(raw_hint or "").strip(":")
        if not hint:
            return ""
        parts = [part for part in hint.split("::") if part]
        if not parts:
            return ""
        if parts[0] in {"self", "super"}:
            parts = parts[1:]
        if parts and parts[0] == "crate":
            parts = parts[1:]
        if not parts:
            return ""
        if parts[0] and parts[0][0].isupper():
            return ""
        return "::".join(parts)

    def filter_candidates_by_module_hint(candidates: list[str], hint: str) -> list[str]:
        module_hint = normalize_module_hint(hint)
        deduped = list(dict.fromkeys(candidates))
        if not module_hint:
            return deduped

        filtered = []
        for symbol_id in deduped:
            module_value = str(nodes.get(symbol_id, {}).get("module", "")).strip()
            if not module_value:
                continue
            if (
                module_value == module_hint
                or module_value.endswith(f"::{module_hint}")
                or module_value.endswith(module_hint)
            ):
                filtered.append(symbol_id)

        return filtered if filtered else deduped

    def resolve_function_symbol_id(
        handler_expr: str,
        rel_path: str,
        crate: str,
        module_hint: str = "",
    ) -> str | None:
        expr = str(handler_expr or "").strip()
        if not expr:
            return None

        qualifier = ""
        symbol_name = expr
        if "::" in expr:
            qualifier, symbol_name = expr.rsplit("::", 1)
        symbol_name = symbol_name.strip()
        if not qualifier:
            qualifier = str(module_hint or "").strip()
        if not symbol_name:
            return None

        name_variants = [symbol_name]

        for name_variant in name_variants:
            file_candidates = filter_candidates_by_module_hint(
                function_nodes_by_file_name.get((rel_path, name_variant), []),
                qualifier,
            )
            file_unique = unique_symbol_id(file_candidates)
            if file_unique:
                return file_unique
            if file_candidates:
                return None

            crate_candidates = filter_candidates_by_module_hint(
                function_nodes_by_crate_name.get((crate, name_variant), []),
                qualifier,
            )
            crate_unique = unique_symbol_id(crate_candidates)
            if crate_unique:
                return crate_unique
            if crate_candidates:
                return None

            global_candidates = filter_candidates_by_module_hint(
                function_nodes_by_name.get(name_variant, []),
                qualifier,
            )
            global_unique = unique_symbol_id(global_candidates)
            if global_unique:
                return global_unique
            if global_candidates:
                return None

        return None

    # Crate nodes + dependency edges
    pkg_by_name = {pkg["name"]: pkg for pkg in workspace_packages}
    for pkg in workspace_packages:
        crate_name = pkg["name"]
        crate_id = f"crate::{crate_name}"
        manifest = Path(pkg["manifest_path"])
        add_node(
            crate_id,
            "crate",
            crate_name,
            version=pkg.get("version"),
            path=relpath(manifest, root),
        )

        for dep in pkg.get("dependencies", []):
            dep_name = dep.get("name")
            if dep_name in workspace_crates:
                add_edge(crate_id, f"crate::{dep_name}", "depends_on")

    # Source-level graph for Rust files. We index whatever `.rs` files
    # live under the crate root (with build outputs / vendored deps
    # pruned by `_walk_pruned`). No special-casing of `src/`: Cargo's
    # canonical dirs (`tests/`, `benches/`, `examples/`, `build.rs`)
    # all get scanned uniformly, and so does anything else a crate
    # author drops in (e.g. `xtask/`, custom test harnesses).
    # `derive_module_path` handles the module-name conventions
    # (`src/` is stripped, `lib.rs`/`main.rs` collapse to the crate
    # root, etc.) so module IDs stay stable.
    prog.set_phase("indexing Rust sources")
    for crate_name, pkg in pkg_by_name.items():
        crate_id = f"crate::{crate_name}"
        manifest = Path(pkg["manifest_path"])
        crate_root = manifest.parent
        rust_files = sorted(
            p for p in _walk_pruned(crate_root, skip_subroots=all_project_roots_frozen)
            if p.suffix == ".rs"
        )
        if not rust_files:
            continue

        for file_path in rust_files:
            rel = relpath(file_path, root)
            prog.tick(file=rel)
            file_id = f"file::{rel}"
            add_node(file_id, "file", file_path.name, path=rel, crate=crate_name)
            add_edge(crate_id, file_id, "contains")

            module_path = derive_module_path(crate_name, file_path.relative_to(crate_root))
            module_id = f"module::{module_path}"
            add_node(module_id, "module", module_path, path=rel, crate=crate_name, module=module_path)
            add_edge(crate_id, module_id, "contains")
            add_edge(module_id, file_id, "contains")

            try:
                content = file_path.read_text(encoding="utf-8", errors="ignore")
            except Exception:
                continue

            use_symbol_hints = parse_use_symbol_module_hints(content)
            rust_functions = extract_rust_functions(content)
            rust_test_lines = find_rust_test_function_lines(content)
            rust_functions_by_file[rel] = rust_functions
            rust_function_by_line: dict[int, dict[str, Any]] = {
                int(fn["line"]): fn for fn in rust_functions if "line" in fn
            }

            struct_symbol_id_by_name: dict[str, str] = {}

            for lineno, line in enumerate(content.splitlines(), start=1):
                for sym_kind, pattern in SYMBOL_PATTERNS:
                    match = pattern.match(line)
                    if not match:
                        continue
                    name = match.group(1)
                    symbol_id = f"symbol::{rel}::{sym_kind}::{name}::{lineno}"
                    function_params = None
                    visibility = ""
                    is_test = False
                    if sym_kind == "function":
                        fn_meta = rust_function_by_line.get(lineno)
                        if fn_meta is not None:
                            params_raw = str(fn_meta.get("params", "")).strip()
                            if params_raw:
                                function_params = params_raw
                        visibility = extract_rust_visibility(line)
                        # Three structural signals: annotated as `#[test]`,
                        # inside a `#[cfg(test)] mod`, OR in a Cargo
                        # `tests/`/`benches/`/`examples/` tree.
                        is_test = (lineno in rust_test_lines) or is_test_path(rel)
                    add_node(
                        symbol_id,
                        sym_kind,
                        name,
                        path=rel,
                        crate=crate_name,
                        module=module_path,
                        line=lineno,
                        params=function_params,
                        visibility=visibility or None,
                        public=(visibility == "pub") if sym_kind == "function" else None,
                        test=is_test if sym_kind == "function" else None,
                    )
                    add_edge(file_id, symbol_id, "defines")
                    add_edge(module_id, symbol_id, "contains")

                    if sym_kind == "function":
                        function_nodes_by_file_name[(rel, name)].append(symbol_id)
                        function_nodes_by_file_line[(rel, lineno)] = symbol_id
                        function_nodes_by_crate_name[(crate_name, name)].append(symbol_id)
                        function_nodes_by_name[name].append(symbol_id)
                        if function_params:
                            function_params_by_symbol_id[symbol_id] = function_params
                    elif sym_kind == "struct":
                        struct_symbol_id_by_name[name] = symbol_id
                        struct_nodes_by_crate_name[(crate_name, name)].append(symbol_id)
                        struct_nodes_by_name[name].append(symbol_id)
                    elif sym_kind == "enum":
                        enum_nodes_by_crate_name[(crate_name, name)].append(symbol_id)
                        enum_nodes_by_name[name].append(symbol_id)
                    break

                use_match = USE_RE.match(line)
                if use_match:
                    imported = use_match.group(1)
                    if imported in workspace_crates:
                        add_edge(file_id, f"crate::{imported}", "references")

            struct_defs = extract_struct_definitions(content)
            for struct_name, fields in struct_defs.items():
                template = build_struct_template(fields)
                struct_templates_by_crate_name[(crate_name, struct_name)] = template
                struct_templates_by_name.setdefault(struct_name, template)

                symbol_id = struct_symbol_id_by_name.get(struct_name)
                if symbol_id:
                    add_node(symbol_id, "struct", struct_name, fields=fields, mock_payload=template)

            enum_defs = extract_enum_definitions(content)
            for enum_name, variants in enum_defs.items():
                template = build_enum_template(variants)
                enum_templates_by_crate_name[(crate_name, enum_name)] = template
                enum_templates_by_name.setdefault(enum_name, template)

                enum_node_ids = enum_nodes_by_crate_name.get((crate_name, enum_name), [])
                if enum_node_ids:
                    add_node(
                        enum_node_ids[0],
                        "enum",
                        enum_name,
                        variants=variants,
                        mock_payload=template,
                    )

            # Capture every call site (cross-file resolution deferred).
            for site in extract_call_sites(content, rust_functions, "rust"):
                owner_id = function_nodes_by_file_line.get((rel, int(site["owner_line"])))
                if not owner_id:
                    continue
                callee = str(site["callee"])
                qualifier = str(site.get("qualifier") or "")
                if not qualifier:
                    qualifier = use_symbol_hints.get(callee, "")
                pending_rust_calls.append({
                    "owner_id": owner_id,
                    "callee": callee,
                    "qualifier": qualifier,
                    "file_rel": rel,
                    "crate": crate_name,
                })

            # Impl-block scan: each `impl [Trait for] Type { … }` block
            # links its methods to the type (`defines` edge) and, when
            # the trait is in-workspace, to the trait (`implements` edge).
            for block in extract_rust_impl_blocks(content):
                type_name = block.get("type") or ""
                trait_name = block.get("trait")
                # Resolve the receiver type to a struct/enum node when
                # one exists in this crate. `defines` edges give queries
                # like "what methods does Foo have?".
                type_candidates = (
                    struct_nodes_by_crate_name.get((crate_name, type_name), [])
                    or struct_nodes_by_name.get(type_name, [])
                )
                type_id = unique_symbol_id(type_candidates)
                if not type_id:
                    enum_candidates = (
                        enum_nodes_by_crate_name.get((crate_name, type_name), [])
                        or enum_nodes_by_name.get(type_name, [])
                    )
                    type_id = unique_symbol_id(enum_candidates)
                for method in block.get("methods", []):
                    method_line = int(method.get("line", 0))
                    method_symbol_id = function_nodes_by_file_line.get((rel, method_line))
                    if not method_symbol_id:
                        continue
                    if type_id:
                        add_edge(type_id, method_symbol_id, "defines")
                    if trait_name:
                        pending_impl_methods.append({
                            "method_id": method_symbol_id,
                            "method_name": method.get("name"),
                            "trait_name": trait_name,
                            "crate": crate_name,
                        })

            endpoints = extract_actix_endpoints(content)
            for endpoint_idx, endpoint in enumerate(endpoints, start=1):
                method = str(endpoint["method"]).upper()
                route_path = str(endpoint["path"] or "/")
                handler_name = str(endpoint["handler"] or "")
                line = int(endpoint["line"])
                endpoint_id = f"endpoint::{method}::{route_path}::{rel}::{line}:{endpoint_idx}"
                endpoint_label = f"{method} {route_path}"

                add_node(
                    endpoint_id,
                    "endpoint",
                    endpoint_label,
                    method=method,
                    route=route_path,
                    handler=handler_name,
                    path=rel,
                    crate=crate_name,
                    module=module_path,
                    line=line,
                )
                add_edge(file_id, endpoint_id, "contains")
                add_edge(module_id, endpoint_id, "contains")

                handler_lookup_name = handler_name.rsplit("::", 1)[-1] if handler_name else ""
                handler_module_hint = use_symbol_hints.get(handler_lookup_name, "")
                handler_symbol_id = resolve_function_symbol_id(
                    handler_name,
                    rel,
                    crate_name,
                    module_hint=handler_module_hint,
                )
                if handler_symbol_id:
                    add_edge(endpoint_id, handler_symbol_id, "handles")
                elif handler_name:
                    unresolved_handlers.append(
                        {
                            "endpoint_id": endpoint_id,
                            "handler_name": handler_name,
                            "handler_module_hint": handler_module_hint,
                            "rel_path": rel,
                            "crate_name": crate_name,
                            "route_path": route_path,
                        }
                    )

                payload_template = {
                    "path": build_path_param_template(route_path),
                    "query": {},
                    "json": {},
                }
                schema_refs: dict[str, str] = {}
                handler_params = ""
                if handler_symbol_id and handler_symbol_id in function_params_by_symbol_id:
                    handler_params = str(function_params_by_symbol_id[handler_symbol_id])
                elif handler_name:
                    handler_variants = [handler_name]
                    if "::" in handler_name:
                        handler_variants.append(handler_name.rsplit("::", 1)[-1])
                    for fn in rust_functions:
                        fn_name = str(fn.get("name", ""))
                        if fn_name in handler_variants:
                            handler_params = str(fn.get("params", "")).strip()
                            if handler_params:
                                break

                if handler_params:
                    payload_types = extract_payload_types_from_params(handler_params)
                    for channel in ("path", "query", "json"):
                        if not payload_types[channel]:
                            continue
                        template, struct_node_id, schema_name = resolve_schema_template(
                            payload_types[channel][0],
                            crate_name,
                            struct_templates_by_crate_name,
                            struct_templates_by_name,
                            struct_nodes_by_crate_name,
                            struct_nodes_by_name,
                            enum_templates_by_crate_name,
                            enum_templates_by_name,
                            enum_nodes_by_crate_name,
                            enum_nodes_by_name,
                        )
                        if channel != "path" or not payload_template["path"]:
                            payload_template[channel] = template
                        if schema_name:
                            schema_refs[channel] = schema_name
                        if struct_node_id:
                            add_edge(endpoint_id, struct_node_id, "accepts_payload")

                payload_profile = {
                    "source_id": endpoint_id,
                    "kind": "endpoint",
                    "method": method,
                    "route": route_path,
                    "handler": handler_name,
                    "path": rel,
                    "line": line,
                    "schema_refs": schema_refs,
                    "payload_template": payload_template,
                }
                payload_profiles.append(payload_profile)
                payload_profile_by_source_id[endpoint_id] = payload_profile

                normalized_route = normalize_api_path(route_path)
                if normalized_route:
                    endpoint_index_by_method[method].append(
                        {
                            "id": endpoint_id,
                            "path": normalized_route,
                            "pattern": endpoint_path_regex(normalized_route),
                        }
                    )

            rust_calls = extract_rust_api_calls(content)
            for call_idx, call in enumerate(rust_calls, start=1):
                method = str(call["method"]).upper()
                target = str(call["target"])
                line = int(call["line"])
                caller = str(call["caller"])
                call_id = f"api_call::{rel}::{method.lower()}::{line}:{call_idx}"
                label = f"{method} {target or '<dynamic>'}"

                add_node(
                    call_id,
                    "api_call",
                    label,
                    method=method,
                    target=target,
                    caller=caller,
                    path=rel,
                    crate=crate_name,
                    module=module_path,
                    line=line,
                )
                add_edge(file_id, call_id, "contains")
                add_edge(module_id, call_id, "contains")

                owner_fn = find_function_for_line(rust_functions, line)
                if owner_fn:
                    owner_id = function_nodes_by_file_line.get((rel, int(owner_fn.get("line", 0))))
                    if owner_id:
                        add_edge(owner_id, call_id, "calls_api")

                for endpoint_id in match_endpoint_ids(method, target, endpoint_index_by_method):
                    add_edge(call_id, endpoint_id, "targets_endpoint")

        emit_metadata_files_for_project(
            crate_root, root, crate_id, crate_name, add_node, add_edge,
        )

    # ── Node / TS / JS / Svelte / Vue / static-asset projects ───────
    # Auto-discovered: any `package.json` with a sibling `src/`
    # directory mints a virtual crate. Scans `.ts/.tsx/.jsx/.js/.mjs/
    # .cjs/.svelte/.vue/.html/.css/.scss` recursively. Code-bearing
    # files get function/class/api-call extraction; static assets
    # (.html/.css/.scss) land as file-only nodes for navigation.
    prog.set_phase("indexing Node / TS / Svelte sources")
    for ui_crate_name, ui_proj_root, ui_src in discover_node_projects(root):
        crate_id = f"crate::{ui_crate_name}"
        workspace_crates.add(ui_crate_name)
        add_node(crate_id, "crate", ui_crate_name, path=relpath(ui_proj_root, root))

        for file_path in collect_node_source_files(
            ui_src, skip_subroots=all_project_roots_frozen,
        ):
            rel = relpath(file_path, root)
            prog.tick(file=rel)
            file_id = f"file::{rel}"
            add_node(file_id, "file", file_path.name, path=rel, crate=ui_crate_name)
            add_edge(crate_id, file_id, "contains")

            file_rel_to_ui = file_path.relative_to(ui_src)
            module_path = derive_ui_module_path(ui_crate_name, file_rel_to_ui)
            module_id = f"module::{module_path}"
            add_node(module_id, "module", module_path, path=rel, crate=ui_crate_name, module=module_path)
            add_edge(crate_id, module_id, "contains")
            add_edge(module_id, file_id, "contains")

            # Static assets (.html / .css / .scss) get the file +
            # module nodes above (useful for navigation + dependency
            # tracking) but no symbol extraction — they have no
            # function-like declarations to surface in cgraph_search.
            if file_path.suffix not in NODE_SYMBOL_EXTENSIONS:
                continue

            try:
                raw_content = file_path.read_text(encoding="utf-8", errors="ignore")
            except Exception:
                continue

            # Single-file-component formats (`.svelte`, `.vue`) embed
            # their script inside a `<script>` block. Pull just that
            # block (line numbers preserved) before handing to the TS
            # extractors. Pure-markup components emit zero symbols.
            if file_path.suffix in {".svelte", ".vue"}:
                content = extract_svelte_script(raw_content)
                if not content.strip():
                    continue
            else:
                content = raw_content

            ts_functions = extract_ts_functions(content)
            ts_function_by_name: dict[str, list[str]] = defaultdict(list)
            ts_function_by_line: dict[int, str] = {}
            ts_file_is_test = is_test_path(rel)
            for fn in ts_functions:
                fn_name = str(fn["name"])
                line = int(fn["line"])
                symbol_id = f"symbol::{rel}::function::{fn_name}::{line}"
                add_node(
                    symbol_id,
                    "function",
                    fn_name,
                    path=rel,
                    crate=ui_crate_name,
                    module=module_path,
                    line=line,
                    public=bool(fn.get("public")),
                    test=ts_file_is_test or None,
                )
                add_edge(file_id, symbol_id, "defines")
                add_edge(module_id, symbol_id, "contains")
                ts_function_by_name[fn_name].append(symbol_id)
                ts_function_by_line[line] = symbol_id
                # Shared workspace indices so deferred cross-file
                # resolution can find this function from any caller.
                function_nodes_by_file_name[(rel, fn_name)].append(symbol_id)
                function_nodes_by_file_line[(rel, line)] = symbol_id
                function_nodes_by_crate_name[(ui_crate_name, fn_name)].append(symbol_id)
                function_nodes_by_name[fn_name].append(symbol_id)

            for site in extract_call_sites(content, ts_functions, "ts"):
                owner_id = ts_function_by_line.get(int(site["owner_line"]))
                if not owner_id:
                    continue
                pending_ts_calls.append({
                    "owner_id": owner_id,
                    "callee": str(site["callee"]),
                    "file_rel": rel,
                    "crate": ui_crate_name,
                })

            # Both `fetch(...)` and `requestJson(...)` (a common SDK
            # wrapper idiom) resolve to outbound HTTP calls; emit
            # `api_call` nodes for both flavours and link them to
            # matching endpoint nodes for `targets_endpoint` edges.
            # Projects that wrap fetch under another name (e.g.
            # `timedFetch`) are still picked up by the fetch extractor.
            api_call_sources = [
                ("fetch", extract_ts_fetch_calls(content)),
                ("requestJson", extract_ts_requestjson_calls(content)),
            ]
            for caller_label, ts_calls in api_call_sources:
                for call_idx, call in enumerate(ts_calls, start=1):
                    method = str(call["method"]).upper()
                    target = str(call["target"])
                    line = int(call["line"])
                    call_id = f"api_call::{rel}::{caller_label}::{method.lower()}::{line}:{call_idx}"
                    label = f"{method} {target or '<dynamic>'}"

                    add_node(
                        call_id,
                        "api_call",
                        label,
                        method=method,
                        target=target,
                        caller=caller_label,
                        path=rel,
                        crate=ui_crate_name,
                        module=module_path,
                        line=line,
                    )
                    add_edge(file_id, call_id, "contains")
                    add_edge(module_id, call_id, "contains")

                    owner_fn = find_function_for_line(ts_functions, line)
                    if owner_fn:
                        owner_id = ts_function_by_line.get(int(owner_fn.get("line", 0)))
                        if owner_id:
                            add_edge(owner_id, call_id, "calls_api")

                    for endpoint_id in match_endpoint_ids(method, target, endpoint_index_by_method):
                        add_edge(call_id, endpoint_id, "targets_endpoint")

        emit_metadata_files_for_project(
            ui_proj_root, root, crate_id, ui_crate_name, add_node, add_edge,
        )

    # ── Chrome extensions (JS) ───────────────────────────────────────
    # Auto-discovered: any `manifest.json` carrying a
    # `manifest_version` field marks a Chrome / MV3 extension root.
    # Project name derives from the directory, with the parent dir
    # prefixed when the directory name is generic (`extension`,
    # `ext`, `addon`) to avoid collisions. Scans every source-suffix
    # file under the extension root — Chrome MV3 has no canonical
    # `src/` subdir, so the project root IS the scan root. `.js` /
    # `.ts` / `.mjs` / `.cjs` get full symbol extraction; `.html` /
    # `.css` / `.svelte` / `.vue` land as file-only nodes for
    # navigation (extensions bundle popup pages + content scripts
    # alongside JS).
    prog.set_phase("indexing Chrome extensions")
    for ext_crate_name, ext_src in discover_chrome_extensions(root):
        ext_crate_id = f"crate::{ext_crate_name}"
        workspace_crates.add(ext_crate_name)
        add_node(ext_crate_id, "crate", ext_crate_name, path=relpath(ext_src, root))

        for file_path in collect_node_source_files(
            ext_src, skip_subroots=all_project_roots_frozen,
        ):
            file_rel = relpath(file_path, root)
            prog.tick(file=file_rel)
            file_rel_to_src = file_path.relative_to(ext_src)
            module_path = derive_ui_module_path(ext_crate_name, file_rel_to_src)

            file_id = f"file::{file_rel}"
            module_id = f"module::{module_path}"

            add_node(file_id, "file", file_path.name, path=file_rel, crate=ext_crate_name, module=module_path)
            add_node(module_id, "module", module_path, path=file_rel, crate=ext_crate_name, module=module_path)
            add_edge(ext_crate_id, file_id, "contains")
            add_edge(ext_crate_id, module_id, "contains")
            add_edge(module_id, file_id, "contains")

            # Static-asset suffixes (.html/.css/.svelte/.vue) get no
            # symbol extraction — they're navigable as file nodes only.
            if file_path.suffix not in {".js", ".mjs", ".cjs", ".ts", ".tsx", ".jsx"}:
                continue

            try:
                content = file_path.read_text(encoding="utf-8", errors="replace")
            except Exception:
                continue

            # Functions (function declarations + const arrow assignments).
            js_functions = extract_ts_functions(content)
            js_function_by_name: dict[str, list[str]] = defaultdict(list)
            js_function_by_line: dict[int, str] = {}
            for fn in js_functions:
                fn_name = str(fn.get("name", ""))
                fn_line = int(fn.get("line", 0))
                symbol_id = f"symbol::{file_rel}::function::{fn_name}::{fn_line}"
                add_node(
                    symbol_id, "function", fn_name,
                    path=file_rel, crate=ext_crate_name, module=module_path, line=fn_line,
                    public=bool(fn.get("public")),
                    test=is_test_path(file_rel) or None,
                )
                add_edge(file_id, symbol_id, "defines")
                add_edge(module_id, symbol_id, "contains")
                js_function_by_name[fn_name].append(symbol_id)
                js_function_by_line[fn_line] = symbol_id
                function_nodes_by_file_name[(file_rel, fn_name)].append(symbol_id)
                function_nodes_by_file_line[(file_rel, fn_line)] = symbol_id
                function_nodes_by_crate_name[(ext_crate_name, fn_name)].append(symbol_id)
                function_nodes_by_name[fn_name].append(symbol_id)

            # ES classes (modeled as struct nodes for parity with the bot scan).
            js_classes = extract_ts_classes(content)
            for cls in js_classes:
                cls_name = str(cls.get("name", ""))
                cls_line = int(cls.get("line", 0))
                cls_id = f"symbol::{file_rel}::struct::{cls_name}::{cls_line}"
                add_node(
                    cls_id, "struct", cls_name,
                    path=file_rel, crate=ext_crate_name, module=module_path, line=cls_line,
                )
                add_edge(file_id, cls_id, "defines")
                add_edge(module_id, cls_id, "contains")

            for site in extract_call_sites(content, js_functions, "ts"):
                owner_id = js_function_by_line.get(int(site["owner_line"]))
                if not owner_id:
                    continue
                pending_js_calls.append({
                    "owner_id": owner_id,
                    "callee": str(site["callee"]),
                    "file_rel": rel,
                    "crate": ext_crate_name,
                })

            # fetch() calls — extensions commonly talk to a host HTTP
            # service, so wire these to matching endpoint nodes when
            # the URL resolves to one.
            js_fetch = extract_ts_fetch_calls(content)
            for call_idx, call in enumerate(js_fetch, start=1):
                method = str(call.get("method", "GET")).upper()
                target = str(call.get("target", ""))
                line = int(call.get("line", 0))
                call_id = f"api_call::{file_rel}::{method.lower()}::{line}:{call_idx}"
                add_node(
                    call_id, "api_call", f"{method} {target or '<dynamic>'}",
                    method=method, target=target, caller="fetch",
                    path=file_rel, crate=ext_crate_name, module=module_path, line=line,
                )
                add_edge(file_id, call_id, "contains")
                add_edge(module_id, call_id, "contains")
                owner_fn = find_function_for_line(js_functions, line)
                if owner_fn:
                    owner_id = js_function_by_line.get(int(owner_fn.get("line", 0)))
                    if owner_id:
                        add_edge(owner_id, call_id, "calls_api")
                for endpoint_id in match_endpoint_ids(method, target, endpoint_index_by_method):
                    add_edge(call_id, endpoint_id, "targets_endpoint")

        emit_metadata_files_for_project(
            ext_src, root, ext_crate_id, ext_crate_name, add_node, add_edge,
        )

    # ── SwiftPM + XcodeGen Swift projects ────────────────────────────
    # Includes native macOS helper packages and application projects such as
    # Magios. Walk each complete project root so app, extension, widget,
    # shared-source, unit-test, and UI-test targets remain in one crate.
    prog.set_phase("indexing Swift projects")
    for swift_crate_name, swift_root in swift_projects:
        swift_rel_dir = relpath(swift_root, root)
        swift_crate_id = f"crate::{swift_crate_name}"
        workspace_crates.add(swift_crate_name)
        add_node(swift_crate_id, "crate", swift_crate_name, path=swift_rel_dir)

        for file_path in sorted(
            p for p in _walk_pruned(swift_root, skip_subroots=all_project_roots_frozen)
            if p.suffix == ".swift"
        ):
            rel = relpath(file_path, root)
            prog.tick(file=rel)
            file_id = f"file::{rel}"
            add_node(file_id, "file", file_path.name, path=rel, crate=swift_crate_name)
            add_edge(swift_crate_id, file_id, "contains")

            # `derive_ui_module_path` strips a leading `Sources/` so
            # `Sources/Foo/Bar.swift` → `crate::Foo::Bar`; files in
            # `Tests/` etc. keep their dir prefix.
            module_path = derive_ui_module_path(
                swift_crate_name, file_path.relative_to(swift_root)
            )
            module_id = f"module::{module_path}"
            add_node(
                module_id,
                "module",
                module_path,
                path=rel,
                crate=swift_crate_name,
                module=module_path,
            )
            add_edge(swift_crate_id, module_id, "contains")
            add_edge(module_id, file_id, "contains")

            try:
                content = file_path.read_text(encoding="utf-8", errors="ignore")
            except Exception:
                continue

            swift_funcs, swift_types = extract_swift_symbols(content)
            swift_file_is_test = is_test_path(rel)
            swift_function_by_line: dict[int, str] = {}
            for fn in swift_funcs:
                fn_name = fn["name"]
                fn_line = int(fn["line"])
                sym_id = f"symbol::{rel}::function::{fn_name}::{fn_line}"
                add_node(
                    sym_id,
                    "function",
                    fn_name,
                    path=rel,
                    crate=swift_crate_name,
                    module=module_path,
                    line=fn_line,
                    test=swift_file_is_test or None,
                )
                add_edge(file_id, sym_id, "defines")
                add_edge(module_id, sym_id, "contains")
                function_nodes_by_file_name[(rel, fn_name)].append(sym_id)
                function_nodes_by_file_line[(rel, fn_line)] = sym_id
                function_nodes_by_crate_name[(swift_crate_name, fn_name)].append(sym_id)
                function_nodes_by_name[fn_name].append(sym_id)
                swift_function_by_line[fn_line] = sym_id

            # Swift's call syntax is close enough to the existing TS/JS
            # lightweight scanner for plain and receiver-method calls. Deferred
            # resolution is important for XCTest: calls from a test target must
            # become `test_calls` edges into app/shared production symbols.
            for site in extract_call_sites(content, swift_funcs, "swift"):
                owner_id = swift_function_by_line.get(int(site["owner_line"]))
                if not owner_id:
                    continue
                pending_swift_calls.append({
                    "owner_id": owner_id,
                    "callee": str(site["callee"]),
                    "file_rel": rel,
                    "crate": swift_crate_name,
                })
            for ty in swift_types:
                # Map Swift kinds onto the graph's existing kinds so
                # downstream filters (kind=struct / kind=enum) keep
                # working uniformly. `protocol` and `actor` map to
                # `trait` (closest Rust analogue); `extension` maps
                # to `type_alias` (it's structurally a re-opening of
                # an existing type — closest neutral bucket).
                kind_map = {
                    "class": "struct",
                    "struct": "struct",
                    "enum": "enum",
                    "protocol": "trait",
                    "actor": "trait",
                    "extension": "type_alias",
                }
                graph_kind = kind_map.get(ty["kind"], "struct")
                ty_name = ty["name"]
                ty_line = int(ty["line"])
                sym_id = f"symbol::{rel}::{graph_kind}::{ty_name}::{ty_line}"
                add_node(
                    sym_id,
                    graph_kind,
                    ty_name,
                    path=rel,
                    crate=swift_crate_name,
                    module=module_path,
                    line=ty_line,
                    test=swift_file_is_test or None,
                )
                add_edge(file_id, sym_id, "defines")
                add_edge(module_id, sym_id, "contains")

        emit_metadata_files_for_project(
            swift_root, root, swift_crate_id, swift_crate_name, add_node, add_edge,
        )

    # ── Python projects ──────────────────────────────────────────────
    # Auto-discovered via `pyproject.toml` / `setup.py`. Each project
    # mints a virtual crate; the whole project tree is walked
    # recursively with the standard pruning rules (no `src/`
    # privileging — `derive_ui_module_path` handles the leading-`src`
    # strip when computing module IDs).
    prog.set_phase("indexing Python projects")
    for py_crate_name, py_proj_root in discover_python_projects(root):
        if py_crate_name in workspace_crates:
            # Don't double-index when a project also has a Cargo/Node
            # marker that already produced a crate of the same name.
            continue
        py_crate_id = f"crate::{py_crate_name}"
        workspace_crates.add(py_crate_name)
        add_node(py_crate_id, "crate", py_crate_name, path=relpath(py_proj_root, root))

        for file_path in sorted(
            p for p in _walk_pruned(py_proj_root, skip_subroots=all_project_roots_frozen)
            if p.suffix == ".py"
        ):
            file_rel = relpath(file_path, root)
            prog.tick(file=file_rel)
            module_rel = file_path.relative_to(py_proj_root)
            module_path = derive_ui_module_path(py_crate_name, module_rel)
            file_id = f"file::{file_rel}"
            module_id = f"module::{module_path}"
            add_node(file_id, "file", file_path.name, path=file_rel, crate=py_crate_name, module=module_path)
            add_node(module_id, "module", module_path, path=file_rel, crate=py_crate_name, module=module_path)
            add_edge(py_crate_id, file_id, "contains")
            add_edge(py_crate_id, module_id, "contains")
            add_edge(module_id, file_id, "contains")

            try:
                content = file_path.read_text(encoding="utf-8", errors="ignore")
            except Exception:
                continue
            for fn_match in PY_FUNCTION_RE.finditer(content):
                fn_name = fn_match.group(1)
                # Line number = number of newlines before match start + 1.
                fn_line = content.count("\n", 0, fn_match.start()) + 1
                sym_id = f"symbol::{file_rel}::function::{fn_name}::{fn_line}"
                # Python test signals: file path matches pytest convention,
                # OR function name starts with `test_` (pytest discovery).
                py_is_test = is_test_path(file_rel) or fn_name.startswith("test_")
                add_node(sym_id, "function", fn_name, path=file_rel, crate=py_crate_name, module=module_path, line=fn_line,
                         public=(not fn_name.startswith("_")),
                         test=py_is_test or None)
                add_edge(file_id, sym_id, "defines")
                add_edge(module_id, sym_id, "contains")
                function_nodes_by_file_name[(file_rel, fn_name)].append(sym_id)
                function_nodes_by_file_line[(file_rel, fn_line)] = sym_id
                function_nodes_by_crate_name[(py_crate_name, fn_name)].append(sym_id)
                function_nodes_by_name[fn_name].append(sym_id)
            for cls_match in PY_CLASS_RE.finditer(content):
                cls_name = cls_match.group(1)
                cls_line = content.count("\n", 0, cls_match.start()) + 1
                sym_id = f"symbol::{file_rel}::struct::{cls_name}::{cls_line}"
                add_node(sym_id, "struct", cls_name, path=file_rel, crate=py_crate_name, module=module_path, line=cls_line)
                add_edge(file_id, sym_id, "defines")
                add_edge(module_id, sym_id, "contains")

        emit_metadata_files_for_project(
            py_proj_root, root, py_crate_id, py_crate_name, add_node, add_edge,
        )

    # ── Orphan files (loose sources + docs/config outside projects) ──
    # Anything that wasn't pulled in by a per-project scanner — loose
    # `scripts/*.py`, top-level `*.mjs` helpers, repo-root READMEs,
    # `docs/**/*.md`, etc. — lands under a virtual `repo-root` crate
    # so it's still navigable from `cgraph_search`. Two sub-passes:
    #   1. source files (.py / .js / .ts / .rs / .swift / …) — full
    #      file+module nodes; .py gets function/class symbol extraction.
    #   1. metadata files (.md / .yaml / .toml / .json) — safe-key
    #      metadata extraction + Markdown section symbols.
    prog.set_phase("indexing orphan files")
    indexed_paths = {
        str(Path(n["path"]).resolve()) if "path" in n else ""
        for n in nodes.values()
        if n.get("kind") == "file"
    }
    orphan_crate_name = "repo-root"
    orphan_crate_id = f"crate::{orphan_crate_name}"
    orphan_emitted = False

    def _ensure_orphan_crate() -> None:
        nonlocal orphan_emitted
        if not orphan_emitted:
            workspace_crates.add(orphan_crate_name)
            add_node(orphan_crate_id, "crate", orphan_crate_name, path=".")
            orphan_emitted = True

    # 1. Orphan source files. Suffixes mirror what each per-project
    # scanner handles, plus Python (which currently only triggers on
    # `pyproject.toml`/`setup.py` — without one, every `.py` is loose).
    ORPHAN_SOURCE_EXTS: frozenset[str] = frozenset({
        ".py", ".rs", ".swift",
        ".ts", ".tsx", ".jsx", ".js", ".mjs", ".cjs",
        ".svelte", ".vue", ".html", ".css", ".scss",
    })
    for file_path in _walk_pruned(root):
        if file_path.suffix not in ORPHAN_SOURCE_EXTS:
            continue
        if str(file_path.resolve()) in indexed_paths:
            continue
        if file_path.name.endswith(".d.ts"):
            continue
        rel = relpath(file_path, root)
        if SECRET_FILE_RE.search(rel):
            continue
        prog.tick(file=rel)
        _ensure_orphan_crate()
        file_id = f"file::{rel}"
        module_path = derive_ui_module_path(orphan_crate_name, file_path.relative_to(root))
        module_id = f"module::{module_path}"
        add_node(file_id, "file", file_path.name, path=rel, crate=orphan_crate_name, module=module_path)
        add_node(module_id, "module", module_path, path=rel, crate=orphan_crate_name, module=module_path)
        add_edge(orphan_crate_id, file_id, "contains")
        add_edge(orphan_crate_id, module_id, "contains")
        add_edge(module_id, file_id, "contains")

        # Python: extract def/class symbols so loose scripts are
        # searchable by function name (the `discover_python_projects`
        # path does the same — keep parity).
        if file_path.suffix == ".py":
            try:
                content = file_path.read_text(encoding="utf-8", errors="ignore")
            except (OSError, UnicodeDecodeError):
                continue
            for fn_match in PY_FUNCTION_RE.finditer(content):
                fn_name = fn_match.group(1)
                fn_line = content.count("\n", 0, fn_match.start()) + 1
                sym_id = f"symbol::{rel}::function::{fn_name}::{fn_line}"
                py_is_test = is_test_path(rel) or fn_name.startswith("test_")
                add_node(sym_id, "function", fn_name, path=rel, crate=orphan_crate_name, module=module_path, line=fn_line,
                         public=(not fn_name.startswith("_")),
                         test=py_is_test or None)
                add_edge(file_id, sym_id, "defines")
                add_edge(module_id, sym_id, "contains")
                function_nodes_by_file_name[(rel, fn_name)].append(sym_id)
                function_nodes_by_file_line[(rel, fn_line)] = sym_id
                function_nodes_by_crate_name[(orphan_crate_name, fn_name)].append(sym_id)
                function_nodes_by_name[fn_name].append(sym_id)
            for cls_match in PY_CLASS_RE.finditer(content):
                cls_name = cls_match.group(1)
                cls_line = content.count("\n", 0, cls_match.start()) + 1
                sym_id = f"symbol::{rel}::struct::{cls_name}::{cls_line}"
                add_node(sym_id, "struct", cls_name, path=rel, crate=orphan_crate_name, module=module_path, line=cls_line)
                add_edge(file_id, sym_id, "defines")
                add_edge(module_id, sym_id, "contains")

    # 2. Orphan metadata files (READMEs, docs, root config).
    for ext in METADATA_FILE_EXTENSIONS:
        for file_path in sorted(root.rglob(f"*{ext}")):
            if any(part in DISCOVERY_EXCLUDE_DIRS or part.startswith(".")
                   for part in file_path.relative_to(root).parts):
                continue
            if file_path.name in DISCOVERY_EXCLUDE_FILES:
                continue
            if str(file_path.resolve()) in indexed_paths:
                continue
            rel = relpath(file_path, root)
            if SECRET_FILE_RE.search(rel):
                continue
            _ensure_orphan_crate()
            file_id = f"file::{rel}"
            module_path = derive_ui_module_path(orphan_crate_name, file_path.relative_to(root))
            module_id = f"module::{module_path}"
            kwargs: dict[str, object] = {"path": rel, "crate": orphan_crate_name, "module": module_path}
            headings: list[dict] = []
            if ext == ".md":
                try:
                    raw = file_path.read_text(encoding="utf-8", errors="ignore")
                except (OSError, UnicodeDecodeError):
                    raw = ""
                if raw:
                    md_meta, headings = extract_markdown_metadata(raw)
                    if md_meta:
                        kwargs["metadata"] = md_meta
            else:
                md_meta = safe_parse_metadata_file(file_path)
                if md_meta:
                    kwargs["metadata"] = md_meta
            add_node(file_id, "file", file_path.name, **kwargs)
            add_node(module_id, "module", module_path, path=rel, crate=orphan_crate_name, module=module_path)
            add_edge(orphan_crate_id, file_id, "contains")
            add_edge(orphan_crate_id, module_id, "contains")
            add_edge(module_id, file_id, "contains")
            for h in headings:
                clean = re.sub(r"[`*_\[\]]", "", h["title"]).strip()
                if not clean:
                    continue
                sym_id = f"symbol::{rel}::section::{clean}::{h['line']}"
                add_node(
                    sym_id, "section", clean,
                    path=rel, crate=orphan_crate_name, module=module_path,
                    line=h["line"], level=h["level"],
                )
                add_edge(file_id, sym_id, "defines")
                add_edge(module_id, sym_id, "contains")

    for pending in unresolved_handlers:
        endpoint_id = str(pending.get("endpoint_id", ""))
        handler_name = str(pending.get("handler_name", ""))
        handler_module_hint = str(pending.get("handler_module_hint", ""))
        rel_path = str(pending.get("rel_path", ""))
        crate_name = str(pending.get("crate_name", ""))
        if not endpoint_id or not handler_name or not rel_path or not crate_name:
            continue

        handler_symbol_id = resolve_function_symbol_id(
            handler_name,
            rel_path,
            crate_name,
            module_hint=handler_module_hint,
        )
        if not handler_symbol_id:
            continue

        add_edge(endpoint_id, handler_symbol_id, "handles")

        handler_params = str(function_params_by_symbol_id.get(handler_symbol_id, "")).strip()
        if not handler_params:
            continue

        payload_profile = payload_profile_by_source_id.get(endpoint_id)
        if payload_profile is None:
            continue

        payload_template_raw = payload_profile.get("payload_template", {})
        payload_template = dict(payload_template_raw) if isinstance(payload_template_raw, dict) else {
            "path": {},
            "query": {},
            "json": {},
        }

        schema_refs_raw = payload_profile.get("schema_refs", {})
        schema_refs = dict(schema_refs_raw) if isinstance(schema_refs_raw, dict) else {}

        payload_types = extract_payload_types_from_params(handler_params)
        for channel in ("path", "query", "json"):
            if not payload_types[channel]:
                continue

            template, struct_node_id, schema_name = resolve_schema_template(
                payload_types[channel][0],
                crate_name,
                struct_templates_by_crate_name,
                struct_templates_by_name,
                struct_nodes_by_crate_name,
                struct_nodes_by_name,
                enum_templates_by_crate_name,
                enum_templates_by_name,
                enum_nodes_by_crate_name,
                enum_nodes_by_name,
            )
            channel_template = payload_template.get(channel)
            has_channel_template = isinstance(channel_template, dict) and bool(channel_template)
            if channel != "path" or not has_channel_template:
                payload_template[channel] = template
            if schema_name:
                schema_refs[channel] = schema_name
            if struct_node_id:
                add_edge(endpoint_id, struct_node_id, "accepts_payload")

        payload_profile["schema_refs"] = schema_refs
        payload_profile["payload_template"] = payload_template

    # ── Deferred call resolution (cross-file, cross-crate) ────────────
    # Every per-file loop above stashed call sites into pending lists.
    # Now that every workspace function is in the indices, walk each
    # pending site through a permissive resolver: same file → same
    # crate → global-with-qualifier. When multiple candidates match
    # (e.g. `fn new` defined on several types in the same crate), we
    # emit edges to ALL of them — the alternative (bailing on
    # ambiguity, like the handler resolver does) would mark genuine
    # callees as dead. Over-emission inflates "is called" counts but
    # is the right tradeoff for dead-code analysis. Same-self edges
    # are filtered by `add_edge`.
    def resolve_call_targets(
        callee_name: str,
        file_rel: str,
        crate: str,
        module_hint: str = "",
    ) -> list[str]:
        # Tier 1: same file. Most common case — wins immediately.
        file_cands = filter_candidates_by_module_hint(
            function_nodes_by_file_name.get((file_rel, callee_name), []),
            module_hint,
        )
        if file_cands:
            return list(dict.fromkeys(file_cands))
        # Tier 2: same crate. The bulk of cross-file calls land here.
        crate_cands = filter_candidates_by_module_hint(
            function_nodes_by_crate_name.get((crate, callee_name), []),
            module_hint,
        )
        if crate_cands:
            return list(dict.fromkeys(crate_cands))
        # Tier 3: global. Only emit if the qualifier narrows the set —
        # otherwise an unqualified call to a common name (`new`,
        # `from`, `default`) would spam edges across every crate.
        if module_hint:
            global_cands = filter_candidates_by_module_hint(
                function_nodes_by_name.get(callee_name, []),
                module_hint,
            )
            if global_cands:
                return list(dict.fromkeys(global_cands))
        return []

    prog.set_phase("resolving cross-file calls")
    # `test_calls` is the test→anything subset of `calls`. Splitting the
    # two lets dead-code analysis ignore test-only callers while still
    # exposing test-coverage counts as a separate metric.
    test_node_ids: set[str] = {
        n["id"] for n in nodes.values()
        if n.get("kind") == "function" and n.get("test")
    }

    def kind_for_owner(owner_id: str) -> str:
        return "test_calls" if owner_id in test_node_ids else "calls"

    for site in pending_rust_calls:
        kind = kind_for_owner(site["owner_id"])
        for target in resolve_call_targets(
            site["callee"], site["file_rel"], site["crate"],
            module_hint=site.get("qualifier", ""),
        ):
            add_edge(site["owner_id"], target, kind)

    for site in pending_ts_calls:
        kind = kind_for_owner(site["owner_id"])
        for target in resolve_call_targets(
            site["callee"], site["file_rel"], site["crate"],
        ):
            add_edge(site["owner_id"], target, kind)

    for site in pending_js_calls:
        kind = kind_for_owner(site["owner_id"])
        for target in resolve_call_targets(
            site["callee"], site["file_rel"], site["crate"],
        ):
            add_edge(site["owner_id"], target, kind)

    for site in pending_swift_calls:
        kind = kind_for_owner(site["owner_id"])
        for target in resolve_call_targets(
            site["callee"], site["file_rel"], site["crate"],
        ):
            add_edge(site["owner_id"], target, kind)

    # ── Impl-trait edges ──────────────────────────────────────────────
    # `impl Foo for Bar { fn method(...) }` means `Bar::method`
    # implements `Foo::method`. Emit an `implements` edge from the
    # method's symbol to the trait's symbol when the trait is in the
    # workspace. External traits (`std::fmt::Display`, etc.) are
    # recorded as an `implements_trait` attribute on the method node
    # so queries can still find them.
    # `add_node` stores the symbol name under `label`, not `name` —
    # easy gotcha. Build the lookup against the right field.
    trait_symbols_by_name: dict[str, list[str]] = defaultdict(list)
    for node in nodes.values():
        if node.get("kind") == "trait":
            trait_symbols_by_name[str(node.get("label", ""))].append(node["id"])

    for pending in pending_impl_methods:
        method_id = pending["method_id"]
        trait_name = pending["trait_name"]
        trait_candidates = trait_symbols_by_name.get(trait_name, [])
        trait_id = unique_symbol_id(trait_candidates)
        if trait_id:
            add_edge(method_id, trait_id, "implements")
        else:
            method_node = nodes.get(method_id)
            if method_node is not None:
                method_node["implements_trait"] = trait_name

    # ── Hierarchical rollups for crate / module / file nodes ──────────
    # Each function's classification (test / dead / untested) bubbles up
    # to every container it lives under. Stored on the container node as
    # a `rollup` dict so the 2D/3D viewers can show "this crate has X
    # dead functions and Y untested production functions" without
    # re-aggregating client-side. Counts are also retrievable via the
    # HTTP/MCP endpoints — this is the in-graph snapshot.
    prog.set_phase("rolling up dead-code / coverage stats")
    prod_callers_in: dict[str, int] = defaultdict(int)
    test_callers_in: dict[str, int] = defaultdict(int)
    for (src, dst, kind), weight in edge_counts.items():
        if kind in {"calls", "handles", "implements"}:
            prod_callers_in[dst] += int(weight)
        elif kind == "test_calls":
            test_callers_in[dst] += int(weight)

    def _empty_rollup() -> dict:
        return {
            "total_fns": 0, "test_fns": 0, "prod_fns": 0,
            "dead_fns": 0,        # production fn w/ 0 prod callers
            "untested_fns": 0,    # production fn w/ 0 test callers (incl. dead)
            "public_fns": 0,      # production `pub fn` / `export` / non-`_` Python
            "prod_caller_edges": 0,
            "test_caller_edges": 0,
        }

    crate_rollup: dict[str, dict] = defaultdict(_empty_rollup)
    module_rollup: dict[str, dict] = defaultdict(_empty_rollup)
    file_rollup: dict[str, dict] = defaultdict(_empty_rollup)

    for node in nodes.values():
        if node.get("kind") != "function":
            continue
        is_test = bool(node.get("test"))
        prod_cnt = prod_callers_in.get(node["id"], 0)
        test_cnt = test_callers_in.get(node["id"], 0)
        public = bool(node.get("public"))
        crate_key = str(node.get("crate") or "")
        module_key = str(node.get("module") or "")
        file_key = str(node.get("path") or "")
        for key, store in (
            (crate_key, crate_rollup),
            (module_key, module_rollup),
            (file_key, file_rollup),
        ):
            if not key:
                continue
            s = store[key]
            s["total_fns"] += 1
            s["prod_caller_edges"] += prod_cnt
            s["test_caller_edges"] += test_cnt
            if is_test:
                s["test_fns"] += 1
            else:
                s["prod_fns"] += 1
                if public:
                    s["public_fns"] += 1
                if prod_cnt == 0:
                    s["dead_fns"] += 1
                if test_cnt == 0:
                    s["untested_fns"] += 1

    # Attach to container nodes. Match the rollup to the node by the
    # field functions actually carry: crate nodes by their label,
    # module nodes by the `module` attr, file nodes by `path`.
    for node in nodes.values():
        kind = node.get("kind")
        if kind == "crate":
            key = str(node.get("label") or "")
            if key in crate_rollup:
                node["rollup"] = crate_rollup[key]
        elif kind == "module":
            key = str(node.get("module") or node.get("label") or "")
            if key in module_rollup:
                node["rollup"] = module_rollup[key]
        elif kind == "file":
            key = str(node.get("path") or "")
            if key in file_rollup:
                node["rollup"] = file_rollup[key]

    # ── Codegraph extensions ──────────────────────────────────────────
    # Each registered extension can yield additional nodes/edges that
    # capture project-specific concepts (magician's skills/tools/agents,
    # other repos' equivalents). The core walker stays domain-agnostic;
    # see `scripts/codegraph_ext/__init__.py` for the contract.
    prog.set_phase("extensions: discovering domain nodes")
    try:
        from codegraph_ext import load_extensions
        extensions = load_extensions()
    except Exception as exc:
        print(f"[codegraph] extension loader failed: {exc}", file=sys.stderr)
        extensions = []
    in_progress_graph = {"nodes": list(nodes.values()), "edges": []}
    for ext in extensions:
        try:
            for ext_node in ext.discover_nodes(root, in_progress_graph):
                node_id = ext_node.get("id")
                if not node_id:
                    continue
                # Don't clobber an existing node with the same id —
                # extensions are additive; the core walker wins on collisions.
                if node_id not in nodes:
                    nodes[node_id] = dict(ext_node)
            for ext_edge in ext.discover_edges(root, in_progress_graph):
                src = ext_edge.get("from")
                dst = ext_edge.get("to")
                kind = ext_edge.get("kind") or "references"
                if not src or not dst:
                    continue
                add_edge(src, dst, kind)
        except Exception as exc:
            print(f"[codegraph] extension {ext.name} failed during discover: {exc}",
                  file=sys.stderr)

    node_list = sorted(nodes.values(), key=lambda x: x["id"])
    edge_list = []
    for (src, dst, kind), weight in sorted(edge_counts.items(), key=lambda x: (x[0][2], x[0][0], x[0][1])):
        edge_list.append({"from": src, "to": dst, "kind": kind, "weight": weight})

    payload_profiles.sort(key=lambda item: str(item.get("source_id", "")))

    stats = {
        "node_count": len(node_list),
        "edge_count": len(edge_list),
        "node_kinds": dict(sorted(_count_by(node_list, "kind").items())),
        "edge_kinds": dict(sorted(_count_by(edge_list, "kind").items())),
        "workspace_crates": sorted(workspace_crates),
        "endpoint_count": len([node for node in node_list if node.get("kind") == "endpoint"]),
        "api_call_count": len([node for node in node_list if node.get("kind") == "api_call"]),
        "payload_profile_count": len(payload_profiles),
    }

    generated_at = dt.datetime.utcnow().replace(microsecond=0).isoformat() + "Z"

    graph = {
        "version": "1.2.0",
        "generated_at": generated_at,
        "commit": commit,
        "workspace": root.name,
        "nodes": node_list,
        "edges": edge_list,
    }

    payload_index = {
        "version": "1.0.0",
        "generated_at": generated_at,
        "commit": commit,
        "workspace": root.name,
        "profiles": payload_profiles,
    }

    prog.complete_file_scan()
    prog.set_phase("finalizing")
    prog.finish()
    return graph, stats, payload_index


def write_json(path: Path, payload: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    # Compact separators: indent=2 nearly doubled the artifact size, and
    # the explorer pays every extra byte again in JSON.parse at boot
    # (graph.json is hundreds of MB). All consumers use standard
    # json.loads, which is indifferent to whitespace.
    path.write_text(json.dumps(payload, separators=(",", ":"), sort_keys=False) + "\n", encoding="utf-8")


def main() -> int:
    parser = argparse.ArgumentParser(description="Generate interactive code graph artifacts")
    parser.add_argument("--root", default=None, help="Workspace root (defaults to repo root)")
    parser.add_argument("--output", default="docs/codegraph/graph.json", help="Graph JSON output path")
    parser.add_argument("--stats", default="docs/codegraph/stats.json", help="Stats JSON output path")
    parser.add_argument(
        "--payload",
        default="docs/codegraph/payload_profiles.json",
        help="Payload profile JSON output path",
    )
    parser.add_argument("--validate", default=None, help="Validate an existing graph JSON file")
    args = parser.parse_args()

    script_root = Path(__file__).resolve().parents[1]
    root = Path(args.root).resolve() if args.root else script_root

    if args.validate:
        target = Path(args.validate).resolve()
        if not target.exists():
            print(f"graph-check failed: file not found: {target}", file=sys.stderr)
            return 1
        try:
            obj = json.loads(target.read_text(encoding="utf-8"))
        except Exception as exc:
            print(f"graph-check failed: invalid JSON: {exc}", file=sys.stderr)
            return 1
        errors = validate_graph_object(obj)
        if errors:
            print("graph-check failed:", file=sys.stderr)
            for err in errors:
                print(f"- {err}", file=sys.stderr)
            return 1
        print(f"graph-check passed: {target}")
        return 0

    try:
        graph, stats, payload_profiles = generate(root)
    except Exception as exc:
        print(f"graph-index failed: {exc}", file=sys.stderr)
        return 1

    graph_out = (root / args.output).resolve()
    stats_out = (root / args.stats).resolve()
    payload_out = (root / args.payload).resolve()

    write_json(graph_out, graph)
    write_json(stats_out, stats)
    write_json(payload_out, payload_profiles)

    # UI-served variant: the 2D explorer never reads test_calls edges
    # (715k of them, ~half the artifact bytes) — they exist for the MCP
    # server and CLI queries. Shipping a graph without them roughly
    # halves the explorer's fetch+parse+index boot cost.
    graph_ui_out = graph_out.parent / "graph_ui.json"
    graph_ui = dict(graph)
    graph_ui["edges"] = [edge for edge in graph["edges"] if edge["kind"] != "test_calls"]
    write_json(graph_ui_out, graph_ui)
    print(f"[codegraph] wrote {graph_ui_out} ({len(graph_ui['edges'])} ui edges)")

    print(
        "graph-index generated:"
        f" nodes={stats['node_count']}"
        f" edges={stats['edge_count']}"
        f" endpoints={stats['endpoint_count']}"
        f" api_calls={stats['api_call_count']}"
        f" payload_profiles={stats['payload_profile_count']}"
        f" graph={graph_out}"
        f" stats={stats_out}"
        f" payload={payload_out}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
