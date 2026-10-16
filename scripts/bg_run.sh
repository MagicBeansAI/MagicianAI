#!/bin/sh
# Run a long command in the background and ALWAYS leave a status file behind.
#
# Why this exists: the obvious shape for backgrounding a long build is
#
#     cmd > job.log 2>&1; echo "EXIT:$?" > job.status
#
# and it strands a reader forever. The status write is a *separate statement*,
# so anything that kills the shell between the two — SIGTERM, SIGHUP when a
# terminal goes away, a broken pipe — skips it. A waiter blocked on
# `until [ -f job.status ]` then spins until the session ends. That happened
# here on 2026-08-13: two waiters ran ~1.5h against builds that had died
# ~40 minutes earlier, and nothing in either log said so.
#
# The fix is that the status write is a trap, not a statement, so every
# catchable exit path writes exactly one status. SIGKILL and power loss remain
# uncatchable by construction — no producer can cover those — which is why the
# reader (bg_wait.sh) independently detects a dead producer rather than
# trusting this file to appear. Use the pair; this half alone is not enough.
#
# Usage:  scripts/bg_run.sh <job-prefix> <command> [args...]
#         scripts/bg_run.sh /tmp/scratch/check cargo check -p magician
#
# Writes: <job-prefix>.log      combined stdout+stderr
#         <job-prefix>.pid      this wrapper's pid, for liveness checks
#         <job-prefix>.startedat  wrapper start time, to defeat pid reuse
#         <job-prefix>.status   EXIT:<code> | KILLED:<signal>
#
# Run it detached yourself, e.g.  nohup scripts/bg_run.sh ... >/dev/null 2>&1 &

set -u

if [ $# -lt 2 ]; then
    echo "usage: $0 <job-prefix> <command> [args...]" >&2
    exit 64
fi

PREFIX=$1
shift

LOG="$PREFIX.log"
STATUS="$PREFIX.status"
PIDFILE="$PREFIX.pid"
STAMP="$PREFIX.startedat"

# Clear any previous run's artifacts first. Without this the write-once guard
# below would see a stale status and refuse to record the new run's result —
# the reader would then take an hours-old exit code for a fresh one, which is
# worse than no answer at all.
rm -f "$STATUS" "$PIDFILE" "$STAMP"

# Write-once and atomic. Write-once so a signal handler's cause of death is not
# overwritten by the EXIT trap that follows it; atomic so a reader polling for
# this file can never catch it half-written and parse a truncated exit code.
write_status() {
    if [ -f "$STATUS" ]; then
        return 0
    fi
    _tmp="$STATUS.$$.tmp"
    printf '%s\n' "$1" > "$_tmp" 2>/dev/null || return 0
    mv -f "$_tmp" "$STATUS" 2>/dev/null || true
}

CHILD=""

# Collect a process and everything beneath it, deepest last.
#
# Enumerated BEFORE anything is signalled: killing the root first reparents its
# children to init, and `pgrep -P` can no longer find them. That ordering bug
# is why the first version of this script left orphans behind.
collect_tree() {
    _root=$1
    printf '%s\n' "$_root"
    for _kid in $(pgrep -P "$_root" 2>/dev/null); do
        collect_tree "$_kid"
    done
}

# On a signal, kill the whole child tree before recording.
#
# Killing only the direct child is not enough, and the difference is
# observable: `cargo` dies immediately but its `rustc` children do not, and
# they keep burning CPU against the same target dir. Measured on 2026-08-13 —
# two orphaned rustc at 28% and 10% survived their parent and had to be killed
# by hand. The target-dir lock is released either way (cargo holds it, not
# rustc), so this is about not leaving work running, not about the lock.
on_signal() {
    if [ -n "$CHILD" ]; then
        _tree=$(collect_tree "$CHILD")
        # Root first so it cannot spawn more, then the descendants we already
        # enumerated.
        for _pid in $_tree; do
            kill -TERM "$_pid" 2>/dev/null || true
        done
    fi
    write_status "KILLED:$1"
    exit 143
}

trap 'on_signal HUP'  HUP
trap 'on_signal INT'  INT
trap 'on_signal TERM' TERM
# Backstop for every path no explicit handler covers, including `set -u`
# aborts and the shell exiting for reasons not enumerated above.
trap 'write_status "EXIT:$?"' EXIT

: > "$LOG"
printf '%s\n' "$$" > "$PIDFILE"
# Start time identifies *this* process, so a reader can tell a live wrapper
# from an unrelated process that inherited the same pid after it died.
ps -o lstart= -p $$ 2>/dev/null | sed 's/^[[:space:]]*//' > "$STAMP" || true

"$@" >> "$LOG" 2>&1 &
CHILD=$!
wait "$CHILD"
rc=$?
CHILD=""

write_status "EXIT:$rc"
exit "$rc"
