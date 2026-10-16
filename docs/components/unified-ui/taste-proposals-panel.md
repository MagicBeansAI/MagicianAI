# `/memory` — Taste proposals panel

The review surface for cross-session taste capture. Files:
`ui/unified-ui/src/lib/taste/TasteProposalsPanel.svelte` and its fetch module
`tasteProposals.ts`. Mounted on the `/memory` overview tab, above synthesis.

**An empty panel is normal.** Capture is off by default in code
(`memory.taste_profile.capture_enabled: false`; the repo seed config turns it
on) and stays inert until a distiller model is bound under
`llm.router.operation_mapping`. Without both, the panel says "Capture is off",
not an error.

## What it shows

Each pending proposal shows the directive, its destination section, its
confidence, and (expanded by default) the **verbatim evidence quotes** it was
inferred from. The evidence keeps approval from becoming rubber-stamping; the
backend refuses unevidenced candidates for the same reason.

## It polls, and that is not optional

Proposals arrive on the capture worker's sweep, not on user action, so a panel
that loaded once would show "nothing waiting" while proposals sat unread. It
subscribes to a module-level `createSharedPoll`: several mounts share one timer,
and a down backend gets backoff.

**The first read is direct, not from the poll.** `createSharedPoll` swallows
failures into its backoff and only pushes successes, so the mount does its own
fetch to make the first failure visible. Idle cadence is 60s (the producer
sweeps every 15 minutes). Every decision calls `pollNow()` to reconcile with the
server, since another surface may have decided too.

## Behaviour worth knowing

- **Approve** adds the directive to the profile note, so it joins every future
  prompt; the button reads "Add to profile".
- **Reject** is permanent for that directive (the store is content-addressed,
  so the same text is never proposed again); the button reads "Never suggest
  this".
- A decision reports whether the directive was **newly placed** or **already
  present** (after a crash between the note write and the status write); the
  panel then says "already in X — nothing changed".
- In-flight ids are tracked so a double-click cannot double-submit.
- A 404 means the item was decided elsewhere; the panel re-reads instead of
  showing an error.

Long directives use `min-width: 0` + `overflow-wrap: anywhere`, so an unbroken
string cannot collapse the column.

Backend contract, including why the proposals note is never parsed and why the
profile write precedes the status write: [Taste Profile](../magician/taste-profile.md).
