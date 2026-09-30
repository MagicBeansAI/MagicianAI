#!/usr/bin/env python3
"""Repair the decision links in the attention learning store.

An outcome records which candidate the owner acted on; the decision ledger
records which candidates were served and when. The two were never joined
because they name candidates differently: an outcome stores the raw id
(``000a167d-...``) while a decision item stores the canonical form
(``follow_up:000a167d-...``). A naive join returns zero rows, which is why
every rank-recompute job went stale with ``candidate_inactive`` and why 91% of
them reported no decision.

This script reconstructs the link from the serving ledger itself: for each
unlinked outcome, the most recent decision at or before the outcome that
actually contained that candidate. It then resubmits the rank-recompute jobs
that can now resolve, and gives an honest terminal reason to the ones that
never will.

What it deliberately does NOT do:

* It never deletes a row. Outcomes are the label set, and an outcome without a
  decision is still a perfectly good label -- especially while every recorded
  ``selection_probability`` is 1.0, which makes propensity weighting a no-op.
* It never invents ``impression_id`` or ``delivery_id``. A decision is a server
  fact and can be recovered; an impression is a claim about what a client
  actually put on screen, and fabricating one would launder an assumption into
  the ledger that downstream code is explicitly written to distrust.
* It only resubmits jobs that went stale as ``candidate_inactive`` -- the
  linkage bug's own signature. A historical job that has since reached
  ``universe_changed_during_commit`` gave an honest verdict: the universe really
  did move on, and no amount of resubmission changes that. Resubmitting it would
  only reproduce the same result on every run.

It also re-files captured feature vectors that were stored under the decision's
surface rather than the lane they were served into. A decision spans the whole
cross-lane universe and its surface is always ``follow_up``, so Worth-a-look
vectors landed under the wrong lane and became unreachable to the outcomes that
need them. The candidate id carries the true lane as a prefix, so this is
repairable rather than lost.

Read-only by default. Pass --apply to write, which also records an undo file
containing the prior value of every row it touches.

    python3 scripts/repair_attention_decision_links.py
    python3 scripts/repair_attention_decision_links.py --apply
    python3 scripts/repair_attention_decision_links.py --undo <undo-file>
"""

from __future__ import annotations

import argparse
import json
import os
import sqlite3
import sys
import time

DEFAULT_DB = os.path.expanduser("~/MagicianNotes/attention_learning.db")

# Reasons the worker itself writes. `candidate_inactive` means "the current
# universe no longer holds this candidate", which was the symptom of the
# missing link rather than a real verdict. `no_served_decision` is terminal and
# honest: nothing was ever served that we can point at.
REASON_UNRESOLVABLE = "no_served_decision"

# The reconstruction. Correlated rather than a join so each outcome resolves
# against the decisions that preceded *it*; ordering by decided_at DESC picks
# the state the owner was actually looking at, not merely some decision that
# once contained the candidate.
RECONSTRUCT_SQL = """
SELECT o.outcome_id,
       o.principal,
       o.workspace,
       (SELECT i.decision_id
          FROM attention_decision_items i
          JOIN attention_decisions d ON d.decision_id = i.decision_id
         WHERE i.principal = o.principal
           AND i.workspace = o.workspace
           AND i.candidate_id = o.surface || ':' || o.candidate_id
           AND d.decided_at <= o.occurred_at
         ORDER BY d.decided_at DESC
         LIMIT 1) AS served_decision_id
  FROM attention_outcomes o
 WHERE o.decision_id IS NULL
"""


def connect(path: str, writable: bool) -> sqlite3.Connection:
    if not os.path.exists(path):
        sys.exit(f"no attention learning store at {path}")
    uri = f"file:{path}" + ("" if writable else "?mode=ro")
    conn = sqlite3.connect(uri, uri=True, timeout=60)
    # The server owns this file and writes to it continuously. Wait for the
    # write lock rather than failing the run half-applied.
    conn.execute("PRAGMA busy_timeout = 60000")
    return conn


# Captured vectors were briefly filed under the *decision's* surface. A decision
# spans the whole cross-lane universe and its surface is always `follow_up`, so
# every Worth-a-look vector landed under the wrong lane -- and an outcome joins
# features on its own surface, making those rows unreachable. The candidate id
# carries the true lane as its prefix, so this is recoverable rather than lost.
MISFILED_LANE_SQL = """
SELECT COUNT(*) FROM attention_candidate_feature_snapshots
 WHERE instr(candidate_id, ':') > 0
   AND substr(candidate_id, 1, instr(candidate_id, ':') - 1) <> surface
"""

