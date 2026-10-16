#!/usr/bin/env python3
"""Backfill `task_summary.json` sidecars for already-completed tasks.

The Rust forward path writes a cached, readable task-result summary
(`task_summary.json`) beside each task's `out_task_user_*` deliverable in an
exactly-once background revision after the primary answer is ready.
The chat result card + the `/tasks` listing prefer that sidecar.

This script populates the sidecar for tasks that completed BEFORE the forward
path existed. It mirrors the Rust logic exactly:

  - find the newest `out_task_user_*` per `.../outputs/` dir (the primary output),
  - strip HTML to text (same approach as `strip_html_to_text` in Rust),
  - gate on length (> 250 chars) OR noisy (non-alphanumeric ratio > 0.35),
  - if gated: summarize via the configured `task_summary` Ollama profile
    using the SAME store prompt the Rust op uses; tag `generated_by: "llm"`,
  - else (or on LLM error): store the verbatim (truncated) text, tag
    `generated_by: "verbatim"`,
  - write `<outputs_dir>/task_summary.json` = {text, generated_by, source_output_id}.

`source_output_id` is the deliverable's filename stem, which is exactly the
`OutputRef.output_id` the Rust `/tasks` reader matches against.

Idempotent: skips dirs that already have a `task_summary.json` (use --force to
regenerate). Best-effort per task: one failure never aborts the run.

Run AFTER the magician binary that ships the forward path is built/deployed, so
the new sidecars are actually consumed. The store prompt + the model are read
from the same sources as the runtime, so this stays in sync with config.

Examples:
  # default root ($MAGICIAN_ROOT_DIR / $MAGICIAN_STORAGE_PATH / ~/MagicianNotes)
  python3 scripts/backfill_task_summaries.py
  # dry run first
  python3 scripts/backfill_task_summaries.py --dry-run
  # explicit legacy Ollama maintenance pass (normal runtime mapping may be remote)
  python3 scripts/backfill_task_summaries.py --root ~/MagicianNotes \
    --model gemma4:12b --context-tokens 32768 \
    --ollama-url http://localhost:11434/api/generate
"""

from __future__ import annotations

import argparse
import html as html_module
import json
import os
import re
import sys
import time
import urllib.error
import urllib.request

from ollama_config import resolve_operation
from pathlib import Path

# Mirror the Rust constants (artifact_v2/service.rs).
SIDECAR_NAME = "task_summary.json"
DELIVERABLE_PREFIX = "out_task_user_"
LLM_INPUT_CAP = 16_000      # cap fed to the local model (latency/context budget)
VERBATIM_CAP = 4_000        # cap stored when not LLM-summarized
DEFAULT_PROMPT_REL = "data/magician_v2/prompts/task_summary_system_v1.0.0.json"
PROMPT_FALLBACK = (
    "You are a task assistant. Summarize the completed task's deliverable text "
    "for the requester: 1-2 sentences on what was produced or found, then up to "
    "4 short bullets of key results. Be faithful to the content; do not invent. "
    "Under ~120 words."
)

_SCRIPT_RE = re.compile(r"<script\b[^>]*>.*?</script>", re.IGNORECASE | re.DOTALL)
_STYLE_RE = re.compile(r"<style\b[^>]*>.*?</style>", re.IGNORECASE | re.DOTALL)
_TAG_RE = re.compile(r"<[^>]+>")
_WS_RE = re.compile(r"\s+")


def strip_html_to_text(raw: str) -> str:
    """Lossy HTML -> readable text. Matches the Rust `strip_html_to_text`."""
    out = _SCRIPT_RE.sub(" ", raw)
    out = _STYLE_RE.sub(" ", out)
    out = _TAG_RE.sub(" ", out)
    out = html_module.unescape(out)
    return _WS_RE.sub(" ", out).strip()


def truncate(text: str, max_len: int) -> str:
    if len(text) <= max_len:
        return text
    return text[: max(0, max_len - 3)] + "..."


