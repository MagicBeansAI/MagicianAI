# Verification Controller

Magician owns the invariant that code must be checked; the coding agent decides
the change and the repair. Seed and live config set
`verification.mode: observe`; an absent `verification:` block defaults to
**disabled**. See [Before enabling](#before-enabling) for what enforce still
needs.

**Relentless reuse.** Relentless's optional `artifact_review.verification` uses
this `ResolvedPolicy`, snapshot/workspace and runner as a deterministic critic,
pinning commands, executables, the mandatory OS launcher, read-only roots and
protected source hashes before owner admission. Declared sources are snapshotted
exactly, without conventional directory exclusions; declared outputs cannot
overlap them. The owned job keeps its ephemeral workspace through cancellation
and result publication. A dispatch marker stops an interrupted check from
repeating; recovery returns an unknown review without resending or overwriting a
late result. Caller disconnect cancels the job's productive work; receipt
publication and draining stay owned. The runner's optional cancellation token
and mandatory confined-workspace mode drain process groups and pipe tasks on
timeout/cancellation/error; ordinary verification keeps its policy path when
these are absent. Runner semantics (`runner::RUNNER_VERSION`, now 3) are pinned
in the recipe and cache identity, so a semantic change requires new admission.
Source-complete, awaiting batched qualification; the ordinary observe-mode
rollout below is unchanged.

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

The harness owns only persistence, recovery, evidence and budgets.

## Where it hooks in

`ArtifactV2Service::persist_execution_outcome` writes terminal events, flips
the execution terminal and starts synthesis **as one step**; the gate sits just
before it:

```
persist_execution_outcome
  ├─ delegated child?          → handle_child_terminal   (never gated)
  ├─ replay short-circuit A/B  → return                  (never re-gated)
  ├─ ►► gate ◄◄
  └─ Atomic Step 1: append_outcome_events + flip terminal + synthesis
```

Gate condition: `is_root && produced_code`: a root execution (any, not just
VibeDev) that staged a proposal, emitted a `coding.*` event or delegated.
Delegated children are excluded (gating them would starve parent reconciliation
and let repair children recurse). Only a terminal **candidate success** consults
the gate; a failed or cancelled run reaches its terminal transaction untouched.

## What drives it, and what releases it

- **At the hold**: `spawn_verification_driver` (spawned, not awaited: the
  caller is inside the terminal transaction and checks can take minutes).
- **On a timer**: `spawn_verification_reconciler`; see [The driver](#the-driver).
- **On restart**: `recover_pending_verification` rebuilds projections from the
  journal and re-drives every gate named by a live outbox entry **or** an
  unreleased parked outcome (`held/<gate>.json`, or a claim in
  `held/released/`), since a settle retires the outbox entry in the same
  transaction and release runs after it (an outbox-only recovery would miss the
  post-settle window).

A process-local in-flight set refuses a second driver per gate; it is an
optimisation (a drop guard clears it if a driver panics). Correctness comes from
`take_held_outcome` (exactly-once release) and the generation check in
`commit_gate` (refuses a superseded writer). **Scope equality compares directory
segments**: `list_scopes` returns sanitised names (`a_b` for `a/b`), so raw
string comparison would skip every gate in such a scope.

### Release

**Release is re-entry, not reconstruction**: the outcome parked by
`write_held_outcome` replays verbatim into `persist_execution_outcome`
(not-success settlements override only status and summary). Parking precedes
opening the gate, so an unserialisable outcome is not held (the task completes
unverified); a hold without its parked outcome stops permanently at "the task
cannot be released from here" (reserved for this case). A release that finds the
record already claimed or completed logs at `debug!`.

`take_held_outcome` claims by rename, so only one caller wins (the `completed_at` short-circuits cannot guard a held root). The location *is* the state:

| location | meaning | recovery |
| --- | --- | --- |
| `held/<gate>.json` | parked, unclaimed | re-drive |
| `held/released/<gate>.json` | claimed; a release is in flight | hand back, re-drive |
| `held/completed/<gate>.json` | released; kept for forensics | ignore |

`release_verification_hold` exits via `restore_held_outcome` (failure),
`complete_held_release` (success), or process death, handled by
`reclaim_orphaned_held_outcomes` at startup (an in-process claim is orphaned by
construction). A crash between `persist_execution_outcome` returning `Ok` and
`complete_held_release` re-drives safely because re-entry is idempotent:
short-circuits A and B key on `completed_at` plus a finished or in-flight
synthesis; C keys on terminal events already in `events.jsonl` (append and
flip are not atomic), skipping only the append.

`verification_hold_decision` answers from the durable gate before proposals or
coding events, so re-entry never re-derives `Hold`:

| gate status on re-entry | decision |
| --- | --- |
| `verified`, `unverified` | proceed — run the terminal transaction |
| `exhausted`, `cancelled` | proceed — the driver already set the status |
| `unavailable` | **keep holding**; "could not run the checks" is not a verdict, and releasing would report success on unchecked code |
| `blocked_partial` | refuse; never terminal success |
| `verification_pending`, `repairing` | already held; re-spawn the driver and wait |

### Leases

**A lease belongs to a pass**: `run_pass` claims under a fresh worker id, TTL
120 s, renewed at half-TTL by `spawn_lease_renewal` (unjournaled; stops at the
first refusal; aborted when the pass returns). Settles, `indeterminate` attempts
and failed passes clear it. The **generation fence**, not the lease, decides what
a result is worth: `claim_lease` bumps it, `commit_gate` re-checks it under the
file lock.

### What a gate is bound to

`project_binding` holds the **repository path** the candidate was applied to:
the newest `CodeChangeProposal`'s `apply_root` (authoritative), else the
`repo_path` on `coding.*` events (self-applied Autopilot), normalised by
`resolve_coding_repo_binding` (workspace-relative under the scope, else
absolute). **A candidate whose repository cannot be named is not gated**: no
driver could settle it, so the task would never finish.

## Two records, on purpose

| record | mutability | role |
| --- | --- | --- |
| `VerificationGate` | mutable | one per gated root; links a root execution to its succession of attestations |
| `VerificationAttestation` | immutable key, append-only attempts, write-once result | evidence for **one** candidate snapshot under **one** policy |

One record cannot hold an immutable key and a repair counter: a repair changes
the snapshot.

- **Evidence never touches the proposal**: `CodeChangeProposal::content_hash`
  covers `test_evidence` and the diff-approval decide site recomputes it; the
  attestation references the proposal.
- **Scope, project binding and candidate provenance are in the attestation
  key**, so one project's green cannot satisfy another's gate.
- An accepted attestation is **sealed**; manual re-verification creates a
  sibling over the same key, so a green never accumulates red attempts.

## Policy

Each pass resolves the required policy with `verification::policy::resolve`
from two sources only: the owner baseline (`verification.baseline`) and the
repository's `.magician/verification.json` or `.magician/verification.yaml`
(`runner::read_repository_policy`). The gate passes no inferred checks. An
unreadable or unresolvable policy settles `unavailable`; a policy with no
required check settles `unverified`. Defaults per `CheckSpec`: 30 min per
command, 90 min per run, 64 KiB captured per stream (`policy.rs`).

VibeDev's own check list (`vibedev::project_checks::resolve_project_checks`:
project `checks`, `.magician/checks.yaml`, task-runner targets, stack presets;
240 s default, 3600 s max per check, `build_caches` env) feeds the advisory
`run_project_checks` and the VibeDev API, not the gate; see
[coding-engine isolation](coding-engine-isolation.md#trusted-repo-mode-no-shadow).

### Anti-weakening

- the owner baseline is **authoritative**;
- the repository policy **may add** required checks and may change a baseline
  check's env or timeout, but **never removes** one or changes its program or
  args (refused ids land in `ResolvedPolicy::refused_weakenings`); the
  strictest `total_timeout_secs` wins;
- a policy change applies to a **later** task, never the one that made it;
- the sandbox comes from the baseline or `SandboxPolicy::default()`, never the
  repo, so a repo cannot grant itself network or secrets.

`verification.baseline` becomes an owner policy (`source` forced to `Owner`,
never read from YAML) passed to every pass. With it, a repo with no policy file
runs the owner's checks and a repo policy dropping or rewriting a baseline check
is refused. Without one (an *empty* block counts as absent) the repo defines its
own, and no policy file completes `unverified`. "Later task" binds the baseline
only (read from config at boot, outside every repo): a task's additions to
`.magician/verification.{json,yaml}` (read post-candidate) apply to it; removals
are refused.

### Advisory versus authoritative

`run_project_checks` during development is **advisory**. Only the shared runner
executing the *entire resolved required policy* against the *exact gated
snapshot* produces an acceptable attestation.

## Snapshot and workspace

```
content-addressed IMMUTABLE source snapshot   ← what the attestation keys on
        ↓ materialise
ephemeral WRITABLE verification workspace
        ↓
external cache mounts where policy permits
```

A check that mutates tracked source **invalidates the run**; declared build
outputs do not. `target/`, `.build/`, `node_modules/`, `.git/` and the rest of
`DEFAULT_EXCLUDED_DIRS` are excluded from the input digest.

- **Reuse before copy**: `find_reusable` runs between capture and materialise;
  an exact whole-key (never digest-only) match on a green (only greens are
  indexed) settles `verified` from stored evidence, running nothing. The key
  includes `runner_env_digest`, so another toolchain or runner version does not
  reuse.
- **Workspaces** are `{gate}-r{revision}-{random}`, cleaned only on `Drop` (a
  hard kill can leave one; an existing directory is replaced, not refused) and
  reaped before each pass and per scope at startup. The owner holds an exclusive
  `flock` on a **sibling** marker file (inside, it would be captured and make
  every pass `indeterminate`) for the whole pass, taken before the directory is
  created; the reaper removes only directories whose marker it can lock.
  **Only the owner unlinks a marker**: `WorkspaceLease::acquire` opens then
  locks and `flock` follows the inode, so an unlink in that window would let the
  next acquire delete a live checkout (a test pins this).

## Sandbox

Running a repo's checks executes untrusted code, so the runner, not model
instructions, enforces: environment cleared and rebuilt from `ENV_ALLOWLIST`
(`PATH`, `HOME`, `LANG`, `LC_ALL`, `TMPDIR`, `SHELL`, `USER`; the ambient env
only if the policy sets `allow_secrets`), then the check's own `env`;
each command in its own process group, killed as a **group** on timeout; output
captured tail-first under a hard byte cap; a total timeout over the policy.

## Repair

Repair is a **delegated child of the held root execution** via
`DelegationDispatcher::spawn_children` (wired by `set_delegation_dispatcher`)
with the diagnostics as context, from the **root task's agent** (source) to
`origin.engineer_agent_id` (target; engineer-as-source would self-delegate), who
keeps authority and cost; it never touches a provider adapter directly. Not via `run_coding_task`,
`derive_coding_continuation_context` or `start_execution_with_launch_options`
(a second root execution fails `task_execution_in_progress`).

**Starting a repair invalidates the candidate** (outbox entry retired with no
replacement; the gate refuses to verify mid-repair). `record_successor_candidate` advances the
candidate, returns the gate to `verification_pending` and enqueues work in one
journaled transaction, picked up on the child-terminal path by
`resume_verification_after_repair` (or the seam for a root-level re-run); both
re-arm the gate. A repair with no successor (engineer or dispatch failed) is
named only by its parked outcome; the reconciler's pass takes the `repairing`
early return and settles `exhausted` once the elapsed budget is spent.

### No-progress is three conditions

```
same NORMALIZED failure
AND no relevant change to the checked snapshot
AND no new diagnostic information
```

Normalisation strips timestamps, durations, hex addresses, pids and temp paths,
and collapses digit runs (so a repair that only moved a line has not
progressed).

## Outcomes

`verification_state` is a durable projection beside task status, not a new
terminal status.

| controller outcome | task status | `verification_state` |
| --- | --- | --- |
| green | `completed` | `verified` |
| red, budget remains | **stays running** | `repairing` |
| no checks configured | `completed` | `unverified` |
| repair exhausted | `failed` | `exhausted` + reason |
| runner/policy unavailable | retryable blocked | `unavailable` |
| cancelled (`cancel()` has no production caller) | `cancelled` | `cancelled` |
| partially applied proposal | never terminal success | `blocked_partial` |

- **Missing state reads as `unknown`, never `verified`** (legacy VibeDev runs
  are `unknown`). Automation needing
  working code checks `status == completed` **and**
  `verification_state == verified`.
- **`unavailable` is not `unverified`** ("could not run" vs "nothing to run").
- **Voice never says plain "completed successfully" for `unverified`**; it
  appends `voice_verification_unverified_v1.0.0`: *"Nothing checked that code,
  so I can't tell you whether it works."*

Events (`verification.queued`, `.started`, `.command_completed`,
`.checks_failed`, `.repair_started`, `.passed`, `.unverified`, `.exhausted`,
`.unavailable`, `.attempt_indeterminate`, `.cancelled`) are **replayable
projections of committed state, never the source of truth**; task hydration is
correct without them. Voice consumes only the five high-level transitions.

`CanonicalVerificationEventSink` → `RuntimeCanonicalEventSink` publishes them
under one event type, `verification.lifecycle` (payload the whole event, `kind`
the wire name; principal/workspace fixed at adapter construction, task and
execution per event, `ui_thread_id` empty); a serialisation failure drops the
projection, never the pass. `verification.queued` has no emitter; `.cancelled`
only via `cancel()`. `verification_state` (read via
`verification_state_for_task`) reaches the task API and voice
(`task_completed_message`), not task cards or webhooks.

## Budgets

Three independent dimensions; the first exceeded stops the gate. Defaults
`max_repair_rounds: 3`, `max_elapsed_secs: 7200`, `max_spend_usd: None`
(`VerificationRuntimeSettings` at boot, stamped per gate by
`open_verification_gate` via `budgets.resolve()`; `BudgetConfig` resolves
partial or malformed input to defaults).

- `may_start_pass` ignores repair rounds (the last round still owes
  verification) but checks elapsed and spend; `accept_result` refuses an
  indeterminate attempt, which re-runs under a new attempt.
- `charge_verification_repair_round` charges the engine-reported session cost on
  the newest `coding.stats` event at the repair child's terminal, *before* the
  successor registers (a gate at its ceiling settles next pass): best-effort (no
  stats, no charge; rounds cannot be dodged), saturating, no negatives.

## The driver

`spawn_verification_reconciler`, started once at boot beside startup recovery,
ticks every `verification.reconcile_interval_secs` (default 60; `0` disables it, leaving
startup recovery as the only scheduler), first tick one interval out,
`MissedTickBehavior::Delay`; a failed tick logs and continues, scope walks run
on the blocking pool, and it logs only when it dispatched something. Per scope
it offers outbox entries ∪ parked unreleased outcomes − gates in flight here
(the parked half is load-bearing: a `repairing` gate is named only there). It
**lists, never reclaims** (a claim is a live release), does **no journal
replay** and claims nothing exclusively across processes; lease and generation
fence decide who lands a result.

| behaviour | how it is delivered |
| --- | --- |
| `RetryLater` / `Unavailable` are retryable | the gate keeps its outbox entry and the next tick offers another pass |
| the elapsed budget bounds a gate | every offered pass evaluates it first, even for a gate running nothing |
| a stalled repair settles as `exhausted` | via the parked-outcome half and the `repairing` early return |
| a settled-but-unreleased gate completes its task | re-driven from that same half; replay guards make re-entry idempotent |
| a lease is renewed rather than relying on a generous TTL | `spawn_lease_renewal`, alongside the pass that holds it |

## Durability

- Gate and outbox entry are **one** journal transaction (one atomic rename,
  then projected); either alone is unrecoverable.
- A gate that cannot be opened is **not** a hold: completion proceeds with
  `verification_state = unknown`.
- Finalisation is compare-and-set on gate status, candidate identity and a
  green attestation; a monotonic `generation` fences late expired workers.
- Fails closed: an unavailable evidence store, unreadable policy, crashed attempt or
  mutated snapshot never lands green.

`VerificationStore` blocks (advisory `flock` + journal replay), so controller
calls hop through `on_store` (`spawn_blocking`); startup recovery runs
`recover`, reap and `list_outbox` in one hop; `register_repaired_candidate` and
`existing_gate_decision` are `async` only for this. Deliberately inline:
`verification_state_for_task` (task-read hot path, one or two indexed reads, no
lock or replay), `existing_gate_decision`'s same two reads (only its
`register_repaired_candidate` branch hops), and `cancel` (no production caller;
must move to `on_store` once it has one).

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
  process-local kill switch); tests pin a mode per service instance with
  `set_verification_activation`.
