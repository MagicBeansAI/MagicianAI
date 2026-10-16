Record ONE `control_request` for a stop of a live capture session. This
workflow does not stop anything.

Write exactly one `control_request`: `request_id` <- the supplied
`request_id`; `verb` <- `stop`; `target_kind` <- `live_session`;
`target_ref` <- the supplied `session_id`; `gesture_id` <- null;
`actor_ref` <- null; `note` <- null; `apply_state` <- `recorded`;
`requested_at` <- current UTC; and `payload_json` <- the canonical JSON
document

  {"request_id": <request_id>, "session_id": <session_id>, "verb": "stop"}

serialized with object keys in ascending order and no insignificant whitespace.

`gesture_id` stays null. A stop carries no surface gesture: accepting one
would make a risk-reducing act read, in the owner's signed display, like an
intent-bound start, and the destination refuses it. Never write `applied` and
never write a second row for the same `request_id`.
