# `/memory` — Taste proposals panel

The review surface for cross-session taste capture (Slice 2). Files:
`ui/unified-ui/src/lib/taste/TasteProposalsPanel.svelte` and its fetch module
`tasteProposals.ts`. Mounted on the `/memory` overview tab, above synthesis.

**It is empty by default, and that is correct.** Capture ships disabled
(`memory.taste_profile.capture_enabled: false`) and is additionally inert
until a distiller model is bound under `llm.router.operation_mapping`. A
deployment that has done neither sees "Capture is off" rather than an error —
that is the honest state, not a fault.

## What it shows, and why

Each pending proposal renders the directive, its destination section, its
confidence, and — expanded by default — the **verbatim evidence quotes** the
directive was inferred from.

The evidence is the reason the panel exists. A surface that showed only the
directive would turn approval into rubber-stamping, which is precisely the
failure the daily proposal cap exists to prevent. If a proposal cannot be
checked against something the owner actually said, it should not be approvable
at a glance; the backend refuses unevidenced candidates for the same reason.

## It polls, and that is not optional

Proposals arrive on the capture worker's sweep, not on user action. A panel
that loaded once on mount would show "nothing waiting" while proposals sat
unread until the owner happened to reload — the feature failing silently on
the one surface meant to reveal it.

It subscribes to a module-level `createSharedPoll`, so several mounts share
one timer and a backend that is down backs off instead of being hammered.

**The first read is direct, not from the poll.** `createSharedPoll` swallows
failures into its backoff and only ever pushes successes to subscribers, so a
panel that waited on the subscription alone would sit on "Loading…" forever
against a dead backend, saying nothing. The mount does its own fetch so the
first failure is visible; the poll handles everything after.
Idle cadence is 60s: the producer sweeps every 15 minutes, so polling faster
spends requests to learn nothing. Every decision calls `pollNow()` to
reconcile against the server rather than trusting the local row removal —
another surface may have decided something too.

## Behaviour worth knowing

- **Approve** adds the directive to the profile note, which means it joins
  every future prompt. The copy says so ("Add to profile") rather than a bare
  "Approve", because the consequence is not obvious from the verb.
- **Reject** is permanent for that directive — the store is content-addressed,
  so the same text is never proposed again. The button says "Never suggest
  this" for that reason.
- A decision reports whether the directive was **newly placed** or **already
  present**. The second case happens after a crash between the note write and
  the status write; the panel says "already in X — nothing changed" instead of
  implying it just did something.
- In-flight ids are tracked so a double-click cannot double-submit.
- A 404 means the queue moved underneath the view (decided elsewhere). The
  panel re-reads rather than showing a red banner, since the right response is
  a refresh, not an error.

Long directives use the `min-width: 0` + `overflow-wrap: anywhere` pair, so an
unbroken string cannot collapse the column to one character wide.

## Placement

Above the synthesis panel on the overview tab: proposals are the only thing on
`/memory` waiting on a decision, and a queue below the fold is a queue that
stops being read.

Backend contract, including why the proposals note is never parsed and why the
profile write precedes the status write, is in
[Taste Profile](../magician/taste-profile.md).
