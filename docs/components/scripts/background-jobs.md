# Background Jobs — `bg_run.sh` / `bg_wait.sh`

Run a long command detached and always learn how it ended. Scripts:
`scripts/bg_run.sh` (producer), `scripts/bg_wait.sh` (waiter),
`scripts/test_bg_run.sh` (tests, via `make check-bg-run`).

Builds in this workspace run 5–16 minutes, so they get backgrounded and waited
on rather than held in the foreground. That makes the *reporting* of the
outcome a correctness concern in its own right.

## The failure mode this prevents

The naive shape is:

```sh
cmd > job.log 2>&1; echo "EXIT:$?" > job.status   # DO NOT
```

paired with a waiter blocked on `until [ -f job.status ]`.

The status write is a **separate statement**. Anything that kills the shell
between the command and the `echo` — SIGTERM, SIGHUP when a terminal goes
away, a broken pipe — skips it, and the file the waiter is waiting for can
then never exist. The waiter spins until the session ends. The failure is silent on both sides —
the log just stops mid-line — so the only symptom is time passing.

## The contract

`bg_run.sh <job-prefix> <command> [args...]` writes four files:

| File | Purpose |
|---|---|
| `<prefix>.log` | combined stdout + stderr |
| `<prefix>.pid` | the wrapper's pid, for liveness checks |
| `<prefix>.startedat` | wrapper start time, to defeat pid reuse |
| `<prefix>.status` | `EXIT:<code>` or `KILLED:<signal>` |

Four properties matter:

- **The status write is a trap, not a statement**, so every catchable exit
  path records exactly one outcome.
- **It is written atomically** (temp file, then rename). A waiter polling for
  existence must never catch the file half-written and parse a truncated code.
- **It is write-once**, so a signal handler's cause of death is not overwritten
  by the EXIT trap that follows it.
- **Stale artifacts are cleared upfront.** Without this a rerun's waiter reads
  the *previous* run's exit code and takes it for a fresh answer — a wrong
  result, which is worse than no result.

On a signal the wrapper kills its child **tree** before recording. Killing only
the direct child stops `cargo` and leaves its `rustc` children running, competing
with the build that replaces them.

The tree is enumerated *before* anything is signalled. Killing the root first
reparents its children to init, and `pgrep -P` can then no longer find them —
so the naive order looks correct and leaves exactly the orphans it was meant
to prevent.

Two separate problems are worth keeping apart here. The **target-dir lock** is
released as soon as `cargo` exits, because `cargo` holds it and `rustc` does
not; killing the direct child is sufficient for that. The **wasted compute** of orphaned grandchildren is a
different failure and needs the tree walk.

## Why the waiter is half the fix

**A trap cannot cover SIGKILL, an OOM kill, or a power cut.** No producer can
guarantee a status file, so a waiter that assumes one will eventually appear is
wrong by construction.

`bg_wait.sh <job-prefix> [timeout-seconds] [poll-seconds]` therefore never
waits on the status file alone. Three things end the wait:

| Outcome | Exit | Meaning |
|---|---|---|
| `EXIT:<code>` / `KILLED:<sig>` | 0 | the job reported |
| `DIED:uncatchable-no-status` | 3 | producer gone, no status — SIGKILL or worse |
| `DIED:pid-reused-no-status` | 3 | the pid now belongs to something else |
| `TIMEOUT:<n>s` | 4 | deadline passed |

The pid-reuse case is why `bg_run.sh` records a start time as well as a pid: a
bare `kill -0` proves only that *something* holds that number, and after the
producer dies the number is free for reuse. Comparing start times separates
"still running" from "a stranger wearing its pid".

Pass a timeout for anything that must not outlive a session. Omitting it (or
passing `0`) still terminates on rules 1 and 2 — it only removes the deadline.

## Testing

`make check-bg-run` (in `check-all`). The cases that carry the weight are the
kills: SIGTERM, where the producer's trap must still record an outcome, and
SIGKILL, where it cannot and the waiter must work it out alone.

The suite is mutation-tested. Removing the TERM trap does **not** merely lose
the status: the EXIT backstop then reports `EXIT:0` for a killed job — a false
*success*, which is worse than the hang it replaced. Removing the upfront
cleanup makes a rerun report the prior run's code. Both mutations are caught,
which is what shows the tests are pinning the behaviour rather than the shape.
Test children use distinctive sleep markers (`sleep 4260`, `4242`) so an
unrelated `sleep` from a concurrent session cannot false-fail the suite.
