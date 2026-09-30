#!/usr/bin/env python3
"""Shared drift gate for generated-and-committed artifacts.

Some artifacts in this repo are generated from a source of truth and then
committed, so they can be read without a build. That only works if something
notices when the source moves and the artifact does not. Regenerating on every
build is not an option when generation needs a cargo run; this is the cheap
check that *can* run every time.

  stamp   Hash the sources and embed the digest in the generated file as a
          `SOURCE_HASH:` comment. Run right after generation.
  check   Recompute and compare. Pure file IO, milliseconds. Wired as a
          prerequisite to build and test so a forgotten regeneration fails with
          a remediation line instead of letting the artifact quietly diverge.

The comment syntax comes from the output file's extension, so a generator can
emit HTML, TypeScript, YAML or shell without this script needing to know which.

Hashing whole source files rather than the relevant span is deliberate: it
occasionally over-triggers, and never under-triggers, which is the only
direction that matters.

  codegen_drift.py stamp --label event-taxonomy \\
      --regen "make event-taxonomy-codegen" --output out.ts src/a.rs src/b.rs
"""

from __future__ import annotations

import argparse
import hashlib
import re
import sys
from pathlib import Path

MARKER = "SOURCE_HASH:"

# Comment syntax by extension. A generated file has to carry the digest inside
# itself — a sidecar would be one more thing to forget to commit.
COMMENT_STYLES = {
    ".ts": ("// ", ""),
    ".tsx": ("// ", ""),
    ".js": ("// ", ""),
    ".rs": ("// ", ""),
    ".html": ("<!-- ", " -->"),
    ".svg": ("<!-- ", " -->"),
    ".xml": ("<!-- ", " -->"),
    ".yaml": ("# ", ""),
    ".yml": ("# ", ""),
    ".py": ("# ", ""),
    ".sh": ("# ", ""),
}


def comment_style(output: Path) -> tuple[str, str]:
    try:
        return COMMENT_STYLES[output.suffix]
    except KeyError:
        raise SystemExit(
            f"codegen_drift: no comment syntax known for '{output.suffix}' "
            f"({output}). Add it to COMMENT_STYLES."
        )


def marker_re(output: Path) -> re.Pattern[str]:
    open_c, close_c = comment_style(output)
    return re.compile(
        rf"^{re.escape(open_c)}{re.escape(MARKER)} ([0-9a-f]{{64}}){re.escape(close_c)}\s*$",
        re.MULTILINE,
    )


def source_hash(sources: list[Path]) -> str:
    """SHA-256 over each source's name and bytes, with separators so reordering
    or renaming a source invalidates the digest too."""
    h = hashlib.sha256()
    for path in sources:
        h.update(b"\x1f")
        h.update(path.name.encode("utf-8"))
        h.update(b"\x1f")
        h.update(path.read_bytes())
    return h.hexdigest()


def first_stamp_line(lines: list[str]) -> int:
    """Where the marker goes in a freshly generated file that has none yet.

    After whatever the file uses as its opening: a shebang, a doctype, or a
    leading block comment. Generated files put a "DO NOT EDIT BY HAND" header
    first for a reason, and a hash line above it buries the instruction. This
    also keeps regeneration idempotent — placing the marker somewhere the
    previous run did not means every regeneration rewrites the file even when
    the content is identical.
    """
    if not lines:
        return 0
    if lines[0].startswith(("#!", "<!DOCTYPE", "<!doctype")):
        return 1
    if lines[0].lstrip().startswith(("/*", "<!--")):
        closer = "*/" if lines[0].lstrip().startswith("/*") else "-->"
        for i, line in enumerate(lines):
            if closer in line:
                return i + 1
    return 0


def stamp(label: str, sources: list[Path], output: Path) -> int:
    open_c, close_c = comment_style(output)
    digest = source_hash(sources)
    line = f"{open_c}{MARKER} {digest}{close_c}"
    content = output.read_text(encoding="utf-8")
    pattern = marker_re(output)

    if pattern.search(content):
        content = pattern.sub(line, content, count=1)
    else:
        lines = content.split("\n")
        lines.insert(first_stamp_line(lines), line)
        content = "\n".join(lines)

    output.write_text(content, encoding="utf-8")
    print(f"[{label}] stamped {output} with source hash {digest[:12]}…")
    return 0


def check(label: str, regen: str, sources: list[Path], output: Path) -> int:
    if not output.exists():
        print(
            f"[{label}] FAIL — the generated file is missing: {output}\n"
            f"         Run `{regen}` to produce it.",
            file=sys.stderr,
        )
        return 1

    content = output.read_text(encoding="utf-8")
    match = marker_re(output).search(content)
    if match is None:
        print(
            f"[{label}] FAIL — no `{MARKER} <hex>` marker in {output}.\n"
            f"         Run `{regen}` to regenerate it.",
            file=sys.stderr,
        )
        return 1

    embedded, current = match.group(1), source_hash(sources)
    if embedded == current:
        return 0

    print(
        f"[{label}] FAIL — the source changed but {output}\n"
        f"         still embeds the old {MARKER} Regenerate and commit it:\n"
        f"\n"
        f"             {regen}\n"
        f"             git add {output}\n"
        f"\n"
        f"         Embedded hash : {embedded[:16]}…\n"
        f"         Current hash  : {current[:16]}…",
        file=sys.stderr,
    )
    return 1


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("mode", choices=["stamp", "check"])
    parser.add_argument("--label", required=True, help="prefix for messages")
    parser.add_argument("--regen", required=True, help="the command that regenerates the artifact")
    parser.add_argument("--output", required=True, type=Path, help="the generated file")
    parser.add_argument("sources", nargs="+", type=Path, help="files the artifact is generated from")
    args = parser.parse_args(argv[1:])

    for src in args.sources:
        if not src.exists():
            print(f"[{args.label}] missing source: {src}", file=sys.stderr)
            return 2

    if args.mode == "stamp":
        return stamp(args.label, args.sources, args.output)
    return check(args.label, args.regen, args.sources, args.output)


if __name__ == "__main__":
    sys.exit(main(sys.argv))