# Once serving files vectors correctly, a mislabelled row usually has a correct
# twin: the same candidate at the same revision, re-captured under its real
# lane. Features are revision-bound, so the twin carries identical content and
# the stale row is pure duplicate -- re-filing it would collide on the primary
# key. Drop those, re-file the ones with no twin.
REDUNDANT_MISFILED_SQL = """
SELECT rowid, surface, candidate_id, source_revision, feature_contract,
       semantic_extractor_contract, semantic_prompt_version, features_json,
       first_served_at
  FROM attention_candidate_feature_snapshots m
 WHERE instr(m.candidate_id, ':') > 0
   AND substr(m.candidate_id, 1, instr(m.candidate_id, ':') - 1) <> m.surface
   AND EXISTS (
         SELECT 1 FROM attention_candidate_feature_snapshots f
          WHERE f.surface = substr(m.candidate_id, 1, instr(m.candidate_id, ':') - 1)
            AND f.candidate_id = m.candidate_id
            AND f.source_revision = m.source_revision)
"""

DROP_REDUNDANT_MISFILED_SQL = """
DELETE FROM attention_candidate_feature_snapshots
 WHERE rowid IN (%s)
"""

REFILE_LANE_SQL = """
UPDATE attention_candidate_feature_snapshots
   SET surface = substr(candidate_id, 1, instr(candidate_id, ':') - 1)
 WHERE instr(candidate_id, ':') > 0
   AND substr(candidate_id, 1, instr(candidate_id, ':') - 1) <> surface
"""


# A job whose served universe has been superseded can never commit: the worker
# compares the served universe digest against the current one and refuses. It
# reaches that verdict only AFTER loading and parsing the served projection
# (~841KB each), so a backlog of superseded jobs costs a full parse per job to
# learn nothing -- and with a 120s lease, a batch that runs long loses its lease
# at commit, which aborts the whole pass. Writing the verdict directly is the
# same outcome the worker would reach, minus the thrash.
SUPERSEDED_SQL = """
SELECT COUNT(*)
  FROM attention_rank_recompute_jobs j
  JOIN attention_decisions d ON d.decision_id = j.decision_id
 WHERE j.status IN ('pending', 'in_flight', 'retry')
   AND d.candidate_set_digest <> (
         SELECT candidate_set_digest FROM attention_decisions
          ORDER BY decided_at DESC LIMIT 1)
"""

DRAIN_SUPERSEDED_SQL = """
UPDATE attention_rank_recompute_jobs
   SET status = 'stale', reason = 'universe_changed_during_commit',
       lease_owner = NULL, lease_expires_at = NULL, next_retry_at = NULL,
       updated_at = ?
 WHERE status IN ('pending', 'in_flight', 'retry')
   AND decision_id IN (
         SELECT d.decision_id FROM attention_decisions d
          WHERE d.candidate_set_digest <> (
                SELECT candidate_set_digest FROM attention_decisions
                 ORDER BY decided_at DESC LIMIT 1))
"""


def snapshot(conn: sqlite3.Connection) -> dict:
    def one(sql: str):
        return conn.execute(sql).fetchone()

    outcomes = one(
        "SELECT COUNT(*), SUM(decision_id IS NOT NULL) FROM attention_outcomes"
    )
    jobs = one(
        "SELECT COUNT(*), SUM(decision_id IS NOT NULL) FROM attention_rank_recompute_jobs"
    )
    by_status = conn.execute(
        "SELECT status, COALESCE(reason,'-'), COUNT(*) "
        "FROM attention_rank_recompute_jobs GROUP BY 1,2 ORDER BY 3 DESC"
    ).fetchall()
    misfiled = one(MISFILED_LANE_SQL)[0]
    captured = one("SELECT COUNT(*) FROM attention_candidate_feature_snapshots")[0]
    return {
        "captured_vectors": captured,
        "misfiled_lane": misfiled,
        "outcomes_total": outcomes[0],
        "outcomes_linked": outcomes[1] or 0,
        "jobs_total": jobs[0],
        "jobs_linked": jobs[1] or 0,
        "jobs_by_status": by_status,
    }


def report(title: str, state: dict) -> None:
    print(f"\n{title}")
    print(
        f"  outcomes            {state['outcomes_linked']:>6} / {state['outcomes_total']} carry a decision"
    )
    print(
        f"  rank_recompute jobs {state['jobs_linked']:>6} / {state['jobs_total']} carry a decision"
    )
    for status, reason, count in state["jobs_by_status"]:
        print(f"    {status:<10} {reason:<28} {count}")
    print(
        f"  captured vectors    {state['captured_vectors']:>6}"
        f"  ({state['misfiled_lane']} filed under the wrong lane)"
    )


