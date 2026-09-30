#!/usr/bin/env python3
"""Guard the literals the notes surfaces share across a language boundary.

Selection capture reaches Notes from three surfaces — an agent tool, the browser
extension, and the desktop overlay. Some of the values and boundary rules holding
those surfaces together are duplicated across languages, so nothing inside one
language can notice when another surface stops agreeing.

Both drifts fail silently rather than loudly, which is what makes them worth a
guard instead of a comment.

`save_to_notes` is the id the desktop overlay matches to decide that a button
captures the selection locally. The Rust side puts it in the action catalog and
the Svelte side compares against its own copy. Rename one and the comparison
simply stops matching, so the button falls through to the generic path and asks
a model to write something about the selection instead of filing it. A working
button that does the wrong thing, with no error anywhere.

The search result cap is the third: the backend clamps `limit` and the `/notes`
page offers "Show more" up to its own ceiling. If the page's ceiling drifts above
the backend's, the button stops changing anything — it keeps promising more
results and returning the same ones, which reads as a broken page rather than a
reached limit.

Browser captures must cross the same bearer-aware boundary as every other
extension request. Reintroducing body scope or a raw `fetch` would either make
the request unauthenticated or let the caller appear to choose its owner.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]

TAURI = ROOT / "desktop/src-tauri/src/contextual_assist.rs"
OVERLAY = ROOT / "ui/unified-ui/src/routes/contextual-assist/+page.svelte"
EXTENSION_CAPTURE = ROOT / "magicutor/extension/notes_capture_menu.js"
NOTES_RS = ROOT / "magician/src/magician_v2/notes.rs"
NOTES_PAGE = ROOT / "ui/unified-ui/src/routes/(app)/notes/+page.svelte"

TAURI_ACTION = re.compile(r'const ACTION_SAVE_TO_NOTES\s*:\s*&str\s*=\s*"([a-z_]+)"')
OVERLAY_ACTION = re.compile(r"const SAVE_TO_NOTES_ACTION_ID\s*=\s*'([a-z_]+)'")

BACKEND_SEARCH_CAP = re.compile(r"const MAX_NOTE_SEARCH_LIMIT\s*:\s*usize\s*=\s*(\d+)\s*;")
PAGE_SEARCH_CAP = re.compile(r"const WRITTEN_SEARCH_MAX\s*=\s*(\d+)\s*;")


def read(path: Path) -> str:
    if not path.exists():
        raise SystemExit(f"missing required file: {path.relative_to(ROOT)}")
    return path.read_text(encoding="utf-8")


def one(pattern: re.Pattern[str], text: str) -> str | None:
    match = pattern.search(text)
    return None if match is None else match.group(1)


def check_action_id(failures: list[str]) -> None:
    """The overlay's button id must be the one the desktop catalog publishes."""
    tauri = one(TAURI_ACTION, read(TAURI))
    overlay = one(OVERLAY_ACTION, read(OVERLAY))

    if tauri is None:
        failures.append(
            "could not find `ACTION_SAVE_TO_NOTES` in desktop/src-tauri/src/contextual_assist.rs"
        )
    if overlay is None:
        failures.append(
            "could not find `SAVE_TO_NOTES_ACTION_ID` in the contextual-assist overlay"
        )
    if tauri and overlay and tauri != overlay:
        failures.append(
            f"save-to-notes action id disagrees: Tauri publishes '{tauri}', "
            f"the overlay matches '{overlay}' — the button would fall through to "
            "the generic model path instead of capturing"
        )


def check_capture_auth(failures: list[str]) -> None:
    """Browser capture must use bearer auth and must not assert scope."""
    extension_text = read(EXTENSION_CAPTURE)
    if "import { magicianFetch } from './magician_scope.js';" not in extension_text:
        failures.append("notes capture no longer imports the bearer-aware magicianFetch helper")
    if "await magicianFetch(" not in extension_text:
        failures.append("notes capture bypasses magicianFetch and would omit its bearer")
    if re.search(r"\b(?:principal|workspace)\s*:", extension_text):
        failures.append("notes capture asserts principal/workspace in its request body")


def check_search_cap(failures: list[str]) -> None:
    """The page must not offer more results than the backend will ever return."""
    backend = one(BACKEND_SEARCH_CAP, read(NOTES_RS))
    page = one(PAGE_SEARCH_CAP, read(NOTES_PAGE))

    if backend is None:
        failures.append("could not find `MAX_NOTE_SEARCH_LIMIT` in magician/src/magician_v2/notes.rs")
    if page is None:
        failures.append("could not find `WRITTEN_SEARCH_MAX` in the /notes page")
    if backend and page and int(page) > int(backend):
        failures.append(
            f"the /notes search cap ({page}) exceeds what the backend will return "
            f"({backend}) — Show more would keep offering results that never arrive"
        )


def run() -> list[str]:
    failures: list[str] = []
    check_action_id(failures)
    check_capture_auth(failures)
    check_search_cap(failures)
    return failures


def main() -> int:
    failures = run()
    if failures:
        print("notes capture surface drift:", file=sys.stderr)
        for failure in failures:
            print(f"  - {failure}", file=sys.stderr)
        return 1
    print(
        "notes surface guard passed: the save-to-notes action id, bearer-only "
        "capture boundary, and search result cap agree across surfaces"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
