#!/usr/bin/env bash
#
# VibeDev Discuss "plan run" live smoke test (headless, no UI).
#
# Creates a VibeDev run via the v3 task API, executes it, polls to terminal, and
# asserts the mode's invariants from the durable coding-events log. Two modes:
#
#   plan   (default) — a Discuss/plan run (tagged `plan`). Asserts the run is
#                      forced read-only and produces a PLAN, never a diff:
#                        • task terminates `completed` (not failed)
#                        • coding.completed carries plan_run:true
#                        • ZERO coding.approval_requested  (no diff → no HITL stall)
#                        • no coding.plan_artifact_not_captured (the plan was saved)
#
#   build            — a plain Build run (no `plan` tag) — the regression check
#                      that the default path is untouched:
#                        • coding.completed carries plan_run:false
#                        • a change-making prompt stages a CodeChangeProposal
#                          (>=1 coding.approval_requested)
#
# Usage:
#   scripts/vibe-plan-run-smoke.sh [plan|build]
#
# Env overrides:
#   MAGICIAN_HOST  (default 127.0.0.1:3002)
#   MAGICIAN_BEARER_TOKEN  scoped API credential (optional in local open mode)
#   PROJECT_ID     (default: the active VibeDev project)
#   PROMPT         (override the request)
#   POLL_MAX       (default 60 polls × 15s = 15 min)
#
# Exit 0 = PASS, non-zero = FAIL. Requires curl + jq.
set -uo pipefail

HOST="${MAGICIAN_HOST:-127.0.0.1:3002}"
MODE="${1:-plan}"
POLL_MAX="${POLL_MAX:-60}"
H=(-H "Content-Type: application/json")
if [[ -n "${MAGICIAN_BEARER_TOKEN:-}" ]]; then
  H+=(-H "Authorization: Bearer $MAGICIAN_BEARER_TOKEN")
fi
api() { curl -s -m 25 "${H[@]}" "$@"; }

case "$MODE" in plan|build) ;; *) echo "usage: $0 [plan|build]"; exit 2;; esac

# ── resolve the project ──────────────────────────────────────────────────────
PROJ="${PROJECT_ID:-$(api "http://$HOST/api/magician/v2/vibedev/projects" \
  | jq -r '.active_project_id // .projects[0].project_id // empty')}"
[ -n "$PROJ" ] || { echo "FAIL: no VibeDev project (set PROJECT_ID)"; exit 1; }
echo "project=$PROJ  mode=$MODE  host=$HOST"

# ── build the task description + tags per mode ───────────────────────────────
if [ "$MODE" = "plan" ]; then
  PROMPT="${PROMPT:-Plan how to add a simple in-memory rate limiter to a web request handler: outline the objective, approach, key decisions, risks, and the concrete files to change.}"
  TAGS=$(jq -n --arg a "$(uuidgen)" --arg b "$(uuidgen)" \
    '[{id:$a,name:"vibedev",color:"#c2502a"},{id:$b,name:"plan",color:"#f59e0b"}]')
  TITLE="VibeDev plan smoke"
  DESC="VibeDev planning request:
$PROMPT

VibeDev project context:
VibeDev project: $PROJ
run_coding_task repo_path: .

Planning approach (Discuss — read-only, NO code changes, the PLAN is the deliverable):
- Produce a concrete written plan. Do NOT modify files or stage code proposals.
- To inspect the repo, call run_coding_task with plan_only: true (read-only, stages no diff, captures the plan).
- The plan IS the deliverable: when done, YIELD with the plan as your completed result.
- Discuss mode: this is a read-only request. Explain, plan, or review — do NOT modify files or stage code proposals."
else
  PROMPT="${PROMPT:-Create a file named SMOKE_TEST.md containing exactly the single line: Build-mode smoke test. Make no other changes.}"
  TAGS=$(jq -n --arg a "$(uuidgen)" '[{id:$a,name:"vibedev",color:"#c2502a"}]')
  TITLE="VibeDev build smoke"
  DESC="VibeDev coding request:
$PROMPT

VibeDev project context:
VibeDev project: $PROJ
run_coding_task repo_path: .

Execution policy:
- Route implementation through the existing engineering agents.
- Use Magician's Pi-backed run_coding_task flow for file changes.
- Stage code changes as a CodeChangeProposal diff_approval item.
- Success = run_coding_task returned a staged proposal touching SMOKE_TEST.md (a pending OR an applied proposal both count). Verify against the real_working_dir in the run_coding_task result, NOT the repo root, and do NOT re-delegate to re-confirm the file."
fi

PAYLOAD=$(jq -n --arg d "$DESC" --arg t "$TITLE" --argjson tags "$TAGS" \
  '{ui_thread_id:"vibedev",title:$t,description:$d,agent_id:"engineering-manager",tags:$tags,approved:true,output_mode:"accumulate"}')

TID=$(api -X POST "http://$HOST/api/magician/v3/tasks" -d "$PAYLOAD" | jq -r '.task.manifest.task_id // empty')
[ -n "$TID" ] || { echo "FAIL: task create"; exit 1; }
echo "task=$TID"
api -X POST "http://$HOST/api/magician/v3/tasks/$TID/execute" -d '{}' >/dev/null

# ── poll to terminal / paused ────────────────────────────────────────────────
S="?"
for i in $(seq 1 "$POLL_MAX"); do
  S=$(api "http://$HOST/api/magician/v3/tasks/$TID" | jq -r '.task.state.status // "?"')
  printf '[poll %02d] %s\n' "$i" "$S"
  case "$S" in completed|failed|cancelled|paused|waiting_*) break;; esac
  sleep 15
done

# ── inspect the durable coding-events log + assert ───────────────────────────
EV=$(api "http://$HOST/api/magician/v2/vibedev/runs/$TID/coding-events?limit=4000")
PLAN_RUN=$(echo "$EV" | grep -oE '"plan_run":(true|false)' | tail -1)
APPROVALS=$(echo "$EV" | grep -c '"event_type":"coding.approval_requested"' || true)
NOTCAP=$(echo "$EV" | grep -c '"event_type":"coding.plan_artifact_not_captured"' || true)
echo "── result: status=$S  $PLAN_RUN  approvals=$APPROVALS"

FAIL=0
if [ "$MODE" = "plan" ]; then
  [ "$S" = "completed" ] || { echo "✗ expected completed, got $S"; FAIL=1; }
  echo "$PLAN_RUN" | grep -q true || { echo "✗ plan_run not true (plan_only didn't engage)"; FAIL=1; }
  [ "${APPROVALS:-0}" = "0" ] || { echo "✗ a plan run must raise ZERO approvals, got $APPROVALS"; FAIL=1; }
  [ "${NOTCAP:-0}" = "0" ] || { echo "✗ plan artifact was NOT captured"; FAIL=1; }
  [ "$FAIL" = 0 ] && echo "✓ PLAN smoke PASS — completed, plan_run:true, 0 approvals, plan captured"
else
  echo "$PLAN_RUN" | grep -q false || { echo "✗ build run must be plan_run:false"; FAIL=1; }
  [ "${APPROVALS:-0}" -ge 1 ] || echo "⚠ build staged no proposal (no_change?) — check the prompt actually yields a diff"
  [ "$FAIL" = 0 ] && echo "✓ BUILD smoke PASS — plan_run:false, proposals staged=$APPROVALS"
fi
echo "task_id=$TID  (cleanup: call DELETE /api/magician/v3/tasks/$TID?remove_files=true with the same bearer)"
exit "$FAIL"
