#!/usr/bin/env python3
"""Ratchet the number of markdown links in this repo that resolve to nothing.

`docs_guard.py` enforces that when behaviour-bearing code changes, the docs that
own it change too. It says nothing about whether those docs still *point*
anywhere. Nothing in the tree did, and the gap is not theoretical: archiving a
plan on 2026-08-11 moved a file and left six links hanging, and the repair only
happened because someone went looking by hand afterwards. A doc whose links are
dead is worse than a missing doc — it reads as authoritative and sends you to a
404.

What counts as a link
---------------------

Inline markdown links only — `[text](target)`. There are no reference-style
definitions (`[text]: target`) anywhere in the tree, so that form is not
handled; add it here if one ever appears. HTML anchors (`<a href=...>`) are
also not scanned — checking them properly means parsing HTML, and the tree has
exactly one outside code spans, which resolves. A target with a leading `/`
resolves from the
repository root, exactly as GitHub renders it; an OS-absolute path is therefore
dangling on every machine rather than resolving on the one machine whose
checkout path it hardcodes.

These are deliberately *not* violations:

* **External targets** — `http://`, `https://`, `mailto:`, `tel:`. Reachability
  is a network property, it changes without anyone touching this repo, and a
  guard that fails on someone else's outage gets disabled within a week.
* **Bare anchors** — `#section`. Resolving these means reimplementing GitHub's
  heading slugifier, whose rules differ from every other renderer's. A guard
  that is wrong about anchors is worse than no anchor checking.
* **Line citations** — `path/to/file.rs:137`. That is a reference to a line, not
  a navigable path, and it never resolved in the first place.
* **Code** — a link inside a ``` / ~~~ fence or an inline `backtick span` is an
  example of markdown, not markdown. Eleven fenced ones existed when the guard
  was written, and the first thing it caught was this guard's own writeup
  quoting a link to explain the case rule below.

A `#fragment` on a real path is trimmed and the path part is checked. So
`../config.yaml#L52` must have `../config.yaml`; whether line 52 exists is not
checked, for the same reason bare anchors are not.

What "exists" means
-------------------

A target resolves when the **git index** holds it — not when the filesystem
does. `markdown_sources()` already reads the index, so resolving targets with
`Path.exists()` was asymmetric: the guard scanned only committed or staged
docs while letting any file lying in a working tree satisfy their links. An
untracked target then resolves for whoever created it and 404s on every fresh
clone, which is the same checkout-dependent verdict the repo-root rule and the
containment clamp below both exist to prevent, reached by a third route. Ten
links in the tree were in that state when it was found.

Staged counts, because `git ls-files` reads the index: write a doc and the
file it points at, stage both, and the guard is satisfied before either is
committed. Two consequences worth knowing:

* An **empty directory** is dangling. Git tracks files, never directories, so
  a directory is in the repository only when something inside it is — and an
  empty one never survives a clone.
* A **case mismatch anywhere in the path** is dangling, not just in the final
  component. Git records paths exactly, so index membership is already
  case-correct: `[x](./Foo.md)` pointing at `foo.md` fails here, and so does a
  wrong-case intermediate directory, which the previous final-component
  `iterdir` check could not catch. macOS is case-insensitive by default and
  Linux is not, so both work on the machine that wrote them and 404 elsewhere.

The baseline
------------

Every current dangling link is recorded per file in
`markdown_link_baseline.json` as a count, deliberately not a line number: line
numbers churn on every edit and a stale entry silently stops matching, which is
the failure mode a baseline is supposed to prevent.

The ratchet runs both ways. Exceeding a file's baseline fails, and so does
beating it — because a count that is allowed to drift below silently stops being
a ratchet. Fix some links, run with `--update`, and commit the smaller number.

Renaming a file moves its count to a new key, which reads as "new file with
dangling links" and fails. That is intended — a rename is exactly when links
break — and `--update` is the fix once you have checked them.

Runs from `make check-links` and as part of `make check-all`.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from functools import lru_cache
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
BASELINE_PATH = Path(__file__).resolve().parent / "markdown_link_baseline.json"

# `[text](target)` or `[text](target "title")`. The target alternation allows a
# balanced `(...)` inside an unbracketed target so SvelteKit route groups —
# `src/routes/(app)/foo` — parse as one target instead of terminating at the
# first paren. The `<...>` form is markdown's escape hatch for targets with
# spaces.
LINK = re.compile(
    r"\[[^\]]*\]\(\s*(<[^>]+>|(?:[^()\s]|\([^()]*\))+)\s*(?:\"[^\"]*\")?\)"
)

EXTERNAL_SCHEMES = ("http://", "https://", "mailto:", "tel:", "ftp://", "data:")

# `foo.rs:137` — a line citation, not a path.
LINE_CITATION = re.compile(r"\.\w+:\d+$")

FENCE_CHARS = ("`", "~")


@lru_cache(maxsize=8)
def _index_paths(root: Path) -> frozenset[str]:
    """Every path in `root`'s git index, as repo-relative POSIX strings.

    One `git ls-files` per root for the whole run rather than a filesystem
    probe per link. `resolves()` asks this instead of the filesystem so the
    guard's verdict depends on what the repository contains, not on what
    happens to be sitting in one working tree.

    Keyed on `root` rather than cached globally because the test battery
    repoints `ROOT` at a fixture repository per case; a single-slot cache
    would hand the second case the first one's index.
    """
    result = subprocess.run(
        ["git", "ls-files", "-z"],
        cwd=root,
        capture_output=True,
        text=True,
        check=True,
    )
    return frozenset(name for name in result.stdout.split("\0") if name)


def markdown_sources() -> list[Path]:
    """Every markdown file in the index — tracked or staged, never vendored.

    `git ls-files` reads the index, so a newly staged doc is scanned before it
    is ever committed. Walking the tree instead would drag in `node_modules`
    and build output.
    """
    result = subprocess.run(
        ["git", "ls-files", "-z", "--", "*.md", "*.markdown"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=True,
    )
    return [ROOT / name for name in result.stdout.split("\0") if name]


def _fence_open(stripped: str) -> tuple[str, int] | None:
    """`(char, run length)` if this line opens a fence, else None.

    The subtlety that motivates this function: a line can *start* with three
    backticks and still not be a fence. CommonMark forbids backticks in a
    backtick fence's info string, so ``` `like this` ``` is an inline code
    span using a triple-backtick delimiter. The first version of this guard
    treated any ```-prefixed line as a fence toggle, and one such span in a
    live component doc silently blanked the 600 lines after it — the guard
    blinded itself to exactly the region it existed to watch.
    """
    for char in FENCE_CHARS:
        if stripped.startswith(char * 3):
            run = len(stripped) - len(stripped.lstrip(char))
            rest = stripped[run:]
            if char == "`" and "`" in rest:
                return None  # an inline span, not a fence
            return (char, run)
    return None


def _fence_close(stripped: str, fence: tuple[str, int]) -> bool:
    """CommonMark close: same char, at least the opening run, nothing after."""
    char, open_run = fence
    run = len(stripped) - len(stripped.lstrip(char))
    return run >= open_run and stripped[run:].strip() == ""


def _strip_spans(line: str) -> str:
    """Blank inline code spans: a backtick run closed by an equal-length run.

    Hand-rolled rather than a regex: the backreference regex this replaces was
    measured superlinear on adversarial input (a 8k-backtick line took 12s).
    This is a single pass over the line's backtick runs.
    """
    if "`" not in line:
        return line
    runs: list[tuple[int, int]] = []  # (start, length) of each backtick run
    i, n = 0, len(line)
    while i < n:
        if line[i] == "`":
            start = i
            while i < n and line[i] == "`":
                i += 1
            runs.append((start, i - start))
        else:
            i = line.find("`", i)
            if i == -1:
                break
    out = list(line)
    index = 0
    while index < len(runs):
        _, length = runs[index]
        close = next(
            (later for later in range(index + 1, len(runs)) if runs[later][1] == length),
            None,
        )
        if close is None:
            index += 1
            continue
        span_start = runs[index][0]
        span_end = runs[close][0] + runs[close][1]
        for position in range(span_start, span_end):
            out[position] = " "
        index = close + 1
    return "".join(out)


