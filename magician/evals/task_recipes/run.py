#!/usr/bin/env python3
"""Run the live Task Recipes cases against the running stack.

    python3 run.py --cases p1,p2,p3,p4
    python3 run.py --cases p6            # opt-in, signed-in write case

No build. Edit a case in cases.py and re-run. Exits 0 iff every selected case
passed; writes a report.json next to --output.
"""

from __future__ import annotations

import argparse
import dataclasses
import json
import os
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import cases as C  # noqa: E402
import harness as H  # noqa: E402


def parse_args() -> argparse.Namespace:
    root_default = os.environ.get("MAGICIAN_ROOT_DIR") or str(Path.home() / "MagicianNotes")
    p = argparse.ArgumentParser(description="Task Recipes live eval (Python harness)")
    p.add_argument("--base-url", default="http://127.0.0.1:3002")
    p.add_argument("--magicutor-base-url", default="http://127.0.0.1:3003")
    p.add_argument("--runtime-root", default=root_default)
    p.add_argument("--magician-bin", default="magician.bin")
    p.add_argument("--output", default="/Volumes/build/magician/coverage/evals/task-recipes/py")
    p.add_argument("--timeout-secs", type=int, default=600)
    p.add_argument("--cases", default=",".join(C.ALL_CASES))
    p.add_argument("--workspace-slug", default=None)
    p.add_argument("--keep-scope", action="store_true",
                   help="keep this run's workspace even if it passes")
    return p.parse_args()


def main() -> int:
    args = parse_args()
    selected = [c.strip() for c in args.cases.split(",") if c.strip()]
    known = set(C.ALL_CASES) | set(C.OPT_IN_CASES)
    unknown = [c for c in selected if c not in known]
    if unknown:
        print(f"unknown case(s): {unknown}; known: {sorted(known)}", file=sys.stderr)
        return 64

    slug = args.workspace_slug or f"recipes-eval-py{int(time.time() * 1000) % 100000000:08d}"
    root = Path(args.runtime_root)
    m = H.Magician(args.base_url, args.magicutor_base_url, root)
    m.connect(slug)
    already = m.seal(Path(args.magician_bin))
    # Mark the scope so scripts/purge_task_recipes_eval_workspaces.sh sweeps it
    # too (it removes only recipes-eval-* dirs carrying this marker).
    try:
        sd = m.scope_dir()
        sd.mkdir(parents=True, exist_ok=True)
        (sd / ".task-recipes-fixture-eval").touch()
    except Exception:
        pass
    print(f"task_recipes(py): scope {m.principal}/{m.workspace} · cases {','.join(selected)} · sealed(already={already})")
    purged = _purge_previous_workspaces(m, root)
    if purged:
        print(f"task_recipes(py): purged earlier eval workspaces {', '.join(purged)} (data removed at the next server start)")

    results = []
    for cid in selected:
        print(f"▶ case {cid}")
        case = C.run_case(cid, m, args.timeout_secs)
        for ph in case.phases:
            failed = [g.id for g in ph.gates if not g.passed]
            mark = "pass" if ph.passed() else "FAIL"
            tail = f" · failed: {','.join(failed)}" if failed else ""
            err = f" · error: {ph.error}" if ph.error else ""
            print(f"   {mark} {ph.id:<16} {ph.outcome_type or ph.status} · {ph.summary_excerpt[:70]}{tail}{err}")
        if case.error:
            print(f"   case error: {case.error}")
        results.append(case)

    all_passed = bool(results) and all(c.passed for c in results)
    report = {
        "passed": all_passed,
        "generated_at": time.strftime("%Y-%m-%dT%H:%M:%S"),
        "principal": m.principal,
        "workspace": m.workspace,
        "cases": [_case_json(c) for c in results],
    }
    out_dir = Path(args.output)
    out_dir.mkdir(parents=True, exist_ok=True)
    (out_dir / "report.json").write_text(json.dumps(report, indent=2))
    print(f"task_recipes(py): {'PASSED' if all_passed else 'FAILED'} · report {out_dir / 'report.json'}")

    # A passed run's workspace has no further use; a failed one is kept for
    # diagnosis and purged by the next run's start-up sweep. A workspace the
    # operator named (--workspace-slug) is theirs to keep.
    if all_passed and not args.keep_scope and not args.workspace_slug:
        try:
            m.purge_workspace(m.workspace)
            print(f"task_recipes(py): purged {m.workspace} (data removed at the next server start)")
        except Exception as exc:  # the report is already written; don't mask it
            print(f"task_recipes(py): {m.workspace} not purged: {exc}", file=sys.stderr)
    return 0 if all_passed else 1


WORKSPACE_PREFIX = "recipes-eval"
HARNESS_MARKER = ".task-recipes-fixture-eval"


def _is_harness_workspace(workspace: str) -> bool:
    # The bare prefix counts too; a `recipes-eval-` prefix test never matched
    # it, so that one survived every cleanup.
    return workspace == WORKSPACE_PREFIX or workspace.startswith(WORKSPACE_PREFIX + "-")


def _purge_previous_workspaces(m: "H.Magician", root: Path) -> list[str]:
    """Purge earlier harness workspaces so at most the current one remains.
    Only a workspace whose directory is gone or carries the harness marker is
    touched — one an operator populated by hand is left alone."""
    principal_dir = root / "scopes" / m.principal
    purged = []
    for workspace in m.list_workspaces():
        if workspace == m.workspace or not _is_harness_workspace(workspace):
            continue
        d = principal_dir / workspace
        if d.exists() and not (d / HARNESS_MARKER).exists():
            print(f"task_recipes(py): kept {workspace} (no harness marker)")
            continue
        try:
            m.purge_workspace(workspace)
            purged.append(workspace)
        except Exception as exc:
            print(f"task_recipes(py): {workspace} not purged: {exc}", file=sys.stderr)
    return purged


def _case_json(c: C.Case) -> dict:
    return {
        "id": c.id, "site": c.site, "origin": c.origin, "passed": c.passed, "error": c.error,
        "phases": [{
            "id": p.id, "task_id": p.task_id, "execution_id": p.execution_id,
            "status": p.status, "outcome_type": p.outcome_type, "summary_excerpt": p.summary_excerpt,
            "error": p.error,
            "gates": [dataclasses.asdict(g) for g in p.gates],
        } for p in c.phases],
    }


if __name__ == "__main__":
    sys.exit(main())
