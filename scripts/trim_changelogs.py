#!/usr/bin/env python3
"""Keep every per-crate CHANGELOG.md at its N most recent entries.

Why this exists
---------------
Per-crate changelogs are append-only by nature and nothing ever removed from
them. Left alone they reach tens of thousands of lines, at which point they
stop being read and start being a liability: they ship to the public mirror,
they bury the recent entries a reader actually wants, and a large fraction of
the bulk is work that was never released. Trimming them by hand is the kind of
chore that happens once and then never again, so it lives here instead.

The live file keeps the most recent entries. Everything older moves to
`docs/archive/changelogs/<name>.md`, which is excluded from the public mirror,
so no entry is ever destroyed — it stops being published, not written down.

What counts as an entry
-----------------------
These files are not uniform, so the rule follows how they are actually written
rather than one heading pattern:

* a level-2 heading that is not the `Unreleased` marker starts an entry (a
  release block, together with every `Added`/`Changed`/`Fixed` subsection
  under it), and
* a level-3 heading directly under `Unreleased` starts an entry.

That distinction matters: a level-3 heading inside a release block is a
category within that release, not a separate entry, and treating it as one
would cut a release in half.

Entries are newest-first in every file here, so "keep N" is the first N.

The root CHANGELOG.md is skipped
--------------------------------
The repository-root file is the curated product release log, one entry per
published release. It is not a crate log, it grows slowly, and its directory
is the repository root, so archiving it would resolve to a meaningless
archive name. It is excluded by path.

Version stamping
----------------
The newest entry should say which version shipped it. Where it carries no
version, the crate's own declared version is appended — unless that version
already appears on an older heading in the same file, which means the newest
entry is work *beyond* the last release. Stamping it there would claim one
version twice, so those are reported for a human to resolve with a real
version bump instead.

Link repair
-----------
Moving an entry out of a crate directory rots the relative links inside it: a
target written relative to the crate no longer resolves from the archive. Every
link in a touched archive file is re-resolved from its origin directory and
rewritten, so archiving does not silently manufacture dead links.

Usage
-----
    python3 scripts/trim_changelogs.py            # report only, changes nothing
    python3 scripts/trim_changelogs.py --check    # exit 1 if any file is over
    python3 scripts/trim_changelogs.py --apply    # trim, archive, repair links
    python3 scripts/trim_changelogs.py --keep 20  # different retention

Run `make check-links` afterwards: archived entries may reference files that
have since been deleted, and those belong in the dangling-link baseline.
"""
from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys

ARCHIVE_DIR = "docs/archive/changelogs"
ROOT_CHANGELOG = "CHANGELOG.md"

# Directories whose basename would be ambiguous or generic as an archive name.
ARCHIVE_NAMES = {
    "ui/unified-ui": "unified-ui",
    "sdk/typescript": "sdk-typescript",
    "skillshub/bots": "skillshub-bots",
    "examples/reference-apps/research-planner": "research-planner",
}

UNRELEASED = re.compile(r"^#{2,3}\s*\[?unreleased\]?\s*$", re.I)
HEADING = re.compile(r"^(#{2,3})\s+(.*)$")
SEMVER = re.compile(r"\d+\.\d+\.\d+(?:-[0-9A-Za-z.]+)?")
LINK = re.compile(r"(\]\()([^)\s#]+)((?:#[^)]*)?\))")


def tracked_changelogs() -> list[str]:
    out = subprocess.run(
        ["git", "ls-files", "*CHANGELOG*"], capture_output=True, text=True, check=True
    ).stdout.split()
    return [f for f in out if "node_modules" not in f and f != ROOT_CHANGELOG]


def declared_version(directory: str) -> tuple[str | None, str | None]:
    """The version this component declares, from whichever manifest it uses."""
    cargo = os.path.join(directory, "Cargo.toml")
    if os.path.exists(cargo):
        package = re.search(r"^\[package\](.*?)(?=^\[|\Z)", open(cargo).read(), re.S | re.M)
        if package:
            found = re.search(r'^version\s*=\s*"([^"]+)"', package.group(1), re.M)
            if found:
                return found.group(1), "Cargo.toml"
    package_json = os.path.join(directory, "package.json")
    if os.path.exists(package_json):
        try:
            version = json.load(open(package_json)).get("version")
            if version:
                return version, "package.json"
        except (ValueError, OSError):
            pass
    version_file = os.path.join(directory, "VERSION")
    if os.path.exists(version_file):
        return open(version_file).read().strip(), "VERSION"
    for manifest, pattern in (
        ("CMakeLists.txt", r'set\(PROJECT_VER "([^"]+)"\)'),
        ("project.yml", r'MARKETING_VERSION:\s*"([^"]+)"'),
        ("android/app/build.gradle.kts", r'versionName\s*=\s*"([^"]+)"'),
    ):
        path = os.path.join(directory, manifest)
        if os.path.exists(path):
            found = re.search(pattern, open(path).read())
            if found:
                return found.group(1), manifest
    return None, None


def split_entries(lines: list[str]) -> list[tuple[int, int, str]]:
    """Entry boundaries as (start, end, heading text), newest first."""
    starts: list[tuple[int, str]] = []
    inside_unreleased = False
    for index, line in enumerate(lines):
        match = HEADING.match(line)
        if not match:
            continue
        level, text = len(match.group(1)), match.group(2)
        if UNRELEASED.match(line):
            inside_unreleased = level == 2
            continue
        if level == 2:
            inside_unreleased = False
            starts.append((index, text))
        elif level == 3 and inside_unreleased:
            starts.append((index, text))
    return [
        (start, starts[n + 1][0] if n + 1 < len(starts) else len(lines), text)
        for n, (start, text) in enumerate(starts)
    ]


