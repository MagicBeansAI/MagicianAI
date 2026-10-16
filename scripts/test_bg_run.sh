#!/bin/sh
# Tests for scripts/bg_run.sh + scripts/bg_wait.sh.
#
# Every case here is a way a background job can die. The point of the pair is
# that a reader always learns the outcome, so each case asserts on what the
# waiter reports — not on whether the job succeeded.
#
# Run: sh scripts/test_bg_run.sh

set -u

HERE=$(cd "$(dirname "$0")" && pwd)
RUN="$HERE/bg_run.sh"
WAIT="$HERE/bg_wait.sh"
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

pass=0
fail=0

check() {
    _name=$1; _want=$2; _got=$3
    if [ "$_want" = "$_got" ]; then
        echo "  PASS  $_name"
        pass=$((pass + 1))
    else
        echo "  FAIL  $_name: want '$_want' got '$_got'"
        fail=$((fail + 1))
    fi
}

echo "bg_run/bg_wait:"

# --- a clean exit reports its code -----------------------------------------
sh "$RUN" "$TMP/ok" true >/dev/null 2>&1
check "success reports EXIT:0" "EXIT:0" "$(sh "$WAIT" "$TMP/ok" 30 1)"

# --- a failing command reports its code, not just "failed" -----------------
sh "$RUN" "$TMP/bad" sh -c 'exit 7' >/dev/null 2>&1
check "failure preserves exit code" "EXIT:7" "$(sh "$WAIT" "$TMP/bad" 30 1)"

# --- output is captured ----------------------------------------------------
sh "$RUN" "$TMP/out" sh -c 'echo hello; echo oops >&2' >/dev/null 2>&1
got=$(grep -c . "$TMP/out.log")
check "stdout+stderr both captured" "2" "$got"

# --- THE REGRESSION: the wrapper is killed mid-run -------------------------
# This is the 2026-08-13 failure. The old pattern lost the status entirely
# here, because the `echo` after the command never ran.
sh "$RUN" "$TMP/term" sleep 4260 >/dev/null 2>&1 &
sleep 1
term_pid=$(cat "$TMP/term.pid" 2>/dev/null || echo "")
kill -TERM "$term_pid" 2>/dev/null
sleep 1
check "SIGTERM still writes a status" "KILLED:TERM" "$(sh "$WAIT" "$TMP/term" 30 1)"

# ...and the child dies with it, rather than surviving to hold a build lock.
sleep 1
if [ -n "$term_pid" ] && pgrep -f "sleep 4260" >/dev/null 2>&1; then
    check "child reaped on signal" "reaped" "still-running"
else
    check "child reaped on signal" "reaped" "reaped"
fi

# --- grandchildren die too, not just the direct child ----------------------
# `cargo` spawns `rustc`; killing only the direct child leaves those burning
# CPU against the same target dir. Observed for real on 2026-08-13, which is
# why this case exists. The marker is distinctive so the assertion cannot
# match an unrelated sleep.
sh "$RUN" "$TMP/tree" sh -c 'sleep 4242 & wait' >/dev/null 2>&1 &
tree_launcher=$!
sleep 1
tree_pid=$(cat "$TMP/tree.pid" 2>/dev/null || echo "")
kill -TERM "$tree_pid" 2>/dev/null
wait "$tree_launcher" 2>/dev/null || true
sleep 1
if pgrep -f "sleep 4242" >/dev/null 2>&1; then
    check "grandchild reaped, not just the child" "reaped" "orphaned"
    pkill -f "sleep 4242" >/dev/null 2>&1
else
    check "grandchild reaped, not just the child" "reaped" "reaped"
fi

# --- THE UNCATCHABLE CASE: SIGKILL, which no trap can cover ----------------
# The producer cannot write a status here by construction. The waiter must
# work it out for itself instead of blocking forever.
sh "$RUN" "$TMP/kill" sleep 60 >/dev/null 2>&1 &
kill_launcher=$!
sleep 1
kill_pid=$(cat "$TMP/kill.pid" 2>/dev/null || echo "")
kill -KILL "$kill_pid" 2>/dev/null
# Reap it here, or the shell prints its own "Killed: 9" notice later and a
# check-all run looks like it failed when every case passed.
wait "$kill_launcher" 2>/dev/null || true
check "SIGKILL detected by the waiter" "DIED:uncatchable-no-status" "$(sh "$WAIT" "$TMP/kill" 30 1)"
pkill -f "sleep 60" >/dev/null 2>&1

# --- a job that never ends still releases the waiter -----------------------
sh "$RUN" "$TMP/slow" sleep 60 >/dev/null 2>&1 &
sleep 1
check "deadline releases the waiter" "TIMEOUT:3s" "$(sh "$WAIT" "$TMP/slow" 3 1)"
slow_pid=$(cat "$TMP/slow.pid" 2>/dev/null || echo "")
[ -n "$slow_pid" ] && kill -TERM "$slow_pid" 2>/dev/null
pkill -f "sleep 60" >/dev/null 2>&1

# --- a rerun must not report the previous run's result ---------------------
# The write-once guard is per run; without the upfront rm, a second run would
# hand the reader a stale exit code, which reads as a real answer.
sh "$RUN" "$TMP/reuse" sh -c 'exit 3' >/dev/null 2>&1
sh "$RUN" "$TMP/reuse" true >/dev/null 2>&1
check "rerun overwrites stale status" "EXIT:0" "$(sh "$WAIT" "$TMP/reuse" 30 1)"

# --- a torn status is never observable -------------------------------------
# The reader polls for existence, so the file must appear complete or not at
# all; a two-step write would let it read an empty file as a valid result.
sh "$RUN" "$TMP/atomic" true >/dev/null 2>&1
check "status file is complete when it appears" "EXIT:0" "$(cat "$TMP/atomic.status")"

echo
echo "  $pass passed, $fail failed"
[ "$fail" -eq 0 ] || exit 1
