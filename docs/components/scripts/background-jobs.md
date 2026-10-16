# Background Jobs — `bg_run.sh` / `bg_wait.sh`

Run a long command detached and always learn how it ended. Scripts:
`scripts/bg_run.sh` (producer), `scripts/bg_wait.sh` (waiter),
`scripts/test_bg_run.sh` (tests, via `make check-bg-run`).

Long builds run in the background and get waited on, which makes reporting
the outcome a correctness concern.

## The failure mode this prevents

```sh
cmd > job.log 2>&1; echo "EXIT:$?" > job.status   # DO NOT
```

paired with a waiter blocked on `until [ -f job.status ]`. The status write is
a **separate statement**, so anything that kills the shell in between
(SIGTERM, SIGHUP, a broken pipe) skips it, and the waiter spins until the
session ends with no error on either side.

## The contract

`bg_run.sh <job-prefix> <command> [args...]` writes four files:

| File | Purpose |
|---|---|
| `<prefix>.log` | combined stdout + stderr |
| `<prefix>.pid` | the wrapper's pid, for liveness checks |
| `<prefix>.startedat` | wrapper start time, to defeat pid reuse |
| `<prefix>.status` | `EXIT:<code>` or `KILLED:<signal>` |

- **The status write is a trap, not a statement**, so every catchable exit
  records exactly one outcome.
- **It is atomic** (temp file, then rename), so a polling waiter never reads a
  truncated code.
- **It is write-once**, so the EXIT trap cannot overwrite a signal handler's
  cause of death.
- **Stale artifacts are cleared upfront**, so a rerun's waiter never reads the
  previous run's exit code.

On a signal the wrapper kills its child **tree** before recording; killing
only `cargo` leaves its `rustc` children running. The tree is enumerated
*before* anything is signalled, because killing the root first reparents its
children to init, where `pgrep -P` cannot find them. (The target-dir lock is
released when `cargo` exits; the tree walk is about wasted compute.)

## Why the waiter is half the fix

**A trap cannot cover SIGKILL, an OOM kill, or a power cut**, so no producer
can guarantee a status file.
`bg_wait.sh <job-prefix> [timeout-seconds] [poll-seconds]` never waits on the
status file alone:

| Outcome | Exit | Meaning |
|---|---|---|
| `EXIT:<code>` / `KILLED:<sig>` | 0 | the job reported |
| `DIED:uncatchable-no-status` | 3 | producer gone, no status — SIGKILL or worse |
| `DIED:pid-reused-no-status` | 3 | the pid now belongs to something else |
| `TIMEOUT:<n>s` | 4 | deadline passed |

A bare `kill -0` only proves something holds that pid, so `bg_run.sh` records a
start time too; comparing them separates "still running" from a reused pid.

Pass a timeout for anything that must not outlive a session. Omitting it (or
`0`) removes only the deadline; a status file or a dead producer still ends the
wait. The poll interval defaults to 10 s.

## Testing

`make check-bg-run` (in `check-all`). The key cases are SIGTERM, where the
trap must still record an outcome, and SIGKILL, where the waiter must work it
out alone. The suite is mutation-tested: removing the TERM trap makes the EXIT
backstop report `EXIT:0` for a killed job (a false success), and removing the
upfront cleanup makes a rerun report the prior code; both are caught. Test
children use distinctive sleep markers (`sleep 4260`, `4242`) so an unrelated
`sleep` cannot false-fail the suite.
