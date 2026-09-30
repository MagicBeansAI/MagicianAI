#!/bin/sh
# Wait for a bg_run.sh job and ALWAYS terminate, even if no status ever lands.
#
# The companion to scripts/bg_run.sh. That script traps every catchable exit so
# a status file is written; this one exists because SIGKILL, an OOM kill, a
# panic and a power cut are not catchable, and a waiter that assumes the status
# file must eventually appear will wait forever when they happen.
#
# So this never waits on the status file alone. Three things end the wait:
#   1. the status file appears           -> report it                 (exit 0)
#   2. the producer is gone without one  -> report DIED               (exit 3)
#   3. the deadline passes               -> report TIMEOUT            (exit 4)
#
# Rule 2 is the one that matters, and it is why bg_run.sh records its start
# time as well as its pid: a bare `kill -0` says only that *something* holds
# that pid, and after the producer dies the number is free for reuse. Comparing
# start times distinguishes "still running" from "a stranger wearing its pid".
#
# Usage:  scripts/bg_wait.sh <job-prefix> [timeout-seconds] [poll-seconds]
#         scripts/bg_wait.sh /tmp/scratch/check 3600 15
#
# Prints one line: EXIT:<code> | KILLED:<sig> | DIED:<reason> | TIMEOUT:<n>s

set -u

if [ $# -lt 1 ]; then
    echo "usage: $0 <job-prefix> [timeout-seconds] [poll-seconds]" >&2
    exit 64
fi

PREFIX=$1
TIMEOUT=${2:-0}     # 0 disables the deadline; rules 1 and 2 still apply
POLL=${3:-10}

STATUS="$PREFIX.status"
PIDFILE="$PREFIX.pid"
STAMP="$PREFIX.startedat"

elapsed=0

while :; do
    # 1. The normal path.
    if [ -f "$STATUS" ]; then
        cat "$STATUS"
        exit 0
    fi

    # 2. Producer liveness. Only meaningful once the pid file exists; before
    #    that the job is merely still starting up.
    if [ -f "$PIDFILE" ]; then
        pid=$(cat "$PIDFILE" 2>/dev/null || echo "")
        if [ -n "$pid" ]; then
            if kill -0 "$pid" 2>/dev/null; then
                want=$(cat "$STAMP" 2>/dev/null || echo "")
                now=$(ps -o lstart= -p "$pid" 2>/dev/null | sed 's/^[[:space:]]*//')
                if [ -n "$want" ] && [ -n "$now" ] && [ "$want" != "$now" ]; then
                    echo "DIED:pid-reused-no-status"
                    exit 3
                fi
            else
                # The pid is gone. Re-check the status file before calling it:
                # the producer's trap may have fired in the instant between our
                # two syscalls, and reporting DIED for a job that exited
                # cleanly would be a false alarm in the opposite direction.
                sleep 2
                if [ -f "$STATUS" ]; then
                    cat "$STATUS"
                    exit 0
                fi
                echo "DIED:uncatchable-no-status"
                exit 3
            fi
        fi
    fi

    # 3. Deadline backstop, covering anything the two rules above miss.
    if [ "$TIMEOUT" -gt 0 ] && [ "$elapsed" -ge "$TIMEOUT" ]; then
        echo "TIMEOUT:${TIMEOUT}s"
        exit 4
    fi

    sleep "$POLL"
    elapsed=$((elapsed + POLL))
done
