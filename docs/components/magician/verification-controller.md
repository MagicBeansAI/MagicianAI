# Verification Controller

Seed and live config set `verification.mode: observe`. An absent
`verification:` block defaults to **disabled**. See
[Before enabling](#before-enabling) for what enforce still needs.

Magician owns the invariant that code must be checked. The coding agent still
decides what to change and how to repair it.

## What it changes

```
engine produces and applies code
        ↓
task stays running, visibly VERIFYING
        ↓
Magician runs the project's required checks
   ├─ pass  → release the stored candidate success → completed
   ├─ fail  → diagnostics back to the SAME engineer
   │           → the engineer decides the repair → verify again, within budgets
   └─ none  → finish explicitly as UNVERIFIED
```

The harness owns persistence, recovery, evidence and budgets. Nothing else.

## Where it hooks in

`ArtifactV2Service::persist_execution_outcome` writes the terminal events,
flips the execution terminal and starts synthesis **as one step**, so the gate
sits immediately before that transaction (verification after it would run on a
task the UI, synthesis and voice already believe succeeded):

```
persist_execution_outcome
  ├─ delegated child?          → handle_child_terminal   (never gated)
  ├─ replay short-circuit A/B  → return                  (never re-gated)
  ├─ ►► gate ◄◄
  └─ Atomic Step 1: append_outcome_events + flip terminal + synthesis
```

Gate condition: `is_root && produced_code` — not VibeDev-specific. Any root
execution that staged a proposal, emitted a `coding.*` event or delegated to a
child is a candidate. Delegated children are excluded: gating them would starve
parent reconciliation and let repair children recurse into verification.

Only a terminal **candidate success** consults the gate. A failed or cancelled
run reaches its terminal transaction untouched; holding it would leave the task
reporting neither success nor failure.

## What drives it, and what releases it

A hold is only safe if something always ends it:

- **At the hold.** Opening a gate spawns (not awaits) a driver
  (`spawn_verification_driver`) — the caller is inside the terminal
  transaction, and checks can take many minutes.
- **On a timer.** `spawn_verification_reconciler` re-offers every gate that
  still owes work — see [The driver](#the-driver).
- **On restart.** `recover_pending_verification` rebuilds each scope's
  projections from the journal, then re-spawns a driver for every gate named by
  **either** a live outbox entry (never settled) **or** an unreleased parked
  outcome (`held/<gate>.json`, or a claim in `held/released/`; settled but not
  handed back). Both are needed: every settle retires the outbox entry in the
  same transaction and release runs after the settle, so an outbox-only
  recovery misses the longer post-settle window (release awaits
  `persist_execution_outcome`, which spawns synthesis at LLM latency).

A process-local in-flight set refuses a second driver for a gate already being
worked. It is an optimisation, not a correctness boundary (a drop guard clears
the entry so a panicking driver cannot bar its gate). Correctness comes from
`take_held_outcome` (exactly-once release) and the generation check in
`commit_gate` (refuses a superseded writer).

**Scope equality compares directory segments, not raw strings.** `list_scopes`
returns sanitised names (`a_b` for `a/b`); comparing raw strings would silently
skip every gate in such a scope.

### Release

**Release is re-entry, not reconstruction.** The interrupted outcome is parked
with the gate (`write_held_outcome`) and replayed verbatim into
`persist_execution_outcome`; not-success settlements override only status and
summary.

**An outcome that cannot be serialised is not held.** Parking happens before
the gate opens; a serialisation failure completes the task unverified. A hold
whose parked outcome is missing stops permanently at "the task cannot be
released from here" (reserved for this case). A release that finds nothing
because another release claimed or completed it logs at `debug!`.

**Releasing claims the parked outcome.** `take_held_outcome` renames the record
away; only one caller wins. The `completed_at` replay short-circuits cannot do
this (a held root has no `completed_at`), and without the claim a second
release would append terminal events and spawn synthesis twice.

The claim is a lease on the release; the location *is* the state:

| location | meaning | recovery |
| --- | --- | --- |
| `held/<gate>.json` | parked, unclaimed | re-drive |
| `held/released/<gate>.json` | claimed; a release is in flight | hand back, re-drive |
| `held/completed/<gate>.json` | released; kept for forensics | ignore |

Every exit from `release_verification_hold` moves the record:
`restore_held_outcome` on failure, `complete_held_release` on success, or —
on process death — nowhere, leaving it for `reclaim_orphaned_held_outcomes` at
startup (a claim held only by an in-process task is orphaned by construction).

**Crash window** between `persist_execution_outcome` returning `Ok` and
`complete_held_release` is closed by idempotence: restart re-drives and
re-enters without re-appending events or re-spawning synthesis.

**Re-entry is idempotent in three places.** Short-circuits A and B key on
`completed_at` plus a finished or in-flight synthesis. C keys on the terminal
events already being in `events.jsonl`: Step 1's event append and reducer flip
are not atomic, so C skips the append and falls through to the flip and
synthesis spawn.

`verification_hold_decision` answers from the durable gate before deriving
anything from proposals or coding events, so re-entry does not re-derive `Hold`:

| gate status on re-entry | decision |
| --- | --- |
| `verified`, `unverified` | proceed — run the terminal transaction |
| `exhausted`, `cancelled` | proceed — the driver already set the status |
| `unavailable` | **keep holding**; "could not run the checks" is not a verdict, and releasing would report success on unchecked code |
| `blocked_partial` | refuse; never terminal success |
| `verification_pending`, `repairing` | already held; re-spawn the driver and wait |

### Leases

**A lease belongs to a pass.** TTL 120 s. `run_pass` claims under a fresh
worker id and immediately spawns `spawn_lease_renewal` (extends at half the
TTL), aborting it when the pass returns. Every settle path clears the lease, as
do the two non-transition exits (an `indeterminate` attempt, a pass returning
an error) — otherwise a repair successor re-armed seconds later would be locked
out for the rest of the TTL.

Renewal stops at the first refusal. The **generation fence**, not the lease,
decides what a result is worth: `claim_lease` bumps the generation and
`commit_gate` re-checks it under the file lock. Renewal only keeps a healthy
long pass from looking abandoned. Renewals are not journaled; a crashed renewer
costs only the lease lapsing.

### What a gate is bound to

`project_binding` holds the **repository path** the candidate was applied to,
from, in order:

1. the newest `CodeChangeProposal`'s `apply_root` (authoritative, independent
   of the coding-event read window);
2. the `repo_path` on `coding.*` events, for a self-applied Autopilot run with
   no proposal.

Both are normalised through `resolve_coding_repo_binding` (workspace-relative
under the scope, absolute otherwise).

**A candidate whose repository cannot be named is not gated** — no driver
could settle that gate, so the task would never finish.

## Two records, on purpose

| record | mutability | role |
| --- | --- | --- |
| `VerificationGate` | mutable | one per gated root; the spine linking a root execution to its succession of attestations |
| `VerificationAttestation` | immutable key, append-only attempts, write-once result | evidence for **one** candidate snapshot under **one** policy |

One record cannot hold both an immutable key and a repair counter: after a
repair the snapshot changes.

- **Evidence never touches the proposal.** `CodeChangeProposal::content_hash`
  covers `test_evidence` and the diff-approval decide site recomputes it, so
  appending evidence would break the trusted-store integrity check. The
  attestation *references* the proposal.
- **Scope and provenance are part of the attestation key.** Digests are content
  addresses; without scope, project binding and candidate provenance in the
  key, one project's green could satisfy another's gate.
- Once accepted, an attestation is **sealed**. A manual re-verification creates
  a sibling attestation over the same key, so a green cannot accumulate red
  attempts beneath it.

## Policy

Explicit configuration first; `detect_check_commands` is a fallback. Inference
finds `package.json` scripts, `cargo check/test/clippy` and pytest — not this
repo's `make check-all`, nor required env such as `CARGO_TARGET_DIR`.
Per-command timeouts come from policy.

### Anti-weakening

The policy must be protected from the agent it gates:

- owner/project configuration is **authoritative**;
- repository inference **may add** required checks and **may never remove or
  rewrite** a baseline one;
- a policy change takes effect on a **later** task, never the task that made it;
- a repository cannot grant itself network access or secrets — unconditionally:
  the sandbox comes from the baseline or `SandboxPolicy::default()`, and a
  repository's sandbox block is never read.

Refused weakenings are surfaced, not silently corrected.

`verification.baseline` deserialises into an owner policy (`source` forced to
`Owner`, never read from YAML) that the driver passes to every pass. With a
baseline, a repo with no policy file runs the owner's checks, and a repo policy
that drops or rewrites a baseline check is refused. Without one, the repo
defines its own verification, and a repo with no policy file completes
`unverified`. An *empty* baseline block is treated as absent.

> **"Later task" holds for the baseline, not the repository's own policy.** The
> baseline is read from Magician config at boot, outside every repo. The repo
> policy is read from the *post-candidate* tree, so an addition a task makes to
> `.magician/verification.json` is that task's policy. Additions are additions
> and removals are refused.

### Advisory versus authoritative

`run_project_checks` during development is **advisory**. Only the shared runner
executing the *entire resolved required policy* against the *exact gated
snapshot* produces an acceptable attestation — otherwise a targeted
`cargo test -p one-crate` could pass as full verification.

## Snapshot and workspace

```
content-addressed IMMUTABLE source snapshot   ← what the attestation keys on
        ↓ materialise
ephemeral WRITABLE verification workspace
        ↓
external cache mounts where policy permits
```

A check that mutates tracked source **invalidates the run**; declared build
outputs do not. Build-output dirs (`target/`, `.build/`, `node_modules/`,
`.git/`, rest of `DEFAULT_EXCLUDED_DIRS`) are excluded from the input digest,
otherwise a warm `target/` would make every key unique.

### A green key is answered before anything is copied

Capture and materialise are separate. `find_reusable` runs between them: an
exact whole-key match on a green attestation settles the gate `verified` from
stored evidence, running nothing. Safe because: whole-key equality (never
digest-only); only greens are indexed; `runner_env_digest` is in the key, so a
different toolchain or runner version does not reuse.

### Workspaces are non-colliding and are reaped

Each pass materialises into `{gate}-r{revision}-{random}`. Cleanup is
`Drop`-only, so a hard kill can leave a directory; an existing directory is
**replaced rather than refused**. Stale workspaces are reaped before each pass
and once per scope at startup recovery.

Reaping cannot remove a live pass's tree: each workspace has a **sibling**
marker file its owner holds an exclusive `flock` on for the whole pass, taken
before the directory is created; the reaper removes only directories whose
marker it can lock. (Inside the workspace, the marker would be captured as a new
file and every pass would be `indeterminate`.)

**Nothing unlinks a marker except the owner of the directory it describes.** A
marker without a directory is left behind: `WorkspaceLease::acquire` opens then
locks, and `flock` follows the inode, so unlinking in that window lets the next
`acquire` create a fresh inode and delete a live checkout. A test pins this.

## Sandbox

Running a repository's checks is executing untrusted code, so this lives in the
runner, not in model instructions:

- environment cleared and rebuilt from a short allowlist; secrets absent unless
  policy opts in;
- each command in its own process group; a timeout kills the **group**;
- output captured tail-first with a hard byte cap;
- a total timeout bounds the whole policy.

## Repair

Repair is a **delegated child of the held root execution** via
`DelegationDispatcher::spawn_children`, with the diagnostics as context and the
**root task's agent as the delegation source**. The engineer in
`origin.engineer_agent_id` is the *target* (dispatch validates targets against
the source's roster, so engineer-as-source would be a self-delegation). The
dispatcher is wired via `set_delegation_dispatcher` after both exist (it holds an
`Arc` to the service).

Not on this path: `run_coding_task`, `derive_coding_continuation_context`, and
`start_execution_with_launch_options` (a second root execution is refused with
`task_execution_in_progress`). Authority and cost stay with the original
engineer; the run never touches a provider adapter directly.

**A repair invalidates the candidate the moment it starts**: its outbox entry is
retired with no replacement, and a gate mid-repair refuses to verify.
`record_successor_candidate` advances the candidate, returns the gate to
`verification_pending` and enqueues fresh work in one journaled transaction.
Because a delegated child returns above the terminal seam, the successor is
picked up on the **child-terminal** path (`resume_verification_after_repair`);
a root-level re-run is handled at the seam. Both re-arm the gate.

A repair that never produces a successor (engineer failed, dispatch failed) is
bounded by the elapsed budget: nothing in the outbox names a `repairing` gate
(and `register_repaired_candidate` returns `None` without a proposal), but the
reconciler offers a pass to every unreleased parked outcome; that pass takes the
`repairing` early return, finds the budget spent and settles `exhausted`.

### No-progress is three conditions

```
same NORMALIZED failure
AND no relevant change to the checked snapshot
AND no new diagnostic information
```

Normalisation strips timestamps, durations, hex addresses, pids and temp paths,
and collapses digit runs (losing line numbers — a repair that only moved a line
has not progressed).

## Outcomes

`verification_state` is a durable projection alongside task status, not a new
terminal status (which would ripple through reducers, schedulers, chat, cards,
activity and voice).

| controller outcome | task status | `verification_state` |
| --- | --- | --- |
| green | `completed` | `verified` |
| red, budget remains | **stays running** | `repairing` |
| no checks configured | `completed` | `unverified` |
| repair exhausted | `failed` | `exhausted` + reason |
| runner/policy unavailable | retryable blocked | `unavailable` |
| cancelled (`cancel()` has no production caller) | `cancelled` | `cancelled` |
| partially applied proposal | never terminal success | `blocked_partial` |

Rules:

- **Missing state reads as `unknown`, never `verified`** (legacy VibeDev runs are
  `unknown`). Automation requiring working code must check
  `status == completed` **and** `verification_state == verified`.
- **`unavailable` is not `unverified`** ("could not run" vs "nothing to run").
- **Voice never says plain "completed successfully" for `unverified`.** It
  appends the prompt-store clause `voice_verification_unverified_v1.0.0`:
  *"Nothing checked that code, so I can't tell you whether it works."*

Events (`verification.queued`, `.started`, `.command_completed`,
`.checks_failed`, `.repair_started`, `.passed`, `.unverified`, `.exhausted`,
`.unavailable`, `.attempt_indeterminate`, `.cancelled`) are **replayable
projections of committed state, never the source of truth**; task hydration
yields correct state without them. Voice consumes only the five high-level
transitions. `checks_failed`, not `failed`, because a failed check normally
leads to repair, not task failure.

They reach the canonical runtime stream via `CanonicalVerificationEventSink` →
`RuntimeCanonicalEventSink` under one event type, `verification.lifecycle`; the
payload is the whole serialised event with `kind` set to the stable wire name.
Principal/workspace are fixed at adapter construction; task and execution ride
on each event; `ui_thread_id` is empty. A serialisation failure drops the
projection, never the pass. `verification.queued` has no emitter and
`.cancelled` is reachable only through `cancel()`.

`verification_state` reaches the task API and voice fanout
(`task_completed_message`), not task cards or webhooks; those consumers read
`verification_state_for_task`.

## Budgets

Three dimensions, checked independently; first to blow stops the gate.
Defaults: `max_repair_rounds: 3`, `max_elapsed_secs: 7200`, `max_spend_usd:
None`. The `verification:` block resolves into `VerificationRuntimeSettings` at
boot, and `open_verification_gate` stamps `budgets.resolve()` onto every gate.
`BudgetConfig` resolves partial or malformed input to the defaults.

- **`may_start_pass` ignores repair rounds** — a gate that used its last round
  still owes that round's verification. Elapsed and spend *are* checked.
- **`accept_result` refuses an indeterminate attempt**; the gate re-runs under
  a new attempt.
- **Spend.** `charge_verification_repair_round` runs on the repair child's
  terminal and charges *before* the successor is registered, so a gate at its
  ceiling settles at the next pass. The figure is the engine-reported session
  cost on the newest `coding.stats` event; best-effort (no stats → no charge),
  which is acceptable because `max_repair_rounds` cannot be dodged. Charging is
  saturating and refuses negative costs.

## The driver

`spawn_verification_reconciler`, started once at boot beside startup recovery,
re-offers work between restarts.

- In-process interval task (modelled on the list-index reconciler). First tick
  one interval out; `MissedTickBehavior::Delay`; a failed tick logs and
  continues; each scope's directory walk runs on the blocking pool. Cadence
  `verification.reconcile_interval_secs` (default 60; `0` disables, leaving
  startup recovery as the only scheduler). Logs only when it dispatched
  something.
- **Sweep set per scope** = every outbox entry ∪ every parked unreleased
  outcome − gates already in flight in this process. The second half is
  load-bearing: a `repairing` gate is named only by its parked outcome.
- Differences from startup recovery: parked outcomes are **listed, never
  reclaimed** (a claim is held by a live, maybe slow, in-process release); **no
  journal replay** (a boot cost, not a per-minute one).
- It does not claim work exclusively across processes; the lease and generation
  fence decide who lands a result.

| behaviour | how it is delivered |
| --- | --- |
| `RetryLater` / `Unavailable` are retryable | the gate keeps its outbox entry and the next tick offers another pass |
| the elapsed budget bounds a gate | every offered pass evaluates it at the top, even for a gate running nothing |
| a stalled repair settles as `exhausted` | via the parked-outcome half; the `repairing` early return applies the ceiling |
| a settled-but-unreleased gate completes its task | re-driven from that same half; replay guards make re-entry idempotent |
| a lease is renewed rather than relying on a generous TTL | `spawn_lease_renewal`, alongside the pass that holds it |

## Durability

- The gate and its outbox entry are **one** journal transaction committed with a
  single atomic rename, then projected — either without the other is
  unrecoverable.
- A gate that cannot be opened is **not** a hold: completion proceeds with
  `verification_state = unknown`.
- Finalisation is compare-and-set on gate status, candidate identity and a green
  attestation. Leases are fenced by a monotonic `generation`, so a late expired
  worker cannot commit.
- Everything fails closed: an unavailable evidence store, unreadable policy,
  crashed attempt or mutated snapshot never lands green.

### The store is synchronous, so its callers hop off the runtime

Every `VerificationStore` method blocks (advisory `flock` + journal replay).
Controller calls go through `on_store` (`spawn_blocking`); startup recovery does
`recover`, reap and `list_outbox` in one blocking hop.
`register_repaired_candidate` and `existing_gate_decision` are `async` for this
reason only. Deliberately inline (marked in code):

- `verification_state_for_task` — task-read hot path; one or two indexed reads,
  no lock or replay.
- `existing_gate_decision` — same two reads; only its
  `register_repaired_candidate` branch hops.
- `cancel` — no production caller; must move to `on_store` once it gets one.

## Enabling

```yaml
verification:
  mode: disabled            # disabled | observe | enforce
  max_repair_rounds: 3
  max_elapsed_secs: 7200
  # max_spend_usd: 5.0      # unset = no spend ceiling
  reconcile_interval_secs: 60
  # baseline:               # owner policy; absent = each repo defines its own
  #   required:
  #     - id: check
  #       program: make
  #       args: ["check-all"]
```

- `MAGICIAN_VERIFICATION_CONTROLLER`, **when set**, overrides `mode` (the
  process-local kill switch). Tests pin a mode per service instance
  (`set_verification_activation`) instead.
- Every field defaults identically whether its key or the whole block is
  absent, so env-only enabling still gets a driver (a derived `Default` would
  make `reconcile_interval_secs` `0`).
- An unrecognised *value* degrades to **disabled**; an unrecognised *key* is a
  hard parse error.
- Disabled: the terminal path does no additional I/O (activation is checked
  before any filesystem read).
- `MagicianConfig` denies unknown fields, so a binary that does not know
  `verification:` refuses to start — sequence config resync with binary deploy.

### Observe is the qualification mode

`observe` opens the gate, materialises the snapshot, runs the resolved policy
and seals the attestation exactly as `enforce` does, but:

- **nothing is held** — the task completes on its own terminal path;
- **nothing is parked** — a parked outcome with no holder would be re-offered
  forever;
- **nothing is released** — releasing would replay a terminal transaction;
- **a red result settles `exhausted`** ("checks failed under observe; repair is
  not dispatched"), recording the red attestation as evidence.
  `settle_exhausted` records the attestation the gate names; enforce's
  last-round settlement does the same, and settlements with no attempt keep the
  previous reference.

A green observe pass seals the same attestation enforce would.

## Before enabling

Enforce still requires:

- **An owner baseline.** `verification.baseline` ships commented out and an
  empty block reads as absent, so anti-weakening has no teeth until the checks
  are written down. See [Anti-weakening](#anti-weakening).
- `verification_state` on task cards and webhooks (task API and voice already
  carry it).
- Both coding engines activating together; neither may claim verification-gated
  completion alone. Under enforce, auto repair may switch engines after a failed
  candidate; a named pin stays same-engine.

Operational runbook: §8 of the
closure plan.

## Source

`magician/src/magician_v2/execution/verification/` — `gate`, `attestation`,
`journal`, `store`, `policy`, `snapshot`, `runner`, `gating`, `repair`,
`controller`, `events`, `budgets`, `ids`.

The seam, driver and reconciler live in `magician_v2/artifact_v2/service.rs`;
the canonical event adapter in `magician_v2/artifact_v2/events.rs`; the
`verification:` block in `config.rs` (`VerificationConfig`), wired at boot from
`magician-bin/src/main.rs`.

Tests: seam tests in `artifact_v2/service.rs` (hold → verify → release, hold →
red → still held, failed run) against real subprocess checks; fixtures use
`.magician/verification.json` and red-path assertions require `repairing`. The
module suite covers observe/enforce sealing, budgets, lease renewal and config
parsing.

Design: `docs/archive/plans/2026-08-07-vibedev-verification-controller.md`