def reconstruct(conn: sqlite3.Connection) -> tuple[list[tuple[str, str, str, str]], int]:
    started = time.time()
    rows = conn.execute(RECONSTRUCT_SQL).fetchall()
    elapsed = time.time() - started
    resolved = [
        (outcome_id, principal, workspace, decision_id)
        for outcome_id, principal, workspace, decision_id in rows
        if decision_id
    ]
    print(
        f"\nscanned {len(rows)} unlinked outcomes in {elapsed:.1f}s; "
        f"{len(resolved)} resolve to the decision that served them, "
        f"{len(rows) - len(resolved)} have no serving decision and never will"
    )
    return resolved, len(rows) - len(resolved)


def apply(conn: sqlite3.Connection, resolved: list, undo_path: str) -> None:
    now_ms = int(time.time() * 1000)
    outcome_ids = [row[0] for row in resolved]

    undo = {
        "db_written_at_ms": now_ms,
        "outcomes": [],
        "jobs": [],
        "dropped_feature_rows": [],
    }

    cur = conn.cursor()
    cur.execute("BEGIN IMMEDIATE")
    try:
        # Capture prior state before touching anything, so the undo file is a
        # real inverse rather than an assumption about what was there.
        placeholders = ",".join("?" * len(outcome_ids))
        if outcome_ids:
            undo["outcomes"] = [
                {"outcome_id": r[0], "decision_id": r[1]}
                for r in cur.execute(
                    f"SELECT outcome_id, decision_id FROM attention_outcomes "
                    f"WHERE outcome_id IN ({placeholders})",
                    outcome_ids,
                ).fetchall()
            ]
            undo["jobs"] = [
                {
                    "job_id": r[0],
                    "decision_id": r[1],
                    "status": r[2],
                    "reason": r[3],
                    "attempts": r[4],
                    "next_retry_at": r[5],
                }
                for r in cur.execute(
                    f"SELECT job_id, decision_id, status, reason, attempts, next_retry_at "
                    f"FROM attention_rank_recompute_jobs "
                    f"WHERE outcome_id IN ({placeholders})",
                    outcome_ids,
                ).fetchall()
            ]

        # 1. The outcome learns which decision served it.
        cur.executemany(
            "UPDATE attention_outcomes SET decision_id = ? "
            " WHERE outcome_id = ? AND decision_id IS NULL",
            [(decision_id, outcome_id) for outcome_id, _, _, decision_id in resolved],
        )
        outcomes_written = cur.rowcount

        # 2. The job reads its own decision column, not the outcome's, so it
        #    needs the same value or resubmitting it changes nothing.
        cur.executemany(
            "UPDATE attention_rank_recompute_jobs SET decision_id = ?, updated_at = ? "
            " WHERE outcome_id = ? AND decision_id IS NULL",
            [
                (decision_id, now_ms, outcome_id)
                for outcome_id, _, _, decision_id in resolved
            ],
        )
        jobs_linked = cur.rowcount

        # 3. Resubmit only jobs that went stale *because of the linkage bug*.
        #
        #    Scoped to `candidate_inactive` deliberately. A job that has since
        #    failed for a real reason -- `universe_changed_during_commit` above
        #    all -- reached an honest verdict, and resubmitting it would simply
        #    reproduce that verdict. Since this script is re-runnable, an
        #    unscoped resubmit would put those jobs in a permanent loop: fail,
        #    get resubmitted, fail again, forever.
        cur.execute(
            "UPDATE attention_rank_recompute_jobs "
            "   SET status = 'pending', attempts = 0, next_retry_at = NULL, "
            "       reason = NULL, lease_owner = NULL, lease_expires_at = NULL, "
            "       updated_at = ? "
            " WHERE status = 'stale' AND decision_id IS NOT NULL "
            "   AND reason = 'candidate_inactive'",
            (now_ms,),
        )
        jobs_resubmitted = cur.rowcount

        # 4. Re-file any vector stored under the decision's surface instead of
        #    the lane it was served into. Idempotent: the guard only matches
        #    rows whose prefix disagrees with the stored surface.
        redundant = cur.execute(REDUNDANT_MISFILED_SQL).fetchall()
        undo["dropped_feature_rows"] = [list(row[1:]) for row in redundant]
        if redundant:
            ids = ",".join(str(row[0]) for row in redundant)
            cur.execute(DROP_REDUNDANT_MISFILED_SQL % ids)
        vectors_dropped = cur.rowcount if redundant else 0
        cur.execute(REFILE_LANE_SQL)
        vectors_refiled = cur.rowcount

        # 5. Everything else is unresolvable, and should say so. Left `stale`
        #    on purpose: it is terminal, and the point is to make the backlog
        #    legible rather than to keep retrying something that cannot work.
        cur.execute(
            "UPDATE attention_rank_recompute_jobs "
            "   SET reason = ?, updated_at = ? "
            " WHERE status = 'stale' AND decision_id IS NULL AND reason IS NOT ?",
            (REASON_UNRESOLVABLE, now_ms, REASON_UNRESOLVABLE),
        )
        jobs_marked = cur.rowcount

        # Only commit once the undo file is durable on disk. A repair with no
        # way back is worse than no repair, and a buffered undo file that dies
        # with the process is exactly that.
        with open(undo_path, "w", encoding="utf-8") as handle:
            json.dump(undo, handle)
            handle.flush()
            os.fsync(handle.fileno())
        conn.commit()
    except Exception:
        conn.rollback()
        raise

    print(
        f"\napplied:\n"
        f"  outcomes linked      {outcomes_written}\n"
        f"  jobs linked          {jobs_linked}\n"
        f"  jobs resubmitted     {jobs_resubmitted}\n"
        f"  jobs marked {REASON_UNRESOLVABLE:<12} {jobs_marked}\n"
        f"  vectors re-filed     {vectors_refiled}\n"
        f"  duplicates dropped   {vectors_dropped}\n"
        f"  undo file            {undo_path}"
    )