def strip_code(text: str) -> str:
    """Blank out fenced blocks and inline code spans, preserving line count.

    Fences are tracked per CommonMark: an opening run of >=3 backticks or
    tildes is remembered by character and length, and only a run of the same
    character at least that long with nothing after it closes the block. A
    genuinely unclosed fence blanks to end of file — which matches how GitHub
    renders it, so nothing in that region is a link there either.
    """
    out: list[str] = []
    fence: tuple[str, int] | None = None
    for line in text.splitlines():
        stripped = line.lstrip()
        if fence is None:
            opened = _fence_open(stripped)
            if opened is not None:
                fence = opened
                out.append("")
                continue
            out.append(_strip_spans(line))
        else:
            if _fence_close(stripped, fence):
                fence = None
            out.append("")
    return "\n".join(out)


def resolves(source: Path, target: str) -> bool:
    """Whether `target`, relative to `source`'s directory, names something real.

    A leading `/` resolves from the repository root, which is how GitHub
    renders it. It must NOT resolve from the filesystem root: `pathlib`
    discards the left operand when joining an absolute path, so an OS-absolute
    target that happens to exist on the author's machine — this repo had 21
    links hardcoding one author's checkout path — would pass here and fail on
    every other checkout, making the guard's verdict depend on where the repo
    is cloned. Under repo-root resolution those links are dangling everywhere,
    deterministically.
    """
    base = ROOT if target.startswith("/") else source.parent
    try:
        candidate = (base / target.lstrip("/")).resolve()
    except (OSError, ValueError, RuntimeError):
        # A malformed target (embedded NUL, unresolvable symlink loop) is a
        # dangling link, not a crash.
        return False
    # Contain to the repository. GitHub serves a link only from inside the
    # repo, so a target that escapes the root — `/../x`, enough `../` from a
    # nested doc, or a symlink that leads outside — is dangling there no
    # matter what happens to exist above the checkout on one machine. Without
    # this clamp, a `/../<sibling-directory>` link resolved on the machine
    # this guard was written on purely because that sibling existed there —
    # the same checkout-dependent verdict the repo-root rule exists to
    # prevent. An escape that resolves back INSIDE the repo (this checkout is
    # itself named `magician`, so `/../magician` is the repo root) passes,
    # correctly: containment is about where the target lands, not how the
    # path is spelled.
    if not candidate.is_relative_to(ROOT):
        return False
    relative = candidate.relative_to(ROOT).as_posix()
    # The repository root itself, reached by `/` or by enough `../`.
    if relative == ".":
        return True
    index = _index_paths(ROOT)
    # Resolve against the INDEX, not the filesystem.
    #
    # `markdown_sources()` already reads the index, so a guard that resolved
    # targets with `Path.exists()` was asymmetric: it scanned only committed
    # or staged docs, but let any file lying in the working tree satisfy their
    # links. An untracked target then resolves on the machine that has it and
    # 404s on every fresh clone — the same checkout-dependent verdict the
    # repo-root rule and the containment clamp above both exist to prevent,
    # arrived at by a third route. Measured when this changed: ten links
    # resolved only because of untracked files.
    #
    # Staged-but-uncommitted still counts, because `git ls-files` reads the
    # index — so the ordinary flow of writing a doc and the file it points at,
    # then staging both, keeps working.
    #
    # This also subsumes the case check that used to live here. Git records
    # paths exactly, so membership in the index is already case-correct, and
    # `[x](./Foo.md)` pointing at `foo.md` fails without a second `iterdir`.
    if relative in index:
        return True
    # A directory resolves when the index holds anything beneath it. Git
    # tracks files, never directories, so this is the only way to ask.
    prefix = relative + "/"
    return any(entry.startswith(prefix) for entry in index)


