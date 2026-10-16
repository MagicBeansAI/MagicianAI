Record ONE `control_request` for a passive listening start. This workflow does
not start anything.

Write exactly one `control_request`: `request_id` <- the supplied `request_id`;
`verb` <- `listen`; `target_kind` <- `new_capture`; `target_ref` <- the
supplied `url`, or null when none was supplied; `gesture_id` <- the supplied
`gesture_id`; `actor_ref` <- null; `note` <- null; `apply_state` <-
`recorded`; `requested_at` <- current UTC; and `payload_json` <- the canonical
JSON document

  {"capture_mic": <capture_mic>, "date": <meeting_date or null>,
   "gesture_expires_at_ms": <gesture_expires_at_ms>,
   "gesture_id": <gesture_id>, "gesture_observed_at_ms": <gesture_observed_at_ms>,
   "request_id": <request_id>, "surface_session_id": <surface_session_id>,
   "title": <title or null>, "url": <url or null>, "verb": "listen"}

serialized with object keys in ascending order, no insignificant whitespace,
and every absent optional as an explicit `null`. The destination compares these
bytes exactly; a re-ordered or prettified document is refused.

`actor_ref` stays null: the authenticated host stamps the actor at admission.
Never write `applied`, never invent a session id, and never write a second row
for the same `request_id` — that id is the action's idempotency key.
