#!/usr/bin/env python3
"""Ollama single-chokepoint enforcement gate.

Principle (docs/plans/2026-07-14-ollama-single-chokepoint.md, extended by
docs/archive/plans/2026-08-24-llm-chokepoint-closure.md): every ollama *inference*
call — generation AND embedding — goes through `magicllm`'s `OllamaProvider`.
Callers must NOT hand-build an `/api/generate`, `/api/chat`, or `/api/embed`
request body and POST it directly.

This gate scans the Rust source of the ollama-touching crates for those
endpoint literals and FAILS (exit 1, printing every offender) if any appears
in production, non-allowlisted code. It is the drift-guard that turns a future
bypass into a red build instead of a silent regression. Deterministic: static
text scan, no network, no ollama.

The scanner is string/comment aware (a naive substring scan is fooled by the
`//` inside a `http://…` URL and by braces inside string/comment literals):
  * it distinguishes the `/api/…` literal appearing in CODE (incl. inside a
    string literal — that IS a hand-built request and must be caught) from one
    that appears only inside a `//` line comment (docs — ignored);
  * `#[cfg(test)]`-family block tracking counts braces on code only
    (string/comment braces stripped) so an unbalanced brace in a test fixture
    can't leak the rest of the file as "test code".

Allowed occurrences (NOT offenders):
  * `magicllm/src/providers/ollama.rs`      — the provider itself (the chokepoint)
  * `magician/.../dispatch_glue/prewarm.rs` — documented warm-up exception
                                              (num_predict:1 model-load ping,
                                              not inference)
  * `magicllm/src/dispatch/local_prep.rs`   — assembles the provider's base_url
                                              (`{host}/api/generate`) then calls
                                              `OllamaProvider::invoke`; provider
                                              wiring, not a direct POST.
  * any file path containing `/tests/`      — test fixtures
  * lines inside a `#[cfg(test)]` module    — test fixtures
  * `//` line comments                      — docs/prose
  * a `/api/…` literal on a line that is a config const / URL-normalization and
    NOT a request-building call (no `.post(`/`.send(`/`.json(`/`reqwest`).

Known limitation: a purely text scan cannot catch an endpoint assembled from
split/`concat!`'d string fragments or sourced entirely from runtime config
(the literal never appears near the POST). Those require semantic/AST analysis;
this gate deliberately covers the realistic hand-written forms.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent

# Roots whose Rust sources are scanned. Every crate that imports `reqwest` and
# can reach the ollama daemon belongs here so a `/api/generate` dropped in a new
# crate can't hide from the gate. (`magician-vector-index` talks to ollama for
# embeddings today — `/api/embed`, which does not match the generate/chat
# pattern — but a future generation call there would now be caught.)
SCAN_ROOTS = [
    REPO_ROOT / "magicllm" / "src",
    REPO_ROOT / "magician" / "src",
    REPO_ROOT / "magician-vector-index" / "src",
    REPO_ROOT / "magician-comms" / "src",
    REPO_ROOT / "magician-api" / "src",
    REPO_ROOT / "magician-bin" / "src",
    REPO_ROOT / "magician-core" / "src",
]

ENDPOINT = ("/api/generate", "/api/chat", "/api/embed")

# Files allowlisted by path (relative to REPO_ROOT), POSIX form.
ALLOWLISTED_FILES = {
    "magicllm/src/providers/ollama.rs",
    "magician/src/magician_v2/dispatch_glue/prewarm.rs",
    "magicllm/src/dispatch/local_prep.rs",
}

# A line that builds/sends an HTTP request. If a `/api/…` literal shares a line
# with one of these, it is a POST — never excused as a benign config const.
REQUEST_INDICATORS = (".post(", ".send(", ".json(", "RequestBuilder", "reqwest::")

# Config-const / URL-normalization tokens: a `/api/…` literal on such a line is
# benign ONLY when the line is not also a request-building call.
BENIGN_SUBSTRINGS = ("DEFAULT_BASE_URL", "api_base_url", "strip_suffix")

# URL normalization lives in helpers, not on one-liners: a `#[cfg]`-free fn whose
# whole job is appending `/api/generate` to a base URL touches the literal on
# lines that carry no other signal. Narrow by enclosing function name instead of
# allowlisting the whole file — a request-building call inside these fns is
# still an offender (REQUEST_INDICATORS wins over fn residency).
FN_HEADER_RE = re.compile(
    r"^\s*(?:pub(?:\([a-z_]+\))?\s+)?(?:const\s+)?(?:async\s+)?(?:unsafe\s+)?"
    r"(?:extern\s+\"[^\"]*\"\s+)?fn\s+([A-Za-z0-9_]+)"
)
BENIGN_FUNCTION_NAMES = {"normalize_ollama_generation_endpoint"}


def line_comment_start(line: str) -> int | None:
    """Index of the `//` that starts a Rust line comment, or None. String-aware:
    the `//` inside `"http://…"` is not a comment (it's inside a string)."""
    in_str = False
    escape = False
    i = 0
    n = len(line)
    while i < n:
        c = line[i]
        if in_str:
            if escape:
                escape = False
            elif c == "\\":
                escape = True
            elif c == '"':
                in_str = False
        else:
            if c == '"':
                in_str = True
            elif c == "/" and i + 1 < n and line[i + 1] == "/":
                return i
        i += 1
    return None


def code_portion(line: str) -> str:
    """The line with any trailing `//` comment removed (string-aware)."""
    cut = line_comment_start(line)
    return line if cut is None else line[:cut]


def strip_strings(code: str) -> str:
    """Remove Rust string-literal *contents* (keep structure) so brace counting
    ignores `{`/`}` that live inside strings."""
    out: list[str] = []
    in_str = False
    escape = False
    for c in code:
        if in_str:
            if escape:
                escape = False
            elif c == "\\":
                escape = True
            elif c == '"':
                in_str = False
                out.append(c)
        else:
            out.append(c)
            if c == '"':
                in_str = True
    return "".join(out)


# --- `#[cfg(...)]` test-attribute recognition --------------------------------
#
# The repo overwhelmingly writes test modules as
# `#[cfg(any(test, feature = "test-fixtures"))]` (1000+ blocks) rather than the
# canonical `#[cfg(test)]`. Recognition must:
#   * accept `test` as a bare predicate — directly, or inside `any(...)`/`all(...)`;
#   * reject feature names that merely contain "test" (`feature =
#     "test-hash-embeddings"` compiles in production and must stay gated);
#   * reject `not(test)` — that item is compiled when NOT testing.

CFG_ATTR_START_RE = re.compile(r"#\[\s*cfg\s*\(")
RUST_STRING_RE = re.compile(r'"(?:[^"\\]|\\.)*"')
NOT_TEST_RE = re.compile(r"\bnot\s*\(\s*test\s*\)")
BARE_TEST_RE = re.compile(r"\btest\b")
MOD_TESTS_RE = re.compile(r"^\s*(pub\s+)?mod\s+tests\b")


def _cfg_attribute_inners(code: str) -> list[str]:
    """Inner predicate text of every complete `#[cfg(...)]` in `code` (one line)."""
    inners: list[str] = []
    for match in CFG_ATTR_START_RE.finditer(code):
        depth = 0
        i = match.end() - 1  # index of the `(` that opens cfg's parens
        start = i + 1
        while i < len(code):
            c = code[i]
            if c == "(":
                depth += 1
            elif c == ")":
                depth -= 1
                if depth == 0:
                    inners.append(code[start:i])
                    break
            i += 1
    return inners


def is_test_cfg(code: str) -> bool:
    """True when a `#[cfg(...)]` in `code` compiles the item only for tests."""
    for inner in _cfg_attribute_inners(code):
        # Blank out string contents first: `feature = "test-fixtures"` must not
        # look like a bare `test` predicate.
        predicates = RUST_STRING_RE.sub('""', inner)
        predicates = NOT_TEST_RE.sub("", predicates)
        if BARE_TEST_RE.search(predicates):
            return True
    return False


def scan_lines(lines: list[str]) -> list[tuple[int, str]]:
    """Return [(lineno, text)] offenders for one file's lines (empty = clean)."""
    offenders: list[tuple[int, str]] = []
    cfg_test_pending = False  # saw a test-only `#[cfg(...)]`/`mod tests`, awaiting the item's `{`
    depth = 0  # open `{`s in code (string-stripped)
    test_region_starts: list[int] = []  # depth at which each open test region began
    benign_fn_start_depth: int | None = None  # depth where a benign-named fn's body opened

    for lineno, raw in enumerate(lines, start=1):
        code = code_portion(raw)  # drop `//` comments (string-aware)
        code_no_str = strip_strings(code)  # for brace math only

        if is_test_cfg(code) or MOD_TESTS_RE.match(code):
            cfg_test_pending = True

        fn_match = FN_HEADER_RE.match(code)
        if fn_match is not None and fn_match.group(1) in BENIGN_FUNCTION_NAMES:
            benign_fn_start_depth = depth  # its `{` (if any) opens at depth + 1

        for ch in code_no_str:
            if ch == "{":
                if cfg_test_pending:
                    test_region_starts.append(depth)
                    cfg_test_pending = False
                depth += 1
            elif ch == "}":
                depth -= 1
                while test_region_starts and depth <= test_region_starts[-1]:
                    test_region_starts.pop()
                if benign_fn_start_depth is not None and depth <= benign_fn_start_depth:
                    benign_fn_start_depth = None

        # A match in CODE (including inside a string literal — that's a
        # hand-built request) counts; a match only in a `//` comment does not.
        # A region opened on this very line already counts as test code.
        if any(ep in code for ep in ENDPOINT) and not test_region_starts:
            has_request = any(ind in code for ind in REQUEST_INDICATORS)
            in_benign_fn = benign_fn_start_depth is not None and depth > benign_fn_start_depth
            is_benign = (not has_request) and (
                any(sub in code for sub in BENIGN_SUBSTRINGS) or in_benign_fn
            )
            if not is_benign:
                offenders.append((lineno, raw.strip()))

    return offenders


def scan_file(path: Path) -> list[tuple[int, str]]:
    """Return [(lineno, text)] offenders for one file (empty = clean)."""
    rel = path.relative_to(REPO_ROOT).as_posix()
    if rel in ALLOWLISTED_FILES or "/tests/" in rel:
        return []

    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except (OSError, UnicodeDecodeError):
        return []

    return scan_lines(lines)


def self_test() -> None:
    """Assert the scanner's building blocks on fixed fixtures (fast, no I/O).

    The `#[cfg(...)]` recognition is the most load-bearing piece of the gate:
    this repo writes ~1000 test blocks as `#[cfg(any(test, feature =
    "test-fixtures"))]`, and misreading them as production code floods the gate
    with false offenders (which is how a red gate gets ignored).
    """
    assert is_test_cfg('#[cfg(test)]')
    assert is_test_cfg('#[cfg(any(test, feature = "test-fixtures"))]')
    assert is_test_cfg('#[cfg(all(test, unix))]')
    assert is_test_cfg('#[cfg(any(test, debug_assertions))]')
    assert is_test_cfg('    #[cfg(any(test, feature = "test-fixtures"))] mod tests {')
    # NOT test-only: feature names merely containing "test" compile in prod.
    assert not is_test_cfg('#[cfg(feature = "test-hash-embeddings")]')
    assert not is_test_cfg('#[cfg(feature = "test-support")]')
    # NOT test-only: compiled when *not* testing.
    assert not is_test_cfg('#[cfg(not(test))]')
    assert not is_test_cfg('#[cfg(any(not(test), feature = "bench"))]')
    # Comment stripping stays string-aware.
    assert line_comment_start('let url = "http://x//y"; // real // comment') == 25
    assert code_portion('let a = 1; // /api/generate docs') == 'let a = 1; '
    # A hand-built request inside a string literal is still an offender.
    assert any(ep in code_portion('let u = format!("{base}/api/generate");') for ep in ENDPOINT)
    # Test-region tracking: content inside a `#[cfg(any(test, ...))]` module is
    # clean, production code after it still offends, and a nested test module
    # must not close the outer region early (the stack, not a lone flag).
    assert scan_lines([
        'fn prod_a() { let u = "/api/generate"; }',
        '#[cfg(any(test, feature = "test-fixtures"))]',
        'mod tests {',
        '    fn t() { let u = "/api/generate"; }',
        '    #[cfg(test)]',
        '    mod inner { fn t2() { let u = "/api/generate"; } }',
        '    fn t3() { let u = "/api/generate"; }',
        '}',
        'fn prod_b() { let u = "/api/generate"; }',
    ]) == [(1, 'fn prod_a() { let u = "/api/generate"; }'),
           (9, 'fn prod_b() { let u = "/api/generate"; }')]
    # A benign config const line is not an offender; a request-building line is.
    assert scan_lines(['let base = DEFAULT_BASE_URL.strip_suffix("/api/generate");']) == []
    assert scan_lines(['client.post(format!("{base}/api/generate")).send()']) != []


def main() -> int:
    self_test()
    all_offenders: list[tuple[str, int, str]] = []
    for root in SCAN_ROOTS:
        if not root.exists():
            continue
        for path in sorted(root.rglob("*.rs")):
            for lineno, text in scan_file(path):
                all_offenders.append((path.relative_to(REPO_ROOT).as_posix(), lineno, text))

    if all_offenders:
        print(
            "ollama-single-chokepoint gate FAILED: hand-built ollama "
            "/api/generate, /api/chat or /api/embed request(s) found outside "
            "the allowlist.",
            file=sys.stderr,
        )
        print(
            "Route ollama inference (generation and embeddings) through "
            "magicllm's OllamaProvider.\n"
            "See docs/plans/2026-07-14-ollama-single-chokepoint.md and\n"
            "docs/archive/plans/2026-08-24-llm-chokepoint-closure.md.\n",
            file=sys.stderr,
        )
        for rel, lineno, text in all_offenders:
            print(f"  {rel}:{lineno}: {text}", file=sys.stderr)
        return 1

    print(
        "ollama-single-chokepoint gate OK: no hand-built /api/generate, "
        "/api/chat or /api/embed request outside the allowlist."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