def dangling_in(path: Path) -> list[str]:
    """Every link target in `path` that names nothing, in source order."""
    try:
        text = path.read_text(encoding="utf-8", errors="replace")
    except OSError as error:  # unreadable file is a real failure, not a skip
        raise SystemExit(f"markdown-link guard: cannot read {path}: {error}")

    dangling: list[str] = []
    for match in LINK.finditer(strip_code(text)):
        target = match.group(1).strip("<>").strip()
        if not target or target.startswith("#"):
            continue
        if target.startswith(EXTERNAL_SCHEMES) or target.startswith("//"):
            continue
        # Trim `#fragment`; the path in front of it still has to exist.
        candidate = target.split("#", 1)[0]
        if not candidate or LINE_CITATION.search(candidate):
            continue
        if not resolves(path, candidate):
            dangling.append(target)
    return dangling


def scan() -> dict[str, list[str]]:
    """`{relative path: dangling targets}` for every file with at least one."""
    found: dict[str, list[str]] = {}
    for path in markdown_sources():
        if not path.is_file():  # a deleted-but-still-indexed path
            continue
        dangling = dangling_in(path)
        if dangling:
            found[path.relative_to(ROOT).as_posix()] = dangling
    return found


def load_baseline() -> dict[str, int]:
    if not BASELINE_PATH.exists():
        raise SystemExit(
            f"markdown-link guard: no baseline at {BASELINE_PATH}. "
            "Generate one with --update."
        )
    return json.loads(BASELINE_PATH.read_text(encoding="utf-8"))["files"]


