#!/usr/bin/env python3
"""Per-workspace SilverBullet Space path helpers.

Keep these rules aligned with magician::notes::coerce_forest_pin_to_workspace_space
and magician::resource_authority::is_safe_scope_id.
"""

from __future__ import annotations

import os
import sys
from pathlib import Path

SPACES_DIR = "spaces"


def scope_id_is_safe(value: str) -> bool:
    if not value or value in {".", ".."} or len(value) > 255:
        return False
    if "/" in value or "\\" in value:
        return False
    return not any(ord(ch) < 32 for ch in value)


def scope_segment(value: str) -> str:
    return value if scope_id_is_safe(value) else "default"


def is_workspace_space(path: Path, principal: str, workspace: str) -> bool:
    return (
        path.name == workspace
        and path.parent.name == principal
        and path.parent.parent.name == SPACES_DIR
    )


def notes_pages_besides_spaces(path: Path) -> bool:
    if not path.is_dir():
        return False
    try:
        children = list(path.iterdir())
    except OSError:
        return False
    for child in children:
        if child.name in {".DS_Store", SPACES_DIR}:
            continue
        return True
    return False


def looks_like_on_disk_notes_forest(configured: Path) -> bool:
    if notes_pages_besides_spaces(configured):
        return False
    spaces = configured / SPACES_DIR
    if not spaces.is_dir():
        return False
    try:
        principals = list(spaces.iterdir())
    except OSError:
        return False
    for principal_dir in principals:
        if not principal_dir.is_dir() or not scope_id_is_safe(principal_dir.name):
            continue
        try:
            workspaces = list(principal_dir.iterdir())
        except OSError:
            continue
        for workspace_dir in workspaces:
            if workspace_dir.is_dir() and scope_id_is_safe(workspace_dir.name):
                return True
    return False


def space_is_too_broad(path: Path) -> bool:
    try:
        resolved = path.expanduser().resolve()
    except OSError:
        resolved = path.expanduser()
    if resolved == Path("/"):
        return True
    home = Path.home()
    try:
        return resolved == home.resolve()
    except OSError:
        return resolved == home


def coerce_space_path(
    configured: Path,
    forest: Path,
    principal: str,
    workspace: str,
) -> Path:
    """Resolve a settings pin to one workspace Space.

    A forest pin is the notes forest, its `spaces/` directory, or
    `spaces/<principal>`. An arbitrary ancestor of the default Space
    (HOME, `/`, the Magician runtime root) is not a forest pin.
    """
    principal = scope_segment(principal)
    workspace = scope_segment(workspace)
    configured = Path(os.path.expanduser(str(configured)))
    forest = Path(os.path.expanduser(str(forest)))
    scoped_default = forest / SPACES_DIR / principal / workspace

    if is_workspace_space(configured, principal, workspace):
        return configured

    parts = None
    try:
        parts = scoped_default.relative_to(configured).parts
    except ValueError:
        parts = None

    if parts is not None:
        prefix_ok = False
        if parts == ():
            prefix_ok = True
        elif (
            parts == (workspace,)
            and configured.name == principal
            and configured.parent.name == SPACES_DIR
        ):
            prefix_ok = True
        elif parts == (principal, workspace) and configured.name == SPACES_DIR:
            prefix_ok = True
        elif parts in {
            (SPACES_DIR,),
            (SPACES_DIR, principal),
            (SPACES_DIR, principal, workspace),
        }:
            prefix_ok = True
        if prefix_ok:
            return scoped_default

    from_forest = configured / SPACES_DIR / principal / workspace
    if from_forest.is_dir() or looks_like_on_disk_notes_forest(configured):
        return from_forest
    if configured.name == SPACES_DIR:
        from_spaces = configured / principal / workspace
        if from_spaces.is_dir() or not notes_pages_besides_spaces(configured):
            return from_spaces
    if configured.parent.name == SPACES_DIR and configured.name == principal:
        child = configured / workspace
        if child.is_dir() or not notes_pages_besides_spaces(configured):
            return child
    return configured


def is_this_forest_pin(
    configured: Path,
    forest: Path,
    principal: str,
    workspace: str,
) -> bool:
    """True when settings point at this process's forest or its spaces prefix."""
    configured = Path(os.path.expanduser(str(configured)))
    forest = Path(os.path.expanduser(str(forest)))
    principal = scope_segment(principal)
    try:
        configured_r = configured.resolve()
        forest_r = forest.resolve()
    except OSError:
        configured_r, forest_r = configured, forest
    if configured_r == forest_r:
        return True
    spaces = forest_r / SPACES_DIR
    return configured_r == spaces or configured_r == spaces / principal


def main(argv: list[str]) -> int:
    if len(argv) == 6 and argv[1] == "coerce":
        print(
            coerce_space_path(
                Path(argv[2]),
                Path(argv[3]),
                argv[4],
                argv[5],
            )
        )
        return 0
    print(
        "usage: notes_space_paths.py coerce <configured> <forest> <principal> <workspace>",
        file=sys.stderr,
    )
    return 2


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
