# Obligations — what is owed, and by when

Composable work modules, **Module D**. Module:
`magician/src/magician_v2/obligations/`. Plan (archived):
`docs/archive/plans/2026-08-07-opc-composable-work-modules.md` §6.

Why it exists: program running state tracks **stage**, not **obligation**. If a
counterparty says "send me your metrics by Friday", something must enforce
Friday.

## Generic

An obligation is a promise with a deadline and a direction — the same shape for
a deck owed to an investor, a reply owed to a customer, or a document a supplier
owes us. Nothing about it is fundraising-specific.

Contract tuple: `(audience, what, due_at, owed_by_us | owed_to_us)`. An
engagement is one kind of [audience](audience.md); promises are made to clients,
cohorts, panels and individuals as readily as to counterparties.

The audience's **kind is part of the register's identity**, so one company as a
live deal and as a standing client keep separate books. `lapsed_across` mixes
kinds freely.

## The two things it does that a to-do list does not

### Direction

A lapse means **we** broke a promise or **they** have not replied — different
words and urgency. `lapsed()` and `lapsed_across()` take an optional direction
so the split is available at the read.

### Lapsing is derived, never stored

Nothing writes `lapsed`; it falls out of the clock, so the register does not
depend on a sweep having run. `state()` is inclusive of the deadline: due at
Friday means lapsed at Friday.

## Properties

- **The same promise noticed twice is one obligation.** The id derives from the
  contract tuple, with `what` normalised for whitespace and case (re-extracted
  commitments rarely come back character-identical).
- **A different deadline or direction is a different promise.** Re-promising is
  a new obligation, so the register can show that a date slipped.
- **`Met` and `Released` are distinct** — "we did it" vs "we no longer have to";
  conflating them inflates the hit rate.
- **The first settlement is the settlement.** Later calls are ignored, so the
  register stays evidence of what happened when.
- **Settled obligations stay on the register**, or it would show only failures.
- **Outstanding is ordered by deadline** — "what is closest to being late" puts
  lapsed items first.

## Surfacing across the book

A cycle asks about the whole book. `lapsed_across` takes the audience list and
returns one ordered, deduplicated list, tolerating unknown audiences so a stale
list still surfaces what it can. The audience list is **supplied, not
discovered**: rosters belong to each relationship's owner (same decoupling as
the envelope gate).

## What fills the register

`magician/src/magician_v2/obligation_sweeps/` runs two derivations against real
activity; `obligation_sweeps::worker` is the cadence. Both use one instant so a
row cannot ripen between them:

- **Scheduling silence** — `scheduling::sweep_silence` over every negotiation in
  scope. An unanswered offer ripens into a chase (`OwedToUs`); an ask since
  answered or closed settles the chase, `Met` when they replied, `Released` when
  closed unanswered.
- **Data-room follow-ups** — `data_room::sweep_follow_ups` over every room. A
  shared link nobody opened becomes a delivery question; a room read then quiet
  becomes a follow-up. Both `OwedByUs`.

Writes go through `scheduling::apply_silence_sweep` and
`data_room::apply_follow_up_sweep`, which **record everything, then settle
everything**: a crash between the halves must leave a live obligation, never a
settled-but-unrecorded one (a vanished chase is unrecoverable; a duplicate is
not).

### Why it does not live inside this module

The register is the most primitive work module. `obligation_sweeps` imports the
register, data room and scheduling; nothing imports it. A new emitter (transcript
promise, support SLA, procurement deadline) is a new caller of
`ObligationStore::record`, never a change here. Rooms and negotiations carry an
`AudienceRef` passed straight through, so every relationship kind uses the same
code path.

### Reachable from

- `POST /api/magician/v2/work/obligations/sweep` — the on-demand twin. It passes
  an **unseeded** memory: a one-shot caller has no previous view, and pretending
  otherwise can release a whole register in one call.
- `obligation_sweeps::worker::ObligationSweepWorker::spawn` — the cadence.
  Scopes come from the workspace's `scopes/` directory, so no tenant is
  invisible to the sweep.

### What the sweep cannot see, stated plainly

- **`replied` is never guessed.** A room cannot see mail, calls or meetings, so
  every token enters unanswered. At worst this raises a follow-up an owner
  settles in one act; the other default would suppress every follow-up.
- **The previous view is in memory, not on disk.** The data room's settle half
  derives from *absence*, so after a restart a superseded follow-up stays live
  until an owner settles it — fail-closed, and a real cost.
- **Windows are stated, never defaulted to zero.** A window at or below zero is
  refused: at zero every offer is silent the instant it is made, and append-only
  rows cannot be un-written.

## Reading the register

`GET /api/magician/v2/work/obligations` answers *what is owed, what lapsed*
across every relationship in the scope, directions counted apart.
`ObligationStore::all_obligations` folds the directory (registers are named for
a hash of the audience key), so no roster is needed and a promise against a
relationship missing from a list is not invisible.

An unreadable register is an **error**, never empty — "nothing is owed" from a
disk fault is the most reassuring wrong answer possible.

### The reading beside the row

A row's `what` text is part of the identity tuple, so it cannot carry a visit
count (that would mint a fresh row per visit). `data_room::cycle` composes the
distinction the text cannot ("opened the deck three times, never opened the
financials"); `obligation_sweeps::attention` puts it on this route:

- `attention_notes_for_scope` composes notes for every room, walking the same
  rooms and `snapshot_room` assembly as `sweep_scope`, so a note cannot come
  from a view the sweep never saw.
- `explain_register` joins them onto the loaded rows by `obligation_id`, against
  the **unfiltered** rows — a display filter must not turn into an alarm.
- The response carries `reading` on rows that have one, and a `readings` block
  with counts (never rates):

| Bucket | What it means |
| --- | --- |
| `attached` (`reading` on the row) | the register holds this handle |
| `row_not_in_register` | the reading ripened and the register has no row: the sweep has not run since, or it ran under different windows |
| `not_yet_raised` | nothing is owed and nothing is wrong: they replied, or the waiting window has not closed |

A handle the register does not hold is never attached to the nearest row nor
dropped; an **empty** register attaches nothing. A follow-up's `due_at`
(`shared_at + delivery_question_after` or `last_seen + follow_up_after`) is part
of the derived id, so `WorkModulesApi` takes the sweep worker's own
`ObligationSweepConfig` and the route states the windows it read under.

A reading is **derived on demand and never recorded**. If rooms cannot be read,
`readings.available` is `false` with the reason; the register is still answered
(zeroed counts would falsely say nobody is waiting).

## Not built here

- **Escalation.** This surfaces lapses; what to do belongs to the agent's cycle,
  not a notification policy inside a register.
- **Extraction.** Nothing turns "send me your metrics by Friday" into a record.
  `evidence::transcript_ingestion` produces **commitments**, not obligations;
  its `TranscriptClaim` flow is the model to follow: extract, surface, and let a
  person confirm before it counts.
- **`CorrectionObligation`** in the outward assertions store is a different
  shape (owed because a *claim* changed) and is deliberately not unified.