def archive_name(directory: str) -> str:
    return ARCHIVE_NAMES.get(directory, os.path.basename(directory))


def repair_links(archive_path: str, origin_dir: str) -> tuple[int, int]:
    """Re-resolve links written relative to the origin directory. -> (fixed, still dead)"""
    text = open(archive_path).read()
    archive_dir = os.path.dirname(archive_path)
    counts = [0, 0]

    def rewrite(match: re.Match) -> str:
        target = match.group(2)
        if target.startswith(("http:", "https:", "mailto:", "tel:", "/")) or "://" in target:
            return match.group(0)
        base = target.split("#")[0]
        if os.path.exists(os.path.normpath(os.path.join(archive_dir, base))):
            return match.group(0)
        from_origin = os.path.normpath(os.path.join(origin_dir, base))
        if not os.path.exists(from_origin):
            counts[1] += 1
            return match.group(0)
        counts[0] += 1
        return match.group(1) + os.path.relpath(from_origin, archive_dir) + match.group(3)

    rewritten = LINK.sub(rewrite, text)
    if rewritten != text:
        open(archive_path, "w").write(rewritten)
    return counts[0], counts[1]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--apply", action="store_true", help="write the changes")
    parser.add_argument("--check", action="store_true", help="exit 1 if any changelog is over the limit")
    parser.add_argument("--keep", type=int, default=5, help="entries to keep in the live file (default 5)")
    args = parser.parse_args()

    rows, unstamped, over = [], [], []
    for path in tracked_changelogs():
        directory = os.path.dirname(path) or "."
        lines = open(path, errors="ignore").read().split("\n")
        entries = split_entries(lines)
        version, source = declared_version(directory)
        name = archive_name(directory)
        archive_path = f"{ARCHIVE_DIR}/{name}.md"

        stamp_at = None
        if entries:
            top_index, _, top_text = entries[0]
            if not SEMVER.search(top_text) and version:
                if any(version in text for _, _, text in entries):
                    unstamped.append((path, version, source))
                else:
                    stamp_at = top_index

        stale = entries[args.keep:]
        if stale:
            over.append(path)
        rows.append((path, len(entries), len(stale), len(lines),
                     len(lines) - sum(end - start for start, end, _ in stale),
                     archive_path, bool(stamp_at)))

        if not args.apply:
            continue

        if stamp_at is not None:
            lines[stamp_at] = f"{lines[stamp_at].rstrip()} ({version})"
        if stale:
            moved = "\n".join(lines[stale[0][0]:]).rstrip() + "\n"
            note = (f"> Archived from `{path}`. The live file keeps the {args.keep} most "
                    f"recent entries; everything older is here, newest first.")
            os.makedirs(ARCHIVE_DIR, exist_ok=True)
            if os.path.exists(archive_path):
                existing = open(archive_path).read().split("\n")
                divider = next((i for i, l in enumerate(existing) if l.strip() == "---"), 0)
                existing = [note if l.startswith(">") else l for l in existing[:divider + 1]] + existing[divider + 1:]
                merged = ("\n".join(existing[:divider + 1]) + "\n\n" + moved + "\n"
                          + "\n".join(existing[divider + 1:]).lstrip("\n"))
            else:
                title = lines[0].lstrip("# ").strip() if lines and lines[0].startswith("#") else name
                merged = f"# {title} — archived entries\n\n{note}\n\n---\n\n{moved}"
            open(archive_path, "w").write(merged)

            body = "\n".join(lines[:stale[0][0]]).rstrip() + "\n"
            body += f"\n---\n\nOlder entries: [`{archive_path}`]({os.path.relpath(archive_path, directory)})\n"
            open(path, "w").write(body)
            fixed, dead = repair_links(archive_path, directory)
            if fixed or dead:
                print(f"  links in {archive_path}: {fixed} repointed, {dead} left dangling (target deleted)")
        elif stamp_at is not None:
            open(path, "w").write("\n".join(lines))

    verb = "trimmed" if args.apply else "would trim"
    print(f"{'file':<52}{'entries':>8}{'over':>6}{'lines':>8}{'after':>8}")
    for path, entries, stale, before, after, _, stamped in sorted(rows, key=lambda r: -r[3]):
        if stale or stamped or not args.check:
            print(f"{path:<52}{entries:>8}{stale:>6}{before:>8}{after:>8}"
                  + ("   +version stamp" if stamped else ""))
    print(f"\n{len(over)} of {len(rows)} changelogs {verb} to {args.keep} entries; "
          f"live lines {sum(r[3] for r in rows):,} -> {sum(r[4] for r in rows):,}")
    if unstamped:
        print("\nNewest entry left unstamped — that version is already on an older heading, so the")
        print("entry is work beyond the last release and wants a real version bump, not a docs edit:")
        for path, version, source in unstamped:
            print(f"  {path}: declares {version} ({source})")
    if args.apply:
        print("\nRun `make check-links` — archived entries may point at files that no longer exist.")
    if args.check and over:
        print(f"\nFAIL: {len(over)} changelog(s) over {args.keep} entries. Run `make trim-changelogs`.")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
