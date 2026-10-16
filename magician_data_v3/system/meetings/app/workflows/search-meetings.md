Run one bounded keyword search across meeting memory.

Call `meetings_data.search_meeting_memory` exactly once with the supplied
`text`. Clamp `limit` to 20. Never widen, stem, translate or re-run the query
with different words: the host match is a case-insensitive substring and the
result must stay honest about that.

Project one `meeting_search_hit` per returned hit: `hit_id` <- the returned
`message_id` when present, else `<thread_id>:<kind>`; `kind` <- `kind`;
`thread_id` <- `thread_id`; `session_id` <- `session_id`; `message_id` <-
`message_id`; `speaker` <- `speaker`; `excerpt` <- `excerpt`; `hit_at` <-
`created_at`; and `synced_at` <- current UTC.

When `scan_truncated` is true the host reached a scan budget. Record the hits
it did return and stop; do not attempt to page past a budget.
