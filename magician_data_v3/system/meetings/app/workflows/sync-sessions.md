Sync the live capture state into this package.

Call `meetings_data.active_session` exactly once with no arguments. It takes
none: the read is registry-wide by construction because capture that is
running must never be invisible to the operator.

Project one `capture_session` per returned session using this exact mapping:
`session_id` <- `session_id`; `mode` <- `mode`; `status` <- `status`;
`live` <- `live`; `in_scope` <- `in_scope`; `paused` <- `paused`;
`capture_mic` <- `capture_mic`; `thread_id` <- `thread_id`; `title` <- `title`;
`url` <- `url`;
`started_seconds_ago` <- `started_seconds_ago`; `ended_seconds_ago` <-
`ended_seconds_ago`; `retained_for_seconds` <- `retained_for_seconds`; and
`synced_at` <- current UTC. Keep at most `limit` rows when one was supplied,
clamped to 20, preserving the returned order.

Preserve `live`, `in_scope` and `status` exactly as returned. Never infer
liveness from a status string of your own, never fill a missing
`ended_seconds_ago` with zero, and never invent a summary: this read carries no
meeting content.

`in_scope: false` means the session belongs to another scope. Its `thread_id`,
`title` and `url` are absent by design — record them as null rather than
substituting a placeholder, and never guess which meeting it is.