def needs_llm(text: str, min_chars: int, noisy_ratio: float) -> bool:
    """Cheap gate (no LLM): long OR noisy. Matches the Rust `task_summary_needs_llm`."""
    t = text.strip()
    n = len(t)
    if n > min_chars:
        return True
    if n == 0:
        return False
    non_alnum = sum(1 for c in t if not c.isalnum() and not c.isspace())
    return (non_alnum / n) > noisy_ratio


def load_prompt(prompt_file: Path) -> str:
    try:
        data = json.loads(prompt_file.read_text(encoding="utf-8"))
        content = data.get("content")
        if isinstance(content, list):
            return " ".join(str(line) for line in content).strip() or PROMPT_FALLBACK
        if isinstance(content, str) and content.strip():
            return content.strip()
    except Exception as exc:  # noqa: BLE001 - best effort, fall back loudly
        print(f"  ! could not read prompt {prompt_file}: {exc} — using fallback", file=sys.stderr)
    return PROMPT_FALLBACK


def ollama_generate(
    url: str,
    model: str,
    context_tokens: int,
    system: str,
    prompt: str,
    timeout: int,
) -> str:
    body = json.dumps(
        {
            "model": model,
            "system": system,
            "prompt": prompt,
            "stream": False,
            "options": {"num_ctx": context_tokens},
        }
    ).encode("utf-8")
    req = urllib.request.Request(url, data=body, headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        payload = json.loads(resp.read().decode("utf-8"))
    return (payload.get("response") or "").strip()


def default_root() -> Path:
    for env in ("MAGICIAN_ROOT_DIR", "MAGICIAN_STORAGE_PATH"):
        val = os.environ.get(env)
        if val:
            return Path(val).expanduser()
    home = os.environ.get("HOME")
    return Path(home, "MagicianNotes") if home else Path("MagicianNotes")


def newest_deliverable_per_outputs_dir(root: Path) -> dict[Path, Path]:
    """Map each `.../outputs/` dir -> its newest `out_task_user_*` deliverable."""
    best: dict[Path, Path] = {}
    for path in root.rglob(f"{DELIVERABLE_PREFIX}*"):
        if not path.is_file() or path.parent.name != "outputs":
            continue
        if path.name == SIDECAR_NAME:
            continue
        cur = best.get(path.parent)
        if cur is None or path.stat().st_mtime > cur.stat().st_mtime:
            best[path.parent] = path
    return best


def atomic_write_json(path: Path, obj: dict) -> None:
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(json.dumps(obj, ensure_ascii=False, indent=2), encoding="utf-8")
    os.replace(tmp, path)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--root", type=Path, default=default_root(),
                        help="Runtime data root to walk (default: $MAGICIAN_ROOT_DIR / $MAGICIAN_STORAGE_PATH / ~/MagicianNotes).")
    parser.add_argument("--model", default=None, help="Explicit Ollama model override.")
    parser.add_argument("--context-tokens", type=int, default=None, help="Explicit Ollama context override.")
    parser.add_argument("--ollama-url", default=None, help="Explicit Ollama /api/generate endpoint override.")
    parser.add_argument("--prompt-file", type=Path, default=None,
                        help=f"Store prompt JSON (default: <repo>/{DEFAULT_PROMPT_REL}).")
    parser.add_argument("--min-chars", type=int, default=250, help="Length gate for LLM summarization (default: 250).")
    parser.add_argument("--noisy-ratio", type=float, default=0.35, help="Non-alphanumeric ratio gate (default: 0.35).")
    parser.add_argument("--timeout", type=int, default=180, help="Per-LLM-call timeout seconds (default: 180).")
    parser.add_argument("--sleep", type=float, default=0.0, help="Delay between LLM calls, seconds (rate-limit).")
    parser.add_argument("--limit", type=int, default=0, help="Process at most N tasks (0 = all).")
    parser.add_argument("--force", action="store_true", help="Regenerate even if a task_summary.json already exists.")
    parser.add_argument("--dry-run", action="store_true", help="Show what would happen; write nothing, call no LLM.")
    args = parser.parse_args()

    root: Path = args.root.expanduser()
    if not root.is_dir():
        print(f"error: root not found: {root}", file=sys.stderr)
        return 2

    repo_root = Path(__file__).resolve().parent.parent
    if not (args.model and args.context_tokens and args.ollama_url):
        try:
            configured_model, configured_context, configured_endpoint = resolve_operation(
                repo_root, "task_summary"
            )
        except RuntimeError as exc:
            parser.error(
                f"{exc}; task_summary may be mapped to a remote profile. "
                "Use Magician startup recovery, or provide --model, "
                "--context-tokens, and --ollama-url for this legacy local pass."
            )
        args.model = args.model or configured_model
        args.context_tokens = args.context_tokens or configured_context
        args.ollama_url = args.ollama_url or configured_endpoint
    prompt_file = args.prompt_file or (repo_root / DEFAULT_PROMPT_REL)
    system_prompt = load_prompt(prompt_file)

    print(f"root        : {root}")
    print(f"model       : {args.model}  ({args.ollama_url})")
    print(f"prompt      : {prompt_file}")
    print(f"gate        : > {args.min_chars} chars OR non-alnum ratio > {args.noisy_ratio}")
    print(f"mode        : {'DRY RUN' if args.dry_run else 'WRITE'}{' (force)' if args.force else ''}")

    deliverables = newest_deliverable_per_outputs_dir(root)
    print(f"found       : {len(deliverables)} task output dir(s)\n")

    processed = wrote = skipped = llm = verbatim = errors = 0
    for outputs_dir, deliverable in sorted(deliverables.items(), key=lambda kv: str(kv[0])):
        if args.limit and processed >= args.limit:
            break
        sidecar = outputs_dir / SIDECAR_NAME
        rel = outputs_dir.relative_to(root)
        if sidecar.exists() and not args.force:
            skipped += 1
            continue
        processed += 1
        try:
            raw = deliverable.read_text(encoding="utf-8", errors="replace")
        except Exception as exc:  # noqa: BLE001
            print(f"  ! {rel}: read failed: {exc}")
            errors += 1
            continue
        readable = strip_html_to_text(raw) if deliverable.suffix.lower() == ".html" else raw
        readable = readable.strip()
        if not readable:
            print(f"  - {rel}: empty deliverable, skipped")
            skipped += 1
            continue

        if needs_llm(readable, args.min_chars, args.noisy_ratio):
            if args.dry_run:
                print(f"  ~ {rel}: would LLM-summarize ({len(readable)} chars) [{deliverable.name}]")
                llm += 1
                continue
            try:
                summary = ollama_generate(
                    args.ollama_url,
                    args.model,
                    args.context_tokens,
                    system_prompt,
                    truncate(readable, LLM_INPUT_CAP),
                    args.timeout,
                )
            except (urllib.error.URLError, TimeoutError, Exception) as exc:  # noqa: BLE001
                print(f"  ! {rel}: LLM failed ({exc}) — storing verbatim")
                summary = ""
            if summary:
                text, generated_by = summary, "llm"
                llm += 1
            else:
                text, generated_by = truncate(readable, VERBATIM_CAP), "verbatim"
                verbatim += 1
            if args.sleep:
                time.sleep(args.sleep)
        else:
            text, generated_by = readable, "verbatim"
            verbatim += 1
            if args.dry_run:
                print(f"  ~ {rel}: would store verbatim ({len(readable)} chars)")
                continue

        record = {"text": text, "generated_by": generated_by, "source_output_id": deliverable.stem}
        if args.dry_run:
            continue
        try:
            atomic_write_json(sidecar, record)
            wrote += 1
            print(f"  + {rel}: {generated_by} ({len(text)} chars)")
        except Exception as exc:  # noqa: BLE001
            print(f"  ! {rel}: write failed: {exc}")
            errors += 1

    print(
        f"\ndone: processed={processed} wrote={wrote} skipped(existing/empty)={skipped} "
        f"llm={llm} verbatim={verbatim} errors={errors}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