def undo(conn: sqlite3.Connection, undo_path: str) -> None:
    with open(undo_path, encoding="utf-8") as handle:
        saved = json.load(handle)
    cur = conn.cursor()
    cur.execute("BEGIN IMMEDIATE")
    try:
        cur.executemany(
            "UPDATE attention_outcomes SET decision_id = ? WHERE outcome_id = ?",
            [(row["decision_id"], row["outcome_id"]) for row in saved["outcomes"]],
        )
        cur.executemany(
            "INSERT OR IGNORE INTO attention_candidate_feature_snapshots ("
            " surface, candidate_id, source_revision, feature_contract,"
            " semantic_extractor_contract, semantic_prompt_version, features_json,"
            " first_served_at, principal, workspace)"
            " VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'anonymous', 'default')",
            saved.get("dropped_feature_rows", []),
        )
        cur.executemany(
            "UPDATE attention_rank_recompute_jobs "
            "   SET decision_id = ?, status = ?, reason = ?, attempts = ?, next_retry_at = ? "
            " WHERE job_id = ?",
            [
                (
                    row["decision_id"],
                    row["status"],
                    row["reason"],
                    row["attempts"],
                    row["next_retry_at"],
                    row["job_id"],
                )
                for row in saved["jobs"]
            ],
        )
        conn.commit()
    except Exception:
        conn.rollback()
        raise
    print(
        f"reverted {len(saved['outcomes'])} outcomes and {len(saved['jobs'])} jobs "
        f"to their state before {saved['db_written_at_ms']}"
    )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--db", default=DEFAULT_DB)
    parser.add_argument(
        "--apply", action="store_true", help="write; omit for a read-only report"
    )
    parser.add_argument("--undo", metavar="UNDO_FILE", help="revert a previous --apply")
    parser.add_argument(
        "--drain-superseded",
        action="store_true",
        help="give the terminal verdict to jobs whose served universe has moved on",
    )
    args = parser.parse_args()

    if args.undo:
        conn = connect(args.db, writable=True)
        report("before", snapshot(conn))
        undo(conn, args.undo)
        report("after", snapshot(conn))
        return

    conn = connect(args.db, writable=args.apply or args.drain_superseded)
    report("current", snapshot(conn))

    if args.drain_superseded:
        doomed = conn.execute(SUPERSEDED_SQL).fetchone()[0]
        print(f"\n{doomed} queued jobs were served from a superseded universe")
        if not args.apply:
            print("read-only. re-run with --apply --drain-superseded to write.")
            return
        cur = conn.cursor()
        cur.execute("BEGIN IMMEDIATE")
        try:
            cur.execute(DRAIN_SUPERSEDED_SQL, (int(time.time() * 1000),))
            drained = cur.rowcount
            conn.commit()
        except Exception:
            conn.rollback()
            raise
        print(f"drained {drained} to stale/universe_changed_during_commit")
        report("after", snapshot(conn))
        return
    resolved, unresolvable = reconstruct(conn)

    if not args.apply:
        for outcome_id, _, _, decision_id in resolved[:3]:
            print(f"  e.g. outcome {outcome_id} <- decision {decision_id}")
        print(
            f"\nread-only. {len(resolved)} outcomes and their jobs would be linked and "
            f"resubmitted; {unresolvable} would be marked {REASON_UNRESOLVABLE}."
            f"\nre-run with --apply to write."
        )
        return

    undo_path = os.path.join(
        os.path.dirname(os.path.abspath(args.db)),
        f"attention_decision_link_undo_{int(time.time())}.json",
    )
    apply(conn, resolved, undo_path)
    report("after", snapshot(conn))


if __name__ == "__main__":
    main()
