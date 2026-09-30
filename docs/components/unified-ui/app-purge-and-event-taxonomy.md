# Purge receipts and the known-event list

Two client-side contracts where an omission was read as a different failure than
the one that happened.

## A purge that did not complete says so

An app purge receipt carries a per-target `status` and an overall `completion`.
Both vocabularies include the unsettled cases:

- `status`: `deleted`, `cryptographically_erased`, `retained_shared`,
  `retained_by_policy`, `provider_retention_unknown`, `failed`
- `completion`: `fully_erased`, `completed_with_disclosed_retention`,
  `incomplete`

Omitting the unsettled values did not make a failed purge impossible — it made
one **unreadable**. The receipt failed to parse, and the owner was told the
outcome was "invalid" rather than that part of the purge did not happen.

The failure check runs **before** the retention assertions, and the ordering is
the point. A failed terminal target evaluated later would be reported as
"retained an installation-owned authority surface" — which describes a
deliberate retention *decision* rather than work that did not happen. The
message names the failed targets and states plainly that the installation was
not fully removed.

`failure_ref` is the only field the server attaches to a failed target.

## The known-event list comes from the taxonomy

`v2-websocket.ts` sources its known event types from `KNOWN_EVENT_TYPES` in
`$lib/realtime/event-taxonomy` rather than a hand-maintained list beside it.

The two had drifted. The whole Activity family, plus `HitlRequested`,
`HitlResolved`, `ProgressEvent` and the `ThinkingMode*` events, were declared in
the taxonomy, routed, and rendered — and still warned as a protocol mismatch on
every message. Sourcing the list from the taxonomy means a new row can never
reappear as a false mismatch.

Event types with **no taxonomy row** — client-side events and V3 planning events
— are still listed by hand below the spread, and that list is the only part a
new event might need.