def write_baseline(counts: dict[str, int]) -> int:
    total = sum(counts.values())
    payload = {
        "_comment": (
            "Known dangling markdown links per file for scripts/check_markdown_links.py. "
            "Counts, not line numbers, so ordinary edits do not churn this file. "
            "The ratchet is two-way: fix some links, re-run with --update, commit the "
            "smaller number."
        ),
        "total_dangling": total,
        "files": dict(sorted(counts.items())),
    }
    BASELINE_PATH.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
    return total


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--update",
        action="store_true",
        help="rewrite the baseline from the current tree",
    )
    parser.add_argument(
        "--list",
        action="store_true",
        help="print every dangling link with its source file, and exit 0",
    )
    args = parser.parse_args()

    found = scan()
    counts = {rel: len(targets) for rel, targets in found.items()}

    if args.list:
        for rel, targets in found.items():
            for target in targets:
                print(f"{rel}: {target}")
        return 0

    if args.update:
        total = write_baseline(counts)
        print(
            f"markdown-link baseline updated: {total} dangling links across "
            f"{len(found)} files"
        )
        return 0

    baseline = load_baseline()
    regressions: list[str] = []
    improvements: list[str] = []

    for rel in sorted(set(counts) | set(baseline)):
        now = counts.get(rel, 0)
        was = baseline.get(rel, 0)
        if now > was:
            regressions.append(
                f"  {rel}: {was} -> {now}"
                + ("  (new or renamed file)" if rel not in baseline else "")
            )
        elif now < was:
            improvements.append(f"  {rel}: {was} -> {now}")

    if regressions:
        print("markdown links regressed:", file=sys.stderr)
        print("\n".join(regressions), file=sys.stderr)
        print(
            "\nThese links point at paths that do not exist, or that differ in case\n"
            "(which works on macOS and 404s on Linux). Moving or renaming a file is\n"
            "the usual cause — fix the links in the file above, not just the links\n"
            "pointing at it.\n"
            "  python3 scripts/check_markdown_links.py --list   # show each one\n"
            "If a link is genuinely fine as written, re-run with --update.",
            file=sys.stderr,
        )
        return 1

    if improvements:
        print("markdown links improved — commit the smaller baseline:")
        print("\n".join(improvements))
        print("\n  python3 scripts/check_markdown_links.py --update")
        return 1

    total = sum(counts.values())
    print(
        f"markdown-link guard passed: {total} known dangling links across "
        f"{len(found)} files, none added"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