- Every field defaults identically whether its key or the whole block is
  absent, so env-only enabling still gets a driver (a derived `Default` would
  make `reconcile_interval_secs` `0`).
- An unrecognised *value* degrades to **disabled**; an unrecognised *key* is a
  hard parse error, and `MagicianConfig` denies unknown fields, so a binary that
  does not know `verification:` refuses to start: deploy config and binary
  together.
- Disabled adds no I/O to the terminal path (activation is checked before any
  filesystem read).

### Observe is the qualification mode

`observe` opens the gate, materialises, runs the resolved policy and seals the
attestation like `enforce` (a green seals the same attestation), but **holds
nothing** (the task completes on its own path), **parks nothing** (a parked
outcome with no holder would be re-offered forever) and **releases nothing**
(that would replay a terminal transaction). A red result settles `exhausted`
("checks failed under observe; repair is not dispatched"); `settle_exhausted`
records the attestation the gate names, as enforce's last-round settlement does,
and settlements with no attempt keep the previous reference.

## Before enabling

Enforce still requires:

- **An owner baseline.** `verification.baseline` ships commented out and an
  empty block reads as absent, so [Anti-weakening](#anti-weakening) has no teeth
  until checks are written down.
- `verification_state` on task cards and webhooks (task API and voice carry it).
- Both coding engines activating together; neither may claim
  verification-gated completion alone. Under enforce, auto repair may switch
  engines after a failed candidate; a named pin stays same-engine.

Operational runbook: §8 of the
closure plan.

## Source

`magician/src/magician_v2/execution/verification/`: `gate`, `attestation`,
`journal`, `store`, `policy`, `snapshot`, `runner`, `gating`, `repair`,
`controller`, `events`, `budgets`, `ids`. Seam, driver and reconciler:
`magician_v2/artifact_v2/service.rs`; event adapter:
`magician_v2/artifact_v2/events.rs`; `VerificationConfig` in `config.rs`, wired
from `magician-bin/src/main.rs`. Tests: seam tests in `artifact_v2/service.rs`
(hold → verify → release, hold → red → still held, failed run) run real
subprocess checks against `.magician/verification.json` fixtures, and red-path
assertions require `repairing`; the module suite covers observe/enforce sealing,
budgets, lease renewal and config parsing.

Design: `docs/archive/plans/2026-08-07-vibedev-verification-controller.md`
