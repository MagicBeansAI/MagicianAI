# Purge receipts and the known-event list

Two client-side contracts that keep a failure from being reported as a different
one.

## A purge that did not complete says so

An app purge receipt carries a per-target `status` and an overall `completion`.
Both vocabularies include the unsettled cases:

- `status`: `deleted`, `cryptographically_erased`, `retained_shared`,
  `retained_by_policy`, `provider_retention_unknown`, `failed`
- `completion`: `fully_erased`, `completed_with_disclosed_retention`,
  `incomplete`

Without the unsettled values a failed purge would not parse, and the owner would
see "invalid" instead of learning that part of the purge did not happen.

The failure check runs **before** the retention assertions. Evaluated later, a
failed target would read as "retained an installation-owned authority surface",
a deliberate retention *decision* rather than work that did not happen. The
message names the failed targets and says the installation was not fully
removed.

`failure_ref` is the only field the server attaches to a failed target.

## The known-event list comes from the taxonomy

`v2-websocket.ts` sources its known event types from `KNOWN_EVENT_TYPES` in
`$lib/realtime/event-taxonomy` rather than a hand-maintained list beside it.

So every taxonomy row (the Activity family, `HitlRequested`, `HitlResolved`,
`ProgressEvent`, the `ThinkingMode*` events, and any new row) is known, and none
warns as a false protocol mismatch.

Event types with **no taxonomy row** (client-side events and V3 planning events)
are listed by hand below the spread; that list is the only part a new event might
need.
