Record ONE `control_request` for an agent-attendee join. This workflow does not
join anything.

Write exactly one `control_request`: `request_id` <- the supplied `request_id`;
`verb` <- `join`; `target_kind` <- `new_capture`; `target_ref` <- the supplied
`url`; `gesture_id` <- the supplied `gesture_id`; `actor_ref` <- null; `note`
<- null; `apply_state` <- `recorded`; `requested_at` <- current UTC; and
`payload_json` <- the canonical JSON document

  {"capture_mic": false, "date": <meeting_date or null>,
   "gesture_expires_at_ms": <gesture_expires_at_ms>,
   "gesture_id": <gesture_id>, "gesture_observed_at_ms": <gesture_observed_at_ms>,
   "request_id": <request_id>, "surface_session_id": <surface_session_id>,
   "title": <title or null>, "url": <url>, "verb": "join"}

serialized with object keys in ascending order, no insignificant whitespace,
and every absent optional as an explicit `null`.

`capture_mic` is always false here: only the passive listener owns a microphone
tap, and the destination refuses a join that claims one. A join without a `url`
is not a valid request — refuse rather than defaulting one.
